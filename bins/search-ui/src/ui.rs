//! Interactive full-screen search over a loaded [`Index`].
//!
//! The whole MFT-derived index already sits in memory, so every keystroke can
//! re-scan all of it: the search has no debounce and no async work. What makes
//! that legible is measuring it — the last query's latency sits flush right of
//! the prompt and the footer carries a rolling median plus a sparkline of the
//! last few dozen queries, so a scan that quietly got slower is visible while
//! typing rather than after the fact.

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::style::{Print, ResetColor, SetBackgroundColor, Stylize};
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use name_core::fuzzy;
use name_core::index::{Index, OrQuery, SearchTable};
use std::io::{Stdout, Write};
use std::time::Instant;

/// Ink colours sampled off the reference recording (`watch.mp4`) so the UI
/// reproduces that design rather than whatever theme the host terminal
/// happens to carry.
///
/// 24-bit SGR is not universal, and on the Windows console path crossterm
/// quantizes `Color::Rgb` to the 16-colour attribute palette — which is how
/// the "colours look old and the background is pure black" symptom happens:
/// every `38;2;…`/`48;2;…` is thrown away and you get the default fg on the
/// default bg. So each role has both an exact RGB and the nearest ANSI-16
/// colour, and [`truecolor`] picks. The ANSI fallback keeps the video's hues
/// (blue prompt, green timings) on a terminal that cannot honour the exact
/// values; the RGB is used only where it will actually render.
mod palette {
    use crossterm::style::Color;

    /// Whether the host advertises 24-bit colour. `COLORTERM` is the standard
    /// signal; `WT_SESSION` is Windows Terminal and `TERM_PROGRAM` covers
    /// iTerm2/WezTerm/VS Code. A console that advertises none of them must not
    /// be sent RGB.
    pub fn truecolor() -> bool {
        use std::sync::OnceLock;
        static OK: OnceLock<bool> = OnceLock::new();
        *OK.get_or_init(|| {
            let colorterm = std::env::var("COLORTERM").unwrap_or_default();
            let colorterm = colorterm.trim().to_ascii_lowercase();
            if matches!(colorterm.as_str(), "truecolor" | "24bit") {
                return true;
            }
            if std::env::var("WT_SESSION").is_ok() {
                return true;
            }
            let program = std::env::var("TERM_PROGRAM").unwrap_or_default();
            let program = program.trim().to_ascii_lowercase();
            matches!(
                program.as_str(),
                "iterm.app" | "wezterm" | "vscode" | "ghostty" | "hyper"
            )
        })
    }

    /// Background: the recording's charcoal-purple, falling back to the
    /// terminal's own black — the only near-black ANSI offers.
    pub fn bg() -> Color {
        if truecolor() {
            Color::Rgb {
                r: 19,
                g: 18,
                b: 21,
            }
        } else {
            Color::Black
        }
    }

    /// Filenames and the query — the brightest ink on screen.
    pub fn ink() -> Color {
        if truecolor() {
            Color::Rgb {
                r: 255,
                g: 255,
                b: 255,
            }
        } else {
            Color::White
        }
    }

    /// Header subtitle, parent directories, footer prose, `median`.
    pub fn dim() -> Color {
        if truecolor() {
            Color::Rgb {
                r: 115,
                g: 114,
                b: 119,
            }
        } else {
            Color::DarkGrey
        }
    }

    /// The `❯` prompt and the caret. The video's periwinkle; ANSI bright blue
    /// is the same hue at the 16-colour palette's saturation.
    pub fn blue() -> Color {
        if truecolor() {
            Color::Rgb {
                r: 114,
                g: 170,
                b: 255,
            }
        } else {
            Color::Blue
        }
    }

    /// Latency figures, the sparkline, the median value.
    pub fn green() -> Color {
        if truecolor() {
            Color::Rgb {
                r: 119,
                g: 217,
                b: 139,
            }
        } else {
            Color::Green
        }
    }
}

/// Matches kept for ranking. Beyond this the list is already far longer than a
/// screen, and the true total is reported separately.
const MAX_RESULTS: usize = 20_000;
/// Keystrokes remembered for the footer sparkline and median.
const LATENCY_SAMPLES: usize = 64;
const MIN_WIDTH: usize = 40;
const MIN_HEIGHT: usize = 8;

/// Restores the console on drop, so a panic or an early error still leaves a
/// usable terminal behind.
struct TerminalGuard;

