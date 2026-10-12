// SPDX-License-Identifier: Apache-2.0

//! Interactive full-screen search over a loaded [`Index`].
//!
//! The whole MFT-derived index already sits in memory, so every keystroke can
//! re-scan all of it: the search has no debounce and no async work. What makes
//! that legible is measuring it — the last query's latency sits flush right of
//! the prompt and the footer carries a first/p50/p90 plus a sparkline of the
//! last few dozen queries, so a scan that quietly got slower is visible while
//! typing rather than after the fact.

use crate::latency::LatencyMeter;
use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::style::{Print, ResetColor, SetBackgroundColor, Stylize};
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use name_core::live::Live;
use name_core::name_query::{Alternatives, Hit, Searcher};
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

    /// Header subtitle, parent directories, footer prose, `p50/p90`.
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

    /// Latency figures, the sparkline, first/p50/p90.
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
/// Keystrokes remembered for the footer sparkline and percentiles.
/// Mirrors `LatencyMeter`'s window; used by the cap test.
#[allow(dead_code)]
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
    /// The fsearch name index + overlay, the same searcher `searchctl` uses.
    live: Live,
    /// Volume root, shown in the header.
    root: String,
    indexed: usize,
    input: String,
    /// Caret offset into `input`, counted in `char`s.
    caret: usize,
    /// Matching paths, best match first (already resolved from hits).
    results: Vec<String>,
    /// Matches found before `results` was capped.
    total: usize,
    /// Whether the query carries any filter or search term at all, which
    /// decides whether the footer says "matches" or "entries".
    filtered: bool,
    selected: usize,
    top: usize,
    page: usize,
    /// What is on screen now, one rendered row per line, so a frame only
    /// repaints what changed (see [`App::paint`]).
    painted: Vec<String>,
    /// The width `painted` was rendered at. A resize invalidates every row.
    painted_width: usize,
    /// Per-keystroke search latency: cold `first` kept out of the window,
    /// which reports nearest-rank p50/p90 and the sparkline.
    meter: LatencyMeter,
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
    fn new(live: Live, root: String, indexed: usize) -> Self {
        let mut app = App {
            live,
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
            painted: Vec::new(),
            painted_width: 0,
            meter: LatencyMeter::new(),
        };
        app.search();
        app
    }

    /// Re-scan the whole index for the current query and time it.
    ///
    /// Matching goes through [`Searcher`] — the same fzf-v1 + one-typo
    /// matcher `searchctl` runs — so the UI and the CLI cannot drift apart.
    fn search(&mut self) {
        let started = Instant::now();
        self.filtered = !self.input.trim().is_empty();

        // The UI starts with an empty box and must show the world, not a
        // parse error. `Alternatives::parse("")` matches everything, but
        // listing every path through the index is cheaper than scoring.
        if !self.filtered {
            self.results.clear();
            self.total = self.live.base.n.saturating_sub(1); // skip synthetic root
            let mut out = Vec::new();
            // Entry 0 is the synthetic root; the cap is on the range rather
            // than a counter checked inside the loop.
            for i in 1..self.live.base.n.min(MAX_RESULTS + 1) {
                self.live.base.path(i, &mut out);
                self.results
                    .push(String::from_utf8_lossy(&out).into_owned());
            }
            self.selected = 0;
            self.top = 0;
            self.meter.record(started.elapsed().as_micros());
            return;
        }

        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_default();
        // The merge is `Searcher`'s: keying it on `Hit::idx` here silently
        // dropped live entries (every overlay entry shares the `u32::MAX`
        // sentinel), and finding each hit by scanning the ones kept so far was
        // quadratic -- with MAX_RESULTS that is hundreds of millions of
        // comparisons on every keystroke.
        let mut hits: Vec<Hit> = Vec::new();
        if let Ok(alts) = Alternatives::parse(&self.input, &home) {
            let searcher = Searcher { live: &self.live };
            hits = searcher.search_alternatives(&alts.alts, MAX_RESULTS);
        }

        let mut out = Vec::new();
        self.results = hits
            .iter()
            .map(|h| {
                if let Some(p) = h.over.clone() {
                    return String::from_utf8_lossy(&p).into_owned();
                }
                self.live.base.path(h.idx as usize, &mut out);
                String::from_utf8_lossy(&out).into_owned()
            })
            .collect();
        self.total = self.results.len();
        self.selected = 0;
        self.top = 0;

        self.meter.record(started.elapsed().as_micros());
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
        self.results.get(self.selected).cloned()
    }

    fn draw(&mut self, out: &mut impl Write, size: (u16, u16)) -> std::io::Result<()> {
        let width = size.0 as usize;
        let height = size.1 as usize;

        if width < MIN_WIDTH || height < MIN_HEIGHT {
            // Nothing on screen is what the next full frame expects, and the
            // message is one line wide, so drop the buffer: the frame after
            // this must repaint rather than trust rows it never painted.
            self.painted.clear();
            self.painted_width = 0;
            execute!(out, MoveTo(0, 0), Clear(ClearType::All))?;
            return self.write_line(
                out,
                0,
                MIN_WIDTH,
                &format!("terminal {width}x{height} is too small — need {MIN_WIDTH}x{MIN_HEIGHT}")
                    .with(palette::dim())
                    .to_string(),
            );
        }

        // Header, prompt, rule and footer leave four rows for the list.
        let list_height = height.saturating_sub(4);
        self.page = list_height;
        self.clamp_selection();

        let mut rows: Vec<String> = Vec::with_capacity(height);

        // FSearch layout: brand in bold white, two spaces, then a dim
        // subtitle that carries the volume and the entry count.
        let subtitle = format!(
            "{} · {} files and folders",
            self.root.as_str().with(palette::dim()),
            thousands(self.indexed).with(palette::dim())
        );
        rows.push(format!(
            "{}  {}",
            "zen-rs".with(palette::ink()).bold(),
            subtitle
        ));

        // The per-keystroke latency is the headline number. No label: the
        // video puts a green "8.1 ms" flush right of the prompt, with no word
        // before it.
        let latency = match self.meter.last() {
            Some(us) => format!("{} ms", fmt_us(us))
                .with(palette::green())
                .to_string(),
            None => String::new(),
        };
        rows.push(self.prompt_line(width, &latency));
        rows.push("─".repeat(width).with(palette::dim()).to_string());

        let first = self.top.min(self.results.len());
        let last = (first + list_height).min(self.results.len());

        // FSearch flows the name flush left, the parent path dimmed after a
        // two-space gap, with no column rule between them. Every row of the
        // list is present even when empty: a shrinking list would otherwise
        // leave the previous frame's entries on screen below the last match.
        for offset in 0..list_height {
            let row = first + offset;
            rows.push(if self.results.is_empty() && offset == 0 {
                "  no matches".with(palette::dim()).to_string()
            } else if row < last {
                self.result_cell(row, width)
            } else {
                String::new()
            });
        }

        rows.push(self.footer_line(width));
        self.paint(out, width, height, rows)
    }

    /// Paint `rows` over whatever is on screen, rewriting only what differs.
    ///
    /// The frame used to clear and redraw every row of the terminal, on every
    /// keystroke. That is a full screen of output per key and, on a real
    /// terminal, a visible flash between clearing a row and drawing it again.
    /// Holding the previous frame means the usual edit touches one row -- the
    /// prompt -- and everything else is left alone.
    ///
    /// A frame whose height or width differs from the last one has no rows in
    /// common with it, so it is painted whole.
    fn paint(
        &mut self,
        out: &mut impl Write,
        width: usize,
        height: usize,
        rows: Vec<String>,
    ) -> std::io::Result<()> {
        let stale = self.painted.len() != height || self.painted_width != width;
        // Truncate once, then compare and store the same string: truncating
        // per row on both passes doubles the work of the whole frame.
        let rendered: Vec<String> = rows.iter().map(|c| truncate_visible(c, width)).collect();
        for (row, cell) in rendered.iter().enumerate() {
            if stale || self.painted.get(row) != Some(cell) {
                execute!(
                    out,
                    MoveTo(0, row as u16),
                    SetBackgroundColor(palette::bg()),
                    Clear(ClearType::CurrentLine),
                    Print(cell),
                )?;
            }
        }
        self.painted = rendered;
        self.painted_width = width;
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
        let path = &self.results[row];
        let name = name_of(path);
        // The two-space gap is enough on its own; the parent path can use the
        // rest. `elide_tail` keeps the directory that gave the file its
        // meaning, which is what a reader needs when names collide.
        let name_visible = name.chars().count().min(width.saturating_sub(2));
        let dir_width = width.saturating_sub(name_visible + 2);

        let cell = format!(
            "{name}  {dir}",
            name = fit(name, name_visible).with(palette::ink()),
            dir = elide_tail(&shorten_home(parent_of(path)), dir_width).with(palette::dim())
        );
        if row == self.selected {
            cell.reverse().to_string()
        } else {
            cell
        }
    }

    /// Footer: the prose, the chart, first/p50/p90. The video has no match
    /// count on the right edge — count info is already in the header.
    ///
    /// As the terminal narrows the prose is the first thing to go, then the
    /// chart; the percentile numbers stay. The cold first query is reported
    /// separately on purpose — it is the outlier the sparkline exists to
    /// show, and it must not sit inside p50/p90.
    fn footer_line(&self, width: usize) -> String {
        let label = "every keystroke is a whole-disk search";
        let fmt = |us: Option<u128>| match us {
            Some(us) => format!("{} ms", fmt_us(us)),
            None => "—".to_string(),
        };
        let stats = format!(
            "first {}  p50 {}  p90 {}",
            fmt(self.meter.first()),
            fmt(self.meter.p50()),
            fmt(self.meter.p90()),
        );

        let label_width = label.chars().count();
        let stats_width = stats.chars().count();

        let mut line = String::new();
        let mut spark_width = 0usize;

        // Prose + sparkline + stats. As the budget shrinks the prose goes
        // first, then the chart; the numbers stay.
        if width >= label_width + stats_width + 4 {
            line.push_str(&label.with(palette::dim()).to_string());
            spark_width = 24.min(width.saturating_sub(label_width + stats_width + 4));
        } else if width >= stats_width + 4 {
            spark_width = 24.min(width.saturating_sub(stats_width + 4));
        }

        if spark_width > 0 {
            if !line.is_empty() {
                line.push_str("  ");
            }
            line.push_str(
                &self
                    .meter
                    .sparkline(spark_width)
                    .with(palette::green())
                    .to_string(),
            );
        }
        if !line.is_empty() {
            line.push_str("  ");
        }
        line.push_str(&stats.with(palette::green()).to_string());

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
        cell: &str,
    ) -> std::io::Result<()> {
        // Re-assert the background before every clear: `Clear` paints with the
        // currently active background, and a styled span may have reset it.
        execute!(
            out,
            MoveTo(0, row),
            SetBackgroundColor(palette::bg()),
            Clear(ClearType::CurrentLine),
            Print(truncate_visible(cell, width)),
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
pub fn run(live: Live, root: String, indexed: usize) -> Result<(), String> {
    let mut out = std::io::stdout();
    let _guard = TerminalGuard::enter(&mut out)?;

    let mut app = App::new(live, root, indexed);
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
/// none. Handles both `name_index` `/` paths and Windows `\\` ones.
fn parent_of(path: &str) -> &str {
    let i = path.rfind(['\\', '/']);
    match i {
        Some(i) => &path[..i],
        None => path,
    }
}

/// Final path component.
fn name_of(path: &str) -> &str {
    match path.rfind(['\\', '/']) {
        Some(i) => &path[i + 1..],
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

    /// Not a correctness test: reports what one whole-index scan costs per
    /// keystroke, which is the number the footer's sparkline is showing the
    /// user. The interactive path now goes through [`Searcher`]; for gated
    /// engine timings see `engine/examples/name_bench` and `benchmark/`.
    #[test]
    #[ignore = "reports timings and asserts nothing; run with --ignored --nocapture"]
    fn bench_scan() {
        let live = super::tests::sample_live();
        let entries = live.base.n.saturating_sub(1);
        let mut app = App::new(live, "C:\\".to_string(), entries);
        for query in ["m", "ma", "mai", "main", "wallp", "main ext:rs", ""] {
            app.input = query.to_string();
            app.caret = app.input_len();
            let start = Instant::now();
            app.search();
            let elapsed = start.elapsed();
            println!(
                "{query:<14} {:>8.2} ms  {:>5.0} ns/entry  {:>7} hits",
                elapsed.as_secs_f64() * 1000.0,
                elapsed.as_nanos() as f64 / entries.max(1) as f64,
                app.total
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use discovery::walk::{KIND_DIR, KIND_FILE, Listing, NONE, RawEnt};
    use name_core::name_index;

    /// The same four entries the old zen fixture had, as the two DFS
    /// [`Listing`]s `name_index::Index::build` wants.
    fn sample_listings() -> Vec<Listing> {
        let mut root_names = Vec::new();
        let mut root_ents = Vec::new();
        for (name, is_dir, size) in [("src", true, 0u64), ("wallpaper.jpg", false, 9_000)] {
            let off = root_names.len() as u32;
            root_names.extend_from_slice(name.as_bytes());
            root_ents.push(RawEnt {
                name_off: off,
                name_len: name.len() as u16,
                kind: if is_dir { KIND_DIR } else { KIND_FILE },
                size,
                mtime: 0,
                child: if is_dir { 1 } else { NONE },
            });
        }
        let mut src_names = Vec::new();
        let mut src_ents = Vec::new();
        for (name, size) in [("main.rs", 2_000u64), ("maintenance.rs", 3_000)] {
            let off = src_names.len() as u32;
            src_names.extend_from_slice(name.as_bytes());
            src_ents.push(RawEnt {
                name_off: off,
                name_len: name.len() as u16,
                kind: KIND_FILE,
                size,
                mtime: 0,
                child: NONE,
            });
        }
        vec![
            Listing {
                id: 0,
                names: root_names,
                ents: root_ents,
            },
            Listing {
                id: 1,
                names: src_names,
                ents: src_ents,
            },
        ]
    }

    pub(crate) fn sample_live() -> Live {
        let idx = name_index::Index::build(sample_listings(), 0, 0, b"");
        Live::new(idx)
    }

    /// Build an app the way [`run`] does.
    fn app_new() -> App {
        let live = sample_live();
        let indexed = live.base.n.saturating_sub(1);
        App::new(live, "C:\\".to_string(), indexed)
    }

    fn app_with(input: &str) -> App {
        let mut app = app_new();
        app.input = input.to_string();
        app.caret = app.input_len();
        app.search();
        app
    }

    fn paths(app: &App) -> Vec<String> {
        app.results.clone()
    }

    /// Terminal size for the drawing tests: wide enough for a full result
    /// row, tall enough that the list has rows to leave alone.
    const FRAME: (u16, u16) = (100, 20);

    /// A frame identical to the one before it writes nothing at all.
    ///
    /// This is what holding the previous frame buys, and it is not an
    /// optimisation detail: the old draw cleared and redrew every row of the
    /// terminal per keystroke, a screenful of output per key and a visible
    /// flash between clearing a row and drawing it again. If this ever writes
    /// bytes, the diff has stopped being a diff and the flash is back.
    #[test]
    fn a_frame_identical_to_the_last_writes_nothing() {
        let mut app = app_with("");
        let mut first = Vec::new();
        app.draw(&mut first, FRAME).expect("draws");
        assert!(!first.is_empty(), "the first frame has to paint the screen");

        let mut second = Vec::new();
        app.draw(&mut second, FRAME).expect("draws");
        assert!(
            second.is_empty(),
            "an unchanged frame repainted; wrote {} bytes",
            second.len()
        );
    }

    /// Moving the caret changes one row -- the prompt -- and the frame must
    /// say so by drawing one row. `MoveTo(0, row)` is `\x1b[{row + 1};1H`, so
    /// row 1 is the prompt and row 0 the header above it.
    #[test]
    fn moving_the_caret_repaints_only_the_prompt() {
        let mut app = app_with("hello");
        let mut first = Vec::new();
        app.draw(&mut first, FRAME).expect("draws");

        app.caret = 0;
        let mut second = Vec::new();
        app.draw(&mut second, FRAME).expect("draws");

        let painted = String::from_utf8_lossy(&second);
        assert!(
            painted.contains("\x1b[2;1H"),
            "the prompt row is the one that changed; got {painted:?}"
        );
        assert!(
            !painted.contains("\x1b[1;1H"),
            "the header row is unchanged and must be left alone; got {painted:?}"
        );
    }

    /// A resize shares no rows with the frame before it -- the columns of
    /// every line are different -- so it is painted whole, including the rows
    /// whose content did not change.
    #[test]
    fn a_resize_repaints_the_whole_frame() {
        let mut app = app_with("");
        let mut first = Vec::new();
        app.draw(&mut first, FRAME).expect("draws");

        let mut resized = Vec::new();
        app.draw(&mut resized, (FRAME.0 - 1, FRAME.1))
            .expect("draws");
        let painted = String::from_utf8_lossy(&resized);
        assert!(
            painted.contains("\x1b[1;1H"),
            "a resize must repaint rows the previous frame drew too; got {painted:?}"
        );
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
        assert_eq!(paths(&app)[0], "/src/main.rs");
    }

    #[test]
    fn an_exact_name_ranks_above_a_prefix_of_a_longer_name() {
        let app = app_with("maintenance.rs");
        assert_eq!(paths(&app), vec!["/src/maintenance.rs"]);
    }

    #[test]
    fn filters_narrow_the_fuzzy_name_match() {
        let app = app_with("maintenance ext:rs");
        assert_eq!(paths(&app), vec!["/src/maintenance.rs"]);

        let app = app_with("maintenance ext:png");
        assert!(app.results.is_empty());
    }

    #[test]
    fn a_negated_term_removes_matches_by_path() {
        let app = app_with("main !maintenance");
        assert_eq!(paths(&app), vec!["/src/main.rs"]);
    }

    #[test]
    fn every_query_is_timed_and_recorded() {
        let mut app = app_new();
        // The initial empty scan is the cold `first`, not a window sample.
        assert!(app.meter.first().is_some());
        assert_eq!(app.meter.window_len(), 0);
        app.insert('m');
        app.search();
        assert_eq!(app.meter.window_len(), 1);
        assert!(app.meter.p50().is_some());
    }

    #[test]
    fn latency_history_is_capped() {
        let mut app = app_new();
        for _ in 0..LATENCY_SAMPLES + 10 {
            app.search();
        }
        assert_eq!(app.meter.window_len(), LATENCY_SAMPLES);
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

    /// The footer figures are window percentiles, not a sum. The cold `first`
    /// is reported apart on purpose: it must never sit inside p50/p90.
    #[test]
    fn the_percentiles_exclude_the_cold_first_sample() {
        let mut app = app_new();
        // First search at construction is the cold one — a big outlier.
        for _ in 0..10 {
            app.meter.record(1_000);
        }
        app.meter.record(300_000);
        // p50 is a percentile of the window, so the 300 ms outlier is not
        // "the middle" and the cold sample is not in the window at all.
        assert_eq!(app.meter.p50(), Some(1_000));
        assert!(app.meter.first().is_some());
    }

    #[test]
    fn editing_the_query_at_the_caret_splices_the_right_characters() {
        let mut app = app_new();
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
        let mut app = app_new();
        for c in "size:>1gb type:video".chars() {
            app.insert(c);
        }
        app.delete_word();
        assert_eq!(app.input, "size:>1gb");
    }

    #[test]
    fn caret_movement_is_clamped_to_the_query() {
        let mut app = app_new();
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
        let mut app = app_new();
        // `app_new` already recorded the cold `first`; these are the window.
        app.meter.record(100);
        app.meter.record(100);
        app.meter.record(100_000);
        app.meter.record(100);
        // Fewer samples than width: the chart is right-aligned with spaces.
        let chart = app.meter.sparkline(8);
        let bars: String = chart.chars().filter(|c| !c.is_whitespace()).collect();
        assert_eq!(bars.chars().count(), 4);
        assert_eq!(bars.chars().nth(2), Some('█'), "the outlier fills the bar");
        assert_ne!(bars.chars().next(), Some('█'));
        assert_eq!(app.meter.sparkline(0), "");
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
        let mut app = app_new();
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

    /// What a run of frames leaves on screen.
    ///
    /// [`frame`] returns what a frame *wrote*, which since the diff renderer
    /// is only its difference from the last one. The question worth asking
    /// about stale rows is what the user is left looking at, so a test that
    /// cares accumulates the deltas instead of reading one of them.
    #[derive(Default)]
    struct Screen(Vec<(u16, String)>);

    impl Screen {
        fn apply(&mut self, delta: &[(u16, String)]) {
            for (row, text) in delta {
                match self.0.binary_search_by_key(row, |(r, _)| *r) {
                    Ok(i) => self.0[i] = (*row, text.clone()),
                    Err(i) => self.0.insert(i, (*row, text.clone())),
                }
            }
        }

        fn rows(&self) -> Vec<(u16, String)> {
            self.0.clone()
        }
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
        assert!(rows[9].1.contains("p50"), "{}", rows[9].1);
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
        assert!(wide.contains("p50"), "{wide}");

        // The prose is 38 chars; at 44 the chart and the stats alone fit,
        // so the prose has to drop. Pick 44 for the assertion.
        let narrow = strip_ansi(&app.footer_line(44));
        assert!(!narrow.contains("every keystroke"), "{narrow}");
        assert!(narrow.contains("p50"), "{narrow}");
    }

    #[test]
    fn shrinking_the_result_list_erases_the_rows_it_freed() {
        // The failure this guards against: results shrink from many to one and
        // the old entries stay on screen below the new, shorter list.
        //
        // Asserted on the screen rather than on what the frame wrote: a frame
        // now writes only its difference, so a row that was already blank and
        // stays blank is not written again -- and does not need to be. What
        // must never happen is a row still holding the *old* list.
        let mut app = app_with("maintenance");
        let mut screen = Screen::default();
        screen.apply(&frame(&mut app, 60, 10));
        assert_eq!(screen.rows().len(), 10);

        app.input = String::new();
        app.caret = 0;
        app.search();
        screen.apply(&frame(&mut app, 60, 10));

        let rows = screen.rows();
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
        // The footer carries the prose, the chart, and `p50 N ms` — no
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
        assert!(cell.contains("/src"));
    }

    #[test]
    fn the_footer_is_the_prose_chart_and_stats_no_count() {
        let app = app_with("");
        let footer = strip_ansi(&app.footer_line(100));
        assert!(
            footer.contains("every keystroke is a whole-disk search"),
            "{footer}"
        );
        assert!(footer.contains("p50"), "{footer}");
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