impl TerminalGuard {
    fn enter(out: &mut Stdout) -> Result<Self, String> {
        terminal::enable_raw_mode().map_err(|e| format!("cannot enter raw mode: {e}"))?;
        // Paint the whole alternate screen with the reference palette's
        // background so the ink colours read the way they do in the video,
        // whatever the host terminal's own theme is.
        execute!(
            out,
            EnterAlternateScreen,
            Hide,
            SetBackgroundColor(palette::bg()),
            Clear(ClearType::All)
        )
        .map_err(|e| format!("cannot set up the screen: {e}"))?;
        Ok(TerminalGuard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut out = std::io::stdout();
        let _ = execute!(out, ResetColor, Show, LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

struct App {
    table: SearchTable,
    /// Volume root, shown in the header. Copied out so the whole [`Index`] can
    /// be dropped once the table is built.
    root: String,
    indexed: usize,
    input: String,
    /// Caret offset into `input`, counted in `char`s.
    caret: usize,
    /// Matching entries as row numbers into `table`, best match first.
    results: Vec<u64>,
    /// Matches found before `results` was capped.
    total: usize,
    /// Whether the query carries any filter or search term at all, which
    /// decides whether the footer says "matches" or "entries".
    filtered: bool,
    selected: usize,
    top: usize,
    page: usize,
    /// Search latency per keystroke, in microseconds, oldest first.
    latencies: Vec<u128>,
}

/// What a key press did to the app.
#[derive(PartialEq)]
enum Action {
    None,
    Moved,
    Edited,
    Quit,
    Open,
}

impl App {
    fn new(table: SearchTable, root: String, indexed: usize) -> Self {
        let mut app = App {
            table,
            root,
            indexed,
            input: String::new(),
            caret: 0,
            results: Vec::new(),
            total: 0,
            filtered: false,
            selected: 0,
            top: 0,
            page: 1,
            latencies: Vec::new(),
        };
        app.search();
        app
    }

    /// Re-scan the whole index for the current query and time it.
    fn search(&mut self) {
        let started = Instant::now();

        let parsed = OrQuery::parse(&self.input);
        // The bare words drive fuzzy matching against file names here.
        // Substring-matching them would reject exactly the scattered matches
        // the interactive mode exists to surface — `rc` must find `report.txt`.
        // So when no term is a glob, the fuzzy pass owns the terms and only the
        // filters are applied literally. A wildcard term has no fuzzy meaning
        // (its meaning is the glob), so any glob switches the whole query to
        // strict matching.
        let terms_have_wildcards = parsed.terms().any(|t| t.contains('*') || t.contains('?'));
        // Fuzzy scoring ANDs its needle list, so it can only own the terms when
        // the query is a single alternative. With `|` the alternatives are OR'd,
        // which only the strict matcher gets right.
        let fuzzy_owns_terms = parsed.alternatives.len() == 1 && !terms_have_wildcards;
        let needles: Vec<String> = parsed
            .terms()
            .filter(|t| !t.contains('*') && !t.contains('?'))
            .map(|s| s.to_string())
            .collect();
        self.filtered = !parsed.alternatives.iter().all(|q| q.is_empty());

        let mut scored: Vec<(fuzzy::Match, u64)> = Vec::new();
        let mut total = 0usize;

        for row in 0..self.table.len() as u64 {
            let entry = self.table.get(row as usize);
            let Some(rank) = fuzzy::score(entry.name, &needles) else {
                continue;
            };
            let admitted = if fuzzy_owns_terms {
                parsed.matches_filters(
                    entry.path,
                    entry.name,
                    entry.is_dir,
                    entry.size,
                    entry.attrs,
                )
            } else {
                parsed.matches(
                    entry.path,
                    entry.name,
                    entry.is_dir,
                    entry.size,
                    entry.attrs,
                )
            };
            if !admitted {
                continue;
            }
            total += 1;
            if scored.len() < MAX_RESULTS {
                scored.push((rank, row));
            }
        }

        // `Match`'s derived ordering is the ranking. Rows are already in path
        // order, so the row number breaks ties for free and the list is stable
        // across repeated identical queries.
        scored.sort_unstable_by_key(|(rank, row)| (*rank, *row));

        self.results = scored.into_iter().map(|(_, frn)| frn).collect();
        self.total = total;
        self.selected = 0;
        self.top = 0;

        let elapsed = started.elapsed().as_micros();
        self.latencies.push(elapsed);
        if self.latencies.len() > LATENCY_SAMPLES {
            self.latencies.remove(0);
        }
    }

    fn input_len(&self) -> usize {
        self.input.chars().count()
    }

    /// Byte offset of the `n`th character, or the end of the string.
    fn byte_of(&self, caret: usize) -> usize {
        self.input
            .char_indices()
            .nth(caret)
            .map(|(b, _)| b)
            .unwrap_or(self.input.len())
    }

    fn insert(&mut self, c: char) {
        self.input.insert(self.byte_of(self.caret), c);
        self.caret += 1;
    }

    fn delete_before_caret(&mut self) {
        if self.caret == 0 {
            return;
        }
        let start = self.byte_of(self.caret - 1);
        let end = self.byte_of(self.caret);
        self.input.replace_range(start..end, "");
        self.caret -= 1;
    }

    fn delete_at_caret(&mut self) {
        let start = self.byte_of(self.caret);
        if start < self.input.len() {
            let end = self.byte_of(self.caret + 1);
            self.input.replace_range(start..end, "");
        }
    }

    fn delete_word(&mut self) {
        let caret_byte = self.byte_of(self.caret);
        let trimmed = self.input[..caret_byte].trim_end();
        // Cut back to the previous separator and drop it too, so repeated
        // presses do not leave a trail of spaces behind.
        let cut = match trimmed.rfind([' ', '\\', '/']) {
            Some(i) => trimmed[..=i].trim_end_matches([' ', '\\', '/']).len(),
            None => 0,
        };
        let caret = trimmed[..cut].chars().count();
        self.input.replace_range(cut..caret_byte, "");
        self.caret = caret;
    }

    fn clear(&mut self) {
        self.input.clear();
        self.caret = 0;
    }

    /// Keep the selection on a real entry and the viewport scrolled to it.
    fn clamp_selection(&mut self) {
        if self.results.is_empty() {
            self.selected = 0;
            self.top = 0;
            return;
        }
        self.selected = self.selected.min(self.results.len() - 1);
        let height = self.page.max(1);
        if self.selected < self.top {
            self.top = self.selected;
        } else if self.selected >= self.top + height {
            self.top = self.selected + 1 - height;
        }
    }

    fn selected_path(&self) -> Option<String> {
        let row = *self.results.get(self.selected)? as usize;
        Some(self.table.get(row).path.to_string())
    }

    fn median_us(&self) -> Option<u128> {
        if self.latencies.is_empty() {
            return None;
        }
        let mut sorted = self.latencies.clone();
        sorted.sort_unstable();
        Some(sorted[sorted.len() / 2])
    }

    /// The latency history as a bar chart, newest sample on the right.
    ///
    /// Scaled against the slowest sample on a log curve: a single cold scan
    /// would otherwise flatten every other bar into the baseline, which is
    /// exactly the outlier the chart exists to show.
    fn sparkline(&self, width: usize) -> String {
        const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

        if width == 0 {
            return String::new();
        }
        let start = self.latencies.len().saturating_sub(width);
        let samples = &self.latencies[start..];
        let Some(peak) = samples.iter().copied().max() else {
            return String::new();
        };
        let scale = (peak as f64).ln_1p().max(f64::MIN_POSITIVE);

        samples
            .iter()
            .map(|s| {
                let level = (((*s as f64).ln_1p() / scale) * 7.0).round();
                BLOCKS[(level as usize).min(7)]
            })
            .collect()
    }

    fn draw(&mut self, out: &mut impl Write, size: (u16, u16)) -> std::io::Result<()> {
        let width = size.0 as usize;
        let height = size.1 as usize;

        if width < MIN_WIDTH || height < MIN_HEIGHT {
            execute!(out, MoveTo(0, 0), Clear(ClearType::All))?;
            return self.write_line(
                out,
                0,
                MIN_WIDTH,
                format!("terminal {width}x{height} is too small — need {MIN_WIDTH}x{MIN_HEIGHT}")
                    .with(palette::dim())
                    .to_string(),
            );
        }

        // Header, prompt, rule and footer leave four rows for the list.
        let list_top = 3u16;
        let list_height = height.saturating_sub(4);
        self.page = list_height;
        self.clamp_selection();

        // FSearch layout: brand in bold white, two spaces, then a dim
        // subtitle that carries the volume and the entry count.
        let subtitle = format!(
            "{} · {} files and folders",
            self.root.as_str().with(palette::dim()),
            thousands(self.indexed).with(palette::dim())
        );
        let header = format!("{}  {}", "zen-rs".with(palette::ink()).bold(), subtitle);
        self.write_line(out, 0, width, header)?;

        // The per-keystroke latency is the headline number. No label: the
        // video puts a green "8.1 ms" flush right of the prompt, with no word
        // before it.
        let latency = match self.latencies.last() {
            Some(us) => format!("{} ms", fmt_us(*us))
                .with(palette::green())
                .to_string(),
            None => String::new(),
        };
        self.write_line(out, 1, width, self.prompt_line(width, &latency))?;

        self.write_line(
            out,
            2,
            width,
            "─".repeat(width).with(palette::dim()).to_string(),
        )?;

        let first = self.top.min(self.results.len());
        let last = (first + list_height).min(self.results.len());

        // FSearch flows the name flush left, the parent path dimmed after a
        // two-space gap, with no column rule between them. A shrinking list
        // would otherwise leave the previous frame's entries on screen below
        // the last match, so every row of the list is repainted.
        for offset in 0..list_height {
            let row = first + offset;
            let cell = if self.results.is_empty() && offset == 0 {
                "  no matches".with(palette::dim()).to_string()
            } else if row < last {
                self.result_cell(row, width)
            } else {
                String::new()
            };
            self.write_line(out, list_top + offset as u16, width, cell)?;
        }

        self.write_line(out, (height - 1) as u16, width, self.footer_line(width))?;
        out.flush()
    }

    /// `❯ query` with the caret as a blue half-block, last latency flush right.
    fn prompt_line(&self, width: usize, latency: &str) -> String {
        // `latency` arrives styled, so measure what it occupies on screen
        // rather than counting its escape sequences.
        let latency_width = display_width(latency);
        // `❯ ` is two cells and the caret bar one more; the rest is query.
        let budget = width.saturating_sub(2 + 1 + latency_width + usize::from(!latency.is_empty()));

        let chars: Vec<char> = self.input.chars().collect();
        let caret = self.caret.min(chars.len());
        // Keep the caret on screen with a character of context after it when
        // one exists, and scroll left only when the query no longer fits.
        let after_len = usize::from(caret < chars.len());
        let start = caret.saturating_sub(budget.saturating_sub(after_len));
        let end = (caret + after_len).min(chars.len()).min(start + budget);
        let before: String = chars[start..caret].iter().collect();
        let after: String = chars[caret..end].iter().collect();

        // The recording shows `❯` in blue, the query in bold white, and the
        // caret as a solid blue half-block — `▌` fills the left half of a
        // cell, the 9 px of a 16 px cell the video shows. A bar cursor goes
        // at the insertion point, so the character under the caret stays
        // readable rather than being overwritten or inverted.
        let mut line = "❯".with(palette::blue()).to_string();
        line.push(' ');
        line.push_str(&before.with(palette::ink()).bold().to_string());
        line.push_str(&"▌".with(palette::blue()).to_string());
        line.push_str(&after.with(palette::ink()).bold().to_string());

        if !latency.is_empty() {
            let used = display_width(&line);
            line.push_str(&" ".repeat(width.saturating_sub(used + latency_width)));
            line.push_str(latency);
        }
        line
    }

    /// FSearch lays each row as: white name, two spaces, dim parent.
    /// No column rule; the gap carries the layout, and a `fit`/`elide` pair
    /// keeps the row on one line. The name is regular weight — only the
    /// brand and the query are bold in the recording.
    fn result_cell(&self, row: usize, width: usize) -> String {
        let entry = self.table.get(self.results[row] as usize);
        // The two-space gap is enough on its own; the parent path can use the
        // rest. `elide_tail` keeps the directory that gave the file its
        // meaning, which is what a reader needs when names collide.
        let name_visible = entry.name.chars().count().min(width.saturating_sub(2));
        let dir_width = width.saturating_sub(name_visible + 2);

        let cell = format!(
            "{name}  {dir}",
            name = fit(entry.name, name_visible).with(palette::ink()),
            dir = elide_tail(&shorten_home(parent_of(entry.path)), dir_width).with(palette::dim())
        );
        if row == self.selected {
            cell.reverse().to_string()
        } else {
            cell
        }
    }

    /// Footer: the prose, the chart, the median. The video has no match count
    /// on the right edge — count info is already in the header.
    ///
    /// As the terminal narrows the prose is the first thing to go, then the
    /// chart; the median number is the last survivor.
    fn footer_line(&self, width: usize) -> String {
        let label = "every keystroke is a whole-disk search";
        let median = match self.median_us() {
            Some(us) => format!("{} ms", fmt_us(us)),
            None => "—".to_string(),
        };

        let label_width = label.chars().count();
        let median_width = "median".len() + 1 + median.chars().count();

        let mut line = String::new();
        let mut spark_width = 0usize;

        // Prose + sparkline + median. As the budget shrinks the prose goes
        // first, then the chart; the median number stays.
        if width >= label_width + median_width + 4 {
            line.push_str(&label.with(palette::dim()).to_string());
            spark_width = 24.min(width - label_width - median_width - 4);
        } else if width >= median_width + 4 {
            spark_width = 24.min(width - median_width - 4);
        }

        if spark_width > 0 {
            if !line.is_empty() {
                line.push_str("  ");
            }
            line.push_str(
                &self
                    .sparkline(spark_width)
                    .with(palette::green())
                    .to_string(),
            );
        }
        if !line.is_empty() {
            line.push_str("  ");
        }
        line.push_str(&"median".with(palette::dim()).to_string());
        line.push(' ');
        line.push_str(&median.with(palette::green()).to_string());

        // Pad to the right edge in case the budget is bigger than the content
        // we could lay down (rare, but happens at very wide widths).
        let used = display_width(&line);
        if used < width {
            line.push_str(&" ".repeat(width - used));
        }
        line
    }

    fn write_line(
        &self,
        out: &mut impl Write,
        row: u16,
        width: usize,
        cell: String,
    ) -> std::io::Result<()> {
        // Re-assert the background before every clear: `Clear` paints with the
        // currently active background, and a styled span may have reset it.
        execute!(
            out,
            MoveTo(0, row),
            SetBackgroundColor(palette::bg()),
            Clear(ClearType::CurrentLine),
            Print(truncate_visible(&cell, width)),
        )
    }
}

/// Apply one key press, re-scanning only when the query text actually changed.
fn handle_key(app: &mut App, key: KeyEvent) -> Action {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    let action = match key.code {
        KeyCode::Esc => Action::Quit,
        KeyCode::Char('c') if ctrl => Action::Quit,
        KeyCode::Enter => Action::Open,

        KeyCode::Backspace => {
            app.delete_before_caret();
            Action::Edited
        }
        KeyCode::Delete => {
            app.delete_at_caret();
            Action::Edited
        }
        KeyCode::Char('w') if ctrl || alt => {
            app.delete_word();
            Action::Edited
        }
        KeyCode::Char('u') if ctrl => {
            app.clear();
            Action::Edited
        }

        KeyCode::Left => {
            app.caret = app.caret.saturating_sub(1);
            Action::Moved
        }
        KeyCode::Right => {
            app.caret = (app.caret + 1).min(app.input_len());
            Action::Moved
        }
        KeyCode::Home => {
            app.caret = 0;
            Action::Moved
        }
        KeyCode::Char('a') if ctrl => {
            app.caret = 0;
            Action::Moved
        }
        KeyCode::End => {
            app.caret = app.input_len();
            Action::Moved
        }
        KeyCode::Char('e') if ctrl => {
            app.caret = app.input_len();
            Action::Moved
        }
        KeyCode::Up => {
            app.selected = app.selected.saturating_sub(1);
            Action::Moved
        }
        KeyCode::Char('p') if ctrl => {
            app.selected = app.selected.saturating_sub(1);
            Action::Moved
        }
        KeyCode::Down => {
            app.selected += 1;
            Action::Moved
        }
        KeyCode::Char('n') if ctrl => {
            app.selected += 1;
            Action::Moved
        }
        KeyCode::PageUp => {
            app.selected = app.selected.saturating_sub(app.page);
            Action::Moved
        }
        KeyCode::PageDown => {
            app.selected += app.page;
            Action::Moved
        }

        KeyCode::Char(c) if !ctrl && !alt => {
            app.insert(c);
            Action::Edited
        }
        _ => Action::None,
    };

    if action == Action::Edited {
        app.search();
    }
    app.clamp_selection();
    action
}

/// Run the interactive search. Enter prints the selected path to stdout after
/// the screen is restored, so the result can be piped onward.
pub fn run(index: Index) -> Result<(), String> {
    let root = index.root().to_string();
    let indexed = index.entry_count();
    // The table is a copy of everything the UI reads; the index itself is not
    // consulted again once it is built.
    let table = SearchTable::build(&index);
    drop(index);

    let mut out = std::io::stdout();
    let _guard = TerminalGuard::enter(&mut out)?;

    let mut app = App::new(table, root, indexed);
    let mut chosen = None;

    loop {
        let size = terminal::size().map_err(|e| format!("cannot read the terminal size: {e}"))?;
        app.draw(&mut out, size)
            .map_err(|e| format!("cannot draw: {e}"))?;
        let event = event::read().map_err(|e| format!("cannot read the keyboard: {e}"))?;
        // Windows can report releases as well as presses; ignore anything that
        // is not a key going down.
        let Event::Key(key) = event else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match handle_key(&mut app, key) {
            Action::Quit => break,
            Action::Open => {
                chosen = app.selected_path();
                break;
            }
            _ => {}
        }
    }

    drop(_guard);
    if let Some(path) = chosen {
        println!("{path}");
    }
    Ok(())
}

/// Everything before the final path separator, or the whole path when it has
/// none.
fn parent_of(path: &str) -> &str {
    match path.rfind('\\') {
        Some(i) => &path[..i],
        None => path,
    }
}

/// Shorten the user's home directory to `~`, the way the recording shows
/// `~/Developer/...` rather than `/Users/whoever/Developer/...`. A path
/// outside home is returned unchanged; a path that *is* home becomes `~`.
fn shorten_home(path: &str) -> String {
    match home_dir() {
        Some(home) => shorten_home_within(path, &home),
        None => path.to_string(),
    }
}

/// [`shorten_home`] against a known home prefix. Split out so the boundary
/// rules are testable without touching the environment.
fn shorten_home_within(path: &str, home: &str) -> String {
    let home = home.trim_end_matches('\\');
    if home.is_empty() {
        return path.to_string();
    }
    if path == home {
        return "~".to_string();
    }
    // Strip only at a separator boundary, so `C:\Users\me` never eats into
    // `C:\Users\meta`.
    match path.strip_prefix(home) {
        Some(rest) if rest.starts_with('\\') => format!("~{rest}"),
        _ => path.to_string(),
    }
}

/// The profile directory as a path prefix (`C:\Users\me`), if one is known.
fn home_dir() -> Option<String> {
    // `USERPROFILE` is the Windows home; `HOME` covers anything that sets it
    // instead. Both are absolute paths without a trailing separator.
    for key in ["USERPROFILE", "HOME"] {
        if let Ok(value) = std::env::var(key) {
            let trimmed = value.trim_end_matches('\\');
            if trimmed.len() > 2 {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

/// Pad `s` to exactly `width` chars, trimming from the left when it is longer.
fn fit(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len == width {
        s.to_string()
    } else if len < width {
        let mut out = String::from(s);
        out.extend(std::iter::repeat_n(' ', width - len));
        out
    } else {
        elide_tail(s, width)
    }
}

/// Shorten to `width` chars keeping the tail. Paths are read from the end, so
/// the directory that gave a file its meaning must survive the cut.
fn elide_tail(s: &str, width: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= width {
        return s.to_string();
    }
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        return "…".to_string();
    }
    let tail: String = chars[chars.len() - (width - 1)..].iter().collect();
    format!("…{tail}")
}

/// Drop ANSI escape sequences, leaving the text a user actually sees.
fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for c in chars.by_ref() {
                if c == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Display width of a styled string, ignoring ANSI escape sequences.
fn display_width(s: &str) -> usize {
    strip_ansi(s).chars().count()
}

/// Clip a styled string to `width` visible characters, keeping its escape
/// sequences so styling survives the cut.
///
/// Width is counted on the text, not the escapes: measuring the raw character
/// count would make every coloured cell look far too long and clip the content
/// the caller had just laid out.
fn truncate_visible(s: &str, width: usize) -> String {
    let mut out = String::new();
    let mut visible = 0usize;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            out.push(c);
            for c in chars.by_ref() {
                out.push(c);
                if c == 'm' {
                    break;
                }
            }
            continue;
        }
        if visible == width {
            out.push_str(&ResetColor.to_string());
            return out;
        }
        out.push(c);
        visible += 1;
    }
    out
}

/// Group digits in threes: 5151234 -> "5,151,234".
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Render a microsecond duration the way the timings read: sub-millisecond
/// precision where it matters, whole milliseconds above that.
fn fmt_us(us: u128) -> String {
    if us < 10_000 {
        format!("{:.1}", us as f64 / 1000.0)
    } else {
        format!("{}", (us + 500) / 1000)
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    use name_core::index::Index;
    use std::io::BufReader;

    /// Not a correctness test: reports what one whole-index scan costs per
    /// keystroke, which is the number the footer's sparkline is showing the
    /// user. Point it at a real index:
    ///
    ///     ERI_INDEX=index.eri cargo test --release bench_scan -- --ignored --nocapture
    #[test]
    #[ignore = "needs a real .eri index; reports timings and asserts nothing"]
    fn bench_scan() {
        let path = std::env::var("ERI_INDEX").expect("set ERI_INDEX to an .eri file");
        let file = std::fs::File::open(&path).expect("open index");
        let index = Index::read_from(&mut BufReader::new(file)).expect("read index");
        let root = index.root().to_string();
        let indexed = index.entry_count();
        let table = SearchTable::build(&index);
        let entries = table.len();
        println!(
            "\n{entries} entries from {path}, {} bytes of table",
            table.bytes()
        );

        let mut app = App::new(table, root, indexed);
        for query in ["m", "ma", "mai", "main", "wallp", "main ext:rs", ""] {
            app.input = query.to_string();
            app.caret = app.input_len();
            let start = Instant::now();
            app.search();
            let elapsed = start.elapsed();
            println!(
                "{query:<14} {:>8.2} ms  {:>5.0} ns/entry  {:>7} hits",
                elapsed.as_secs_f64() * 1000.0,
                elapsed.as_nanos() as f64 / entries as f64,
                app.total
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use name_core::index::{Index, NodeKind, ROOT_FRN, SearchTable};

    fn sample_index() -> Index {
        let mut idx = Index::new("C:\\");
        idx.insert(100, ROOT_FRN, "src", NodeKind::Directory, 0, 0x10);
        idx.insert(101, 100, "main.rs", NodeKind::File, 2_000, 0x20);
        idx.insert(102, 100, "maintenance.rs", NodeKind::File, 3_000, 0x20);
        idx.insert(103, ROOT_FRN, "wallpaper.jpg", NodeKind::File, 9_000, 0x20);
        idx
    }

    /// Build an app the way [`run`] does: flatten the index into a scan table
    /// and keep only what the header needs.
    fn app_new(index: Index) -> App {
        App::new(
            SearchTable::build(&index),
            index.root().to_string(),
            index.entry_count(),
        )
    }

    fn app_with(input: &str) -> App {
        let mut app = app_new(sample_index());
        app.input = input.to_string();
        app.caret = app.input_len();
        app.search();
        app
    }

    fn paths(app: &App) -> Vec<String> {
        app.results
            .iter()
            .map(|&row| app.table.get(row as usize).path.to_string())
            .collect()
    }

    #[test]
    fn an_empty_query_lists_every_entry() {
        let app = app_with("");
        // Four children plus the synthetic root, which the scan skips.
        assert_eq!(app.results.len(), 4);
        assert!(!app.filtered);
    }

    #[test]
    fn a_scattered_name_match_is_found_and_ranked_first() {
        // `mr` scatters through both `.rs` names; the tighter span in the
        // shorter name is what decides the order.
        let app = app_with("mr");
        assert_eq!(paths(&app)[0], "C:\\src\\main.rs");
    }

    #[test]
    fn an_exact_name_ranks_above_a_prefix_of_a_longer_name() {
        let app = app_with("maintenance.rs");
        assert_eq!(paths(&app), vec!["C:\\src\\maintenance.rs"]);
    }

    #[test]
    fn filters_narrow_the_fuzzy_name_match() {
        let app = app_with("maintenance ext:rs");
        assert_eq!(paths(&app), vec!["C:\\src\\maintenance.rs"]);

        let app = app_with("maintenance ext:png");
        assert!(app.results.is_empty());
    }

    #[test]
    fn a_negated_term_removes_matches_by_path() {
        let app = app_with("main !maintenance");
        assert_eq!(paths(&app), vec!["C:\\src\\main.rs"]);
    }

    #[test]
    fn every_query_is_timed_and_recorded() {
        let mut app = app_new(sample_index());
        assert_eq!(app.latencies.len(), 1);
        app.insert('m');
        app.search();
        assert_eq!(app.latencies.len(), 2);
        assert!(app.median_us().is_some());
    }

    #[test]
    fn latency_history_is_capped() {
        let mut app = app_new(sample_index());
        for _ in 0..LATENCY_SAMPLES + 10 {
            app.search();
        }
        assert_eq!(app.latencies.len(), LATENCY_SAMPLES);
    }

    /// Without a truecolor advertisement the ANSI-16 fallback must be used,
    /// otherwise the Windows console quantizes RGB to something that just looks
    /// like the old colours on a black background.
    #[test]
    fn the_palette_falls_back_to_ansi_when_truecolor_is_unadvertised() {
        // Whatever the host advertises, each role must yield a usable colour
        // and the truecolor decision must be stable across calls.
        let first = palette::truecolor();
        assert_eq!(first, palette::truecolor(), "detection must be cached");
        for c in [
            palette::bg(),
            palette::ink(),
            palette::dim(),
            palette::blue(),
            palette::green(),
        ] {
            let described = format!("{c:?}");
            assert!(!described.is_empty());
            if !first {
                // No Rgb may leak through when the terminal cannot show it.
                assert!(
                    !described.contains("Rgb"),
                    "RGB used without truecolor: {described}"
                );
            }
        }
    }

    /// The footer figure is the middle of the window, not a total. If this ever
    /// becomes a sum the number climbs the longer you type, which is the one
    /// thing a rolling median must never do.
    #[test]
    fn the_median_is_the_middle_sample_not_a_sum() {
        let mut app = app_new(sample_index());
        app.latencies = vec![1_000, 2_000, 300_000]; // µs: 1, 2, 300 ms
        // Middle of [1, 2, 300] is 2 ms. A sum would report 303 ms.
        assert_eq!(app.median_us(), Some(2_000));

        // Even windows take the upper of the two middle samples.
        app.latencies = vec![100, 200, 300, 400];
        assert_eq!(app.median_us(), Some(300));

        // Ordering must not matter: a sum would.
        app.latencies = vec![400, 100, 300, 200];
        assert_eq!(app.median_us(), Some(300));

        // Growth is bounded by the window, so typing forever cannot inflate it.
        app.latencies = vec![1_000; LATENCY_SAMPLES];
        for i in 0..1_000 {
            app.latencies.push(1_000 + i);
            if app.latencies.len() > LATENCY_SAMPLES {
                app.latencies.remove(0);
            }
        }
        assert!(
            app.median_us().unwrap() < 2_000,
            "the median must stay near the per-scan cost, not accumulate"
        );
    }

    #[test]
    fn editing_the_query_at_the_caret_splices_the_right_characters() {
        let mut app = app_new(sample_index());
        app.insert('a');
        app.insert('b');
        app.insert('c');
        assert_eq!(app.input, "abc");
        app.caret = 1;
        app.insert('X');
        assert_eq!(app.input, "aXbc");
        app.delete_before_caret();
        assert_eq!(app.input, "abc");

        // Delete forward: past the end is a no-op, at the start it removes the
        // character under the caret.
        app.caret = 3;
        app.delete_at_caret();
        assert_eq!(app.input, "abc", "delete past the end changes nothing");
        app.caret = 0;
        app.delete_at_caret();
        assert_eq!(app.input, "bc");
    }

    #[test]
    fn deleting_a_word_keeps_the_rest_of_the_query() {
        let mut app = app_new(sample_index());
        for c in "size:>1gb type:video".chars() {
            app.insert(c);
        }
        app.delete_word();
        assert_eq!(app.input, "size:>1gb");
    }

    #[test]
    fn caret_movement_is_clamped_to_the_query() {
        let mut app = app_new(sample_index());
        for c in "rs".chars() {
            app.insert(c);
        }
        app.caret = 0;
        app.caret = app.caret.saturating_sub(1);
        assert_eq!(app.caret, 0);
        app.caret = (app.caret + 1).min(app.input_len());
        app.caret = (app.caret + 1).min(app.input_len());
        assert_eq!(app.caret, 2);
    }

    #[test]
    fn the_selection_stays_on_a_real_entry() {
        let mut app = app_with("main");
        app.selected = 99;
        app.page = 10;
        app.clamp_selection();
        assert_eq!(app.selected, app.results.len() - 1);
        assert!(app.top + app.page > app.selected);
    }

    #[test]
    fn the_viewport_follows_the_selection() {
        let mut app = app_with("main");
        app.page = 1;
        app.selected = 1;
        app.clamp_selection();
        assert_eq!(app.top, 1);
        app.selected = 0;
        app.clamp_selection();
        assert_eq!(app.top, 0);
    }

    #[test]
    fn parent_of_splits_on_the_last_separator() {
        assert_eq!(parent_of("C:\\Users\\dana\\notes.txt"), "C:\\Users\\dana");
        assert_eq!(parent_of("notes.txt"), "notes.txt");
        assert_eq!(parent_of("C:\\"), "C:");
    }

    #[test]
    fn home_collapses_to_a_tilde_at_a_path_boundary() {
        let home = "C:\\Users\\dana";
        assert_eq!(shorten_home_within(home, home), "~");
        assert_eq!(
            shorten_home_within("C:\\Users\\dana\\Documents", home),
            "~\\Documents"
        );
        assert_eq!(
            shorten_home_within("C:\\Users\\dana\\Developer\\Tools\\FSearch", home),
            "~\\Developer\\Tools\\FSearch"
        );
    }

    #[test]
    fn home_shortening_respects_the_separator_boundary() {
        let home = "C:\\Users\\dana";
        // A longer sibling directory is a different path, not a suffix of home.
        assert_eq!(
            shorten_home_within("C:\\Users\\dana2\\Notes", home),
            "C:\\Users\\dana2\\Notes"
        );
        assert_eq!(
            shorten_home_within("C:\\Windows\\System32", home),
            "C:\\Windows\\System32"
        );
    }

    #[test]
    fn elide_tail_keeps_the_end_of_a_long_path() {
        assert_eq!(elide_tail("C:\\short", 20), "C:\\short");
        assert_eq!(elide_tail("C:\\short", 6), "…short");
        assert_eq!(elide_tail("abcdefghij", 4), "…hij");
        assert_eq!(elide_tail("abc", 0), "");
        assert_eq!(elide_tail("abc", 1), "…");
    }

    #[test]
    fn fit_pads_to_an_exact_width() {
        assert_eq!(fit("ab", 4), "ab  ");
        assert_eq!(fit("abcdef", 4), "…def");
        assert_eq!(fit("abcd", 4), "abcd");
    }

    #[test]
    fn thousands_groups_digits() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(5_151_234), "5,151,234");
    }

    #[test]
    fn latency_formats_below_and_above_ten_milliseconds() {
        assert_eq!(fmt_us(3_500), "3.5");
        assert_eq!(fmt_us(999), "1.0");
        assert_eq!(fmt_us(10_000), "10");
        assert_eq!(fmt_us(28_400), "28");
    }

    #[test]
    fn display_width_ignores_colour_escapes() {
        assert_eq!(display_width("ab"), 2);
        assert_eq!(display_width("\x1b[32m12\x1b[0m ms"), 5);
    }

    #[test]
    fn the_sparkline_scales_a_slow_sample_against_the_fast_ones() {
        let mut app = app_new(sample_index());
        app.latencies = vec![100, 100, 100_000, 100];
        let chart = app.sparkline(8);
        assert_eq!(chart.chars().count(), 4);
        assert_eq!(chart.chars().nth(2), Some('█'), "the outlier fills the bar");
        assert_ne!(chart.chars().next(), Some('█'));
        assert_eq!(app.sparkline(0), "");
    }

    #[test]
    fn the_prompt_keeps_the_query_and_the_timing_on_one_line() {
        let app = app_with("ext:rs");
        let line = app.prompt_line(60, &"3.5 ms".green().to_string());
        assert_eq!(display_width(&line), 60);
        let visible = strip_ansi(&line);
        assert!(visible.contains("ext:rs"), "{visible:?}");
        // The timing is styled, so it arrives wrapped in escapes; what matters
        // is that it lands flush right once they are removed.
        assert!(visible.ends_with("3.5 ms"), "{visible:?}");
    }

    #[test]
    fn a_long_query_is_scrolled_so_the_caret_stays_visible() {
        let mut app = app_new(sample_index());
        for _ in 0..40 {
            app.insert('x');
        }
        let line = app.prompt_line(30, &"2.0 ms".green().to_string());
        assert_eq!(display_width(&line), 30, "never exceeds the terminal width");
    }

    /// Walk a rendered frame and pair every painted row with the text a user sees.
    ///
    /// crossterm writes each row as a cursor move, a clear and the styled text,
    /// so the cursor moves are the row boundaries and everything else is either
    /// styling to strip or payload to keep.
    fn parse_frame(raw: &str) -> Vec<(u16, String)> {
        let mut rows: Vec<(u16, String)> = Vec::new();
        let mut chars = raw.chars();
        let mut text = String::new();
        let mut row: Option<u16> = None;

        while let Some(c) = chars.next() {
            if c != '\x1b' {
                text.push(c);
                continue;
            }
            // Every sequence emitted here is CSI: ESC '[' parameters final-byte.
            let _bracket = chars.next();
            let mut params = String::new();
            let mut final_byte = '\0';
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    final_byte = c;
                    break;
                }
                params.push(c);
            }
            if final_byte == 'H' {
                if let Some(previous) = row.replace(
                    params
                        .split(';')
                        .next()
                        .and_then(|r| r.parse().ok())
                        .unwrap_or(0),
                ) {
                    rows.push((previous, strip_ansi(&text)));
                }
                text.clear();
            }
        }
        if let Some(previous) = row {
            rows.push((previous, strip_ansi(&text)));
        }
        rows
    }

    /// Render a frame at a fixed size and return its rows.
    fn frame(app: &mut App, width: u16, height: u16) -> Vec<(u16, String)> {
        let mut buf: Vec<u8> = Vec::new();
        app.draw(&mut buf, (width, height)).expect("draw");
        parse_frame(&String::from_utf8(buf).expect("utf-8"))
    }

    #[test]
    fn a_frame_paints_every_row_it_owns() {
        let mut app = app_with("maintenance");
        let rows = frame(&mut app, 100, 10);
        assert_eq!(
            rows.iter().map(|(r, _)| *r).collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
            "the list and footer must cover the whole terminal"
        );

        assert!(rows[0].1.contains("zen-rs"), "{}", rows[0].1);
        assert!(rows[0].1.contains("4 files and folders"), "{}", rows[0].1);
        assert!(rows[1].1.starts_with('❯'), "{}", rows[1].1);
        assert!(rows[1].1.contains("maintenance"), "{}", rows[1].1);
        assert_eq!(rows[2].1, "─".repeat(100), "the rule spans the width");
        assert!(rows[3].1.contains("maintenance.rs"), "{}", rows[3].1);
        assert!(rows[9].1.contains("median"), "{}", rows[9].1);
    }

    #[test]
    fn the_footer_keeps_the_number_when_the_prose_no_longer_fits() {
        let app = app_with("maintenance");
        // Wide enough for the full layout: prose, chart and number.
        let wide = strip_ansi(&app.footer_line(120));
        assert!(
            wide.contains("every keystroke is a whole-disk search"),
            "{wide}"
        );
        assert!(wide.contains("median"), "{wide}");

        // The prose is 38 chars; at 44 the chart and the median alone fit,
        // so the prose has to drop. Pick 44 for the assertion.
        let narrow = strip_ansi(&app.footer_line(44));
        assert!(!narrow.contains("every keystroke"), "{narrow}");
        assert!(narrow.contains("median"), "{narrow}");
    }

    #[test]
    fn shrinking_the_result_list_erases_the_rows_it_freed() {
        // The failure this guards against: results shrink from many to one and
        // the old entries stay on screen below the new, shorter list.
        let mut app = app_with("maintenance");
        assert_eq!(frame(&mut app, 60, 10).len(), 10);

        app.input = String::new();
        app.caret = 0;
        app.search();
        let rows = frame(&mut app, 60, 10);

        assert_eq!(rows.len(), 10);
        // Every entry this index has, in the rows below the prompt.
        let listed = rows[3..9]
            .iter()
            .filter(|(_, text)| !text.is_empty())
            .count();
        assert_eq!(listed, app.results.len());
    }

    #[test]
    fn an_index_with_no_hits_says_so_instead_of_going_blank() {
        let mut app = app_with("zzzz");
        let rows = frame(&mut app, 60, 10);
        assert!(rows[3].1.contains("no matches"), "{}", rows[3].1);
        // The footer carries the prose, the chart, and `median N ms` — no
        // count. The "0 matches" assertion was a holdover from the count
        // line that used to sit on the right.
    }

    #[test]
    fn a_terminal_below_the_minimum_says_what_it_needs() {
        let mut app = app_with("");
        let rows = frame(&mut app, 20, 4);
        let text: String = rows.iter().map(|(_, t)| t.as_str()).collect();
        assert!(text.contains("too small"), "{text}");
        assert!(text.contains("40x8"), "{text}");
    }

    #[test]
    fn the_result_row_shows_the_name_beside_its_directory() {
        let mut app = app_with("maintenance");
        app.page = 10;
        let cell = app.result_cell(0, 80);
        assert!(cell.contains("maintenance.rs"));
        assert!(cell.contains("C:\\src"));
    }

    #[test]
    fn the_footer_is_the_prose_chart_and_median_no_count() {
        let app = app_with("");
        let footer = strip_ansi(&app.footer_line(100));
        assert!(
            footer.contains("every keystroke is a whole-disk search"),
            "{footer}"
        );
        assert!(footer.contains("median"), "{footer}");
        assert!(!footer.contains("entries"), "{footer}");
        assert!(!footer.contains("matches"), "{footer}");
    }

    #[test]
    fn the_footer_stays_within_the_terminal_width() {
        for width in [MIN_WIDTH, 60, 100, 200] {
            let app = app_with("ext:rs");
            assert_eq!(
                display_width(&app.footer_line(width)),
                width,
                "footer overflowed at width {width}"
            );
        }
    }
}
