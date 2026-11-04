// SPDX-License-Identifier: Apache-2.0

//! Interactive full-screen search over a loaded [`Index`].
//!
//! The whole MFT-derived index already sits in memory, so every keystroke can
//! re-scan all of it: the search has no debounce and no async work. What makes
//! that legible is measuring it — the last query's latency sits flush right of
//! the prompt and the footer carries a first/p50/p90 plus a sparkline of the
//! last few dozen queries, so a scan that quietly got slower is visible while
//! typing rather than after the fact.
//!
//! The commands are the launcher's, and the terminal knows them: a line
//! that starts with `>` is a command and not a query, and the terminal
//! takes it before the daemon sees it — the daemon would look for the
//! literal text of the line and answer nothing. What the terminal
//! cannot do, it says: it has no window to open a file in and owns no
//! clipboard.
//!
//! The keys are the launcher's too, with one of its own: `Control+C` is
//! how a user leaves a terminal, so it leaves this one, and the copy is
//! the `>copy` command.

use crate::preview;
use crate::term::{self, Color, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, Paint};
use rgy_launcher::Action as LauncherAction;
use rgy_launcher::latency::{LatencyMeter, duration};
use rgy_launcher::text::{Brand, Lang, Text};
use rgy_launcher::theme::{Rgb, Theme};
use rgy_name_core::live::Live;
use rgy_name_core::name_query::{Alternatives, Hit, Searcher};
use std::io::Write;
use std::time::Instant;

/// Whether the host advertises 24-bit colour. `COLORTERM` is the standard
/// signal; `WT_SESSION` is Windows Terminal and `TERM_PROGRAM` covers
/// iTerm2/WezTerm/VS Code. A console that advertises none of them must not
/// be sent RGB.
mod palette {
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
}

/// The colours one frame is drawn in.
///
/// The theme is the launcher's vocabulary of roles and this is six of them,
/// resolved once when the app is built: a frame has no business asking the
/// environment per row. 24-bit SGR is not universal, and a terminal that
/// cannot honour it shows what it can — which is how the "colours look old
/// and the background is pure black" symptom happens — so each role carries
/// both the theme's RGB and the nearest ANSI-16 colour, and
/// [`palette::truecolor`] picks. The fallback keeps the theme's hues (blue
/// prompt, green timings) at the 16-colour palette's saturation; the RGB is
/// used only where it will actually render.
struct Palette {
    /// What the screen is cleared with.
    bg: Color,
    /// Names, the query — what the user is reading.
    ink: Color,
    /// Header subtitle, parent directories, footer prose, `p50/p90`.
    dim: Color,
    /// The `❯` prompt and the caret — the machine's own mark.
    accent: Color,
    /// The characters of a name the query matched: the span that earned
    /// the score.
    matched: Color,
    /// Latency figures, the sparkline, first/p50/p90.
    good: Color,
}

impl Palette {
    /// The palette a theme resolves to on this terminal.
    fn of(theme: Theme) -> Palette {
        Palette::resolved(theme, palette::truecolor())
    }

    /// The palette a theme resolves to, given whether the terminal takes
    /// 24-bit colour.
    ///
    /// Split out of [`Palette::of`] so the resolution is a fact the tests
    /// can pin on both branches: the environment decides which one the app
    /// runs in, and it decides it once.
    fn resolved(theme: Theme, truecolor: bool) -> Palette {
        let role = |rgb: Rgb, ansi: Color| {
            if truecolor {
                Color::Rgb {
                    r: rgb.r,
                    g: rgb.g,
                    b: rgb.b,
                }
            } else {
                ansi
            }
        };
        Palette {
            bg: role(theme.background, Color::Black),
            ink: role(theme.text, Color::White),
            dim: role(theme.dim, Color::DarkGrey),
            accent: role(theme.accent, Color::Blue),
            matched: role(theme.matched, Color::Yellow),
            good: role(theme.good, Color::Green),
        }
    }
}

/// Matches kept for ranking. Beyond this the list is already far longer than a
/// screen, and the true total is reported separately.
const MAX_RESULTS: usize = 20_000;
/// How many screens one question asks the source for.
///
/// The list is a **screen**: the arrows page through the answer it was
/// given, so a question asks for the screens they can reach — not for
/// every path the source holds. The daemon pays for the answer on the
/// wire and the terminal pays for it on the thread that draws. Measured
/// against the daemon on a 971,675-entry index on the CachyOS box: asking
/// for twenty thousand hits cost **1,255 ms of one keystroke**; asking for
/// a window of pages costs milliseconds, and `PgDn` still walks it.
const PAGE_WINDOW: usize = 8;
/// Keystrokes remembered for the footer sparkline and percentiles.
/// Mirrors `LatencyMeter`'s window; used by the cap test.
#[cfg_attr(not(test), allow(dead_code))]
const LATENCY_SAMPLES: usize = 64;
const MIN_WIDTH: usize = 40;
const MIN_HEIGHT: usize = 8;

/// Restores the console on drop, so a panic or an early error still leaves a
/// usable terminal behind.
struct TerminalGuard;

impl TerminalGuard {
    /// Enter the raw mode and the alternate screen, painted with the
    /// background the frames are drawn on.
    ///
    /// The guard has no app to ask, so the background comes in the way the
    /// code already threads values: the caller hands over the palette's
    /// `bg`, and the whole alternate screen is painted with it so the ink
    /// colours read the way they do in the theme, whatever the host
    /// terminal's own is.
    fn enter(out: &mut impl Write, bg: Color) -> Result<Self, String> {
        term::enable_raw_mode().map_err(|e| format!("cannot enter raw mode: {e}"))?;
        let mut screen = term::Screen::new(&mut *out);
        screen
            .enter_alternate_screen()
            .and_then(|()| screen.hide())
            .and_then(|()| screen.background(bg))
            .and_then(|()| screen.clear_all())
            .and_then(|()| screen.flush())
            .map_err(|e| format!("cannot set up the screen: {e}"))?;
        Ok(TerminalGuard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut out = rgy_engine::console::stdout();
        let mut screen = term::Screen::new(&mut out);
        let _ = screen.reset_colour();
        let _ = screen.show();
        let _ = screen.leave_alternate_screen();
        let _ = screen.flush();
        let _ = term::disable_raw_mode();
    }
}

struct App {
    /// The language the launcher speaks.
    ///
    /// The terminal is the launcher without a desktop, so it says the same
    /// words in the same language; the words are the launcher's, and the
    /// language is a byte the caller hands over.
    lang: Lang,
    /// The name the header carries.
    ///
    /// The terminal is one face of the launcher and the graphical window
    /// is the other, and the window's title says "rgxy", so the header
    /// says it too: two faces that carry two names are a launcher the
    /// user has to learn twice.
    ///
    /// The tool that keeps an index in its own process is a different
    /// program, and it says its own name.
    brand: Brand,
    /// The colours the frame is drawn in.
    ///
    /// The theme is the caller's the way the language is: the terminal is
    /// one face of the launcher and the theme is an answer every face
    /// gives. An entry point that names no theme stays with MIDNIGHT, the
    /// palette this interface was drawn in, so the default is today's
    /// colours byte for byte.
    palette: Palette,
    /// The fsearch name index + overlay, the same searcher `searchctl` uses.
    /// Volume root, shown in the header.
    root: String,
    /// How many entries the index holds, when the source can say.
    ///
    /// `None` is "not known", and it is not zero: zero is a fact —
    /// "there is nothing here" — and a header that says "0" about an
    /// index of a thousand entries is a wrong fact. A source that cannot
    /// say how big its index is leaves the header saying that it does
    /// not know.
    indexed: Option<usize>,
    input: String,
    /// Caret offset into `input`, counted in `char`s.
    caret: usize,
    /// Matching paths, best match first (already resolved from hits).
    results: Vec<String>,
    /// The characters of each result the query matched, so the row can show
    /// the span that earned the score.
    matched: Vec<Vec<u32>>,
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
    /// Where the answers come from, when they come from somewhere else.
    ///
    /// `None` is the index in this process, which is what the UI has
    /// always done. `Some` is a source the caller owns — the daemon, for
    /// the launcher — and then the index in this process is not built at
    /// all, which is the whole point: the answers are already there.
    source: Option<Box<dyn rgy_launcher::Source>>,
    /// The index in this process, when there is one.
    live: Option<Live>,
    /// What a command had to say, drawn over the results.
    ///
    /// The window shows the preview and the menu as panels over the
    /// list; the terminal's answer is the same shape, because a command
    /// that does something and says nothing is a launcher the user
    /// cannot tell from a frozen one.
    notice: Option<Notice>,
}

/// What a key press did to the app.
#[derive(PartialEq, Debug)]
enum Action {
    None,
    Moved,
    Edited,
    Quit,
    /// Do one of the launcher's actions to the selected result.
    Run(LauncherAction),
    /// The line names a command that does not exist.
    Unknown,
}

/// Why the session ended.
#[derive(PartialEq, Debug)]
enum Outcome {
    /// Still running.
    None,
    /// The user quit.
    Quit,
    /// A result was chosen: its path is printed after the screen is
    /// restored.
    Chosen,
}

/// What a command has to say.
///
/// The window shows the preview and the menu as panels over the results,
/// and the terminal's answer is the same shape: the outcome of the
/// command is where the results were, and `Escape` takes it away.
///
/// The terminal cannot do what the window does — it has no window to open
/// a file in and owns no clipboard — so every line here says what was
/// really done.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Notice {
    /// The first line: what the notice is about.
    title: String,
    /// The lines under it.
    lines: Vec<String>,
}

impl Notice {
    /// Nothing to act on: the command had no result to work on.
    ///
    /// The words are the vocabulary's: the terminal says every sentence the
    /// window says in the language the launcher read once at start-up, and
    /// a panel written in English inside a Portuguese launcher is a panel
    /// the user has to read twice.
    fn nothing(lang: Lang) -> Notice {
        let text = Text::for_lang(lang);
        Notice {
            title: text.notice_nothing.to_string(),
            lines: vec![text.no_result_selected.to_string()],
        }
    }

    /// The folder that holds the selected path.
    ///
    /// The window opens it; the terminal has no file manager to open it
    /// with, so it says which folder that is and leaves it there.
    fn folder(path: &str, lang: Lang) -> Notice {
        let text = Text::for_lang(lang);
        Notice {
            title: text.notice_folder_title.to_string(),
            lines: vec![text.notice_folder.to_string(), parent_of(path).to_string()],
        }
    }

    /// The path, for the user to copy.
    ///
    /// The window puts it on the clipboard; the terminal owns no
    /// clipboard, so it prints the path and says that is what it did.
    fn copy(path: &str, lang: Lang) -> Notice {
        let text = Text::for_lang(lang);
        Notice {
            title: text.notice_copy_title.to_string(),
            lines: vec![text.notice_copy.to_string(), path.to_string()],
        }
    }

    /// The beginning of the selected file.
    fn preview(path: &str, lang: Lang) -> Notice {
        let preview = preview::of(path, lang);
        let title = preview.title;
        Notice {
            title: rgy_launcher::text::fill(Text::for_lang(lang).notice_preview_title, &[&title]),
            lines: preview.lines,
        }
    }

    /// The actions of the launcher, with the key that does each one.
    ///
    /// What each action does is the terminal's own word, and the word is
    /// the vocabulary's: the two faces say the same thing about the same
    /// action, in the same language.
    fn menu(lang: Lang) -> Notice {
        let text = Text::for_lang(lang);
        let lines = rgy_launcher::COMMANDS
            .iter()
            .map(|command| {
                let name = format!(">{}", command.name);
                let key = shortcut(command.action);
                // The two columns are the longest command and the longest
                // key plus the space between them: `>settings` is nine
                // cells in both columns, and a nine-cell name column put
                // the two together -- the row read `>settings>settings`,
                // which is a command nobody can type.
                format!("{name:<10}{key:<12}{}", describe(command.action, text))
            })
            .collect();
        Notice {
            title: text.notice_actions.to_string(),
            lines,
        }
    }

    /// The line names a command that does not exist.
    ///
    /// The sentence is the launcher's own: the two faces say the same
    /// thing about the same mistake, in the same language.
    fn unknown(text: &str, lang: Lang) -> Notice {
        Notice {
            title: Text::for_lang(lang).notice_unknown_command.to_string(),
            lines: vec![rgy_launcher::commands::unknown(text, lang)],
        }
    }

    /// The panel's rows, wrapped to the width.
    ///
    /// The notice is not the app's, so the palette comes in the way the
    /// code already threads values: the caller hands over the colours the
    /// frame is drawn in.
    fn rows(&self, width: usize, palette: &Palette) -> Vec<String> {
        let mut out = vec![self.title.clone().with(palette.ink).bold().to_string()];
        for line in &self.lines {
            for piece in wrap(line, width) {
                out.push(piece.with(palette.ink).to_string());
            }
        }
        out
    }
}

/// What each action does, in the terminal's own words.
///
/// The launcher's labels are the window's; the terminal has a voice of
/// its own, and this is it — in the language the launcher speaks.
fn describe(action: LauncherAction, text: &Text) -> &'static str {
    match action {
        LauncherAction::Open => text.terminal_open,
        LauncherAction::OpenFolder => text.terminal_open_folder,
        LauncherAction::CopyPath => text.terminal_copy_path,
        LauncherAction::Preview => text.terminal_preview,
        LauncherAction::Menu => text.terminal_menu,
        LauncherAction::Settings => text.terminal_settings,
    }
}

/// The key that does the action in the terminal, and the command where
/// there is no key.
///
/// The launcher's menu says `Ctrl+C` for the copy; in a terminal that
/// key quits, and a menu that sends the user to the wrong key is a menu
/// that lies. The copy is the `>copy` command there.
///
/// The folder is the same kind of lie for a different reason. `Ctrl+Enter`
/// is not a key a terminal sends: in the raw mode a terminal puts the
/// keyboard in, Control+Enter arrives as the same carriage return as Enter
/// (some terminals offer a quoted form, and this backend does not decode
/// one), so a menu that advertised it would send the user to a key that
/// cannot arrive. The `>folder` command is how the terminal opens the
/// folder — and the handler for the modifier sequence stays, because a
/// backend that decoded one would not need the menu to change.
fn shortcut(action: LauncherAction) -> &'static str {
    match action {
        LauncherAction::Open => "Enter",
        LauncherAction::OpenFolder => ">folder",
        LauncherAction::CopyPath => ">copy",
        LauncherAction::Preview => "Ctrl+P",
        LauncherAction::Menu => "Ctrl+K",
        LauncherAction::Settings => ">settings",
    }
}

impl App {
    fn new(live: Live, root: String, indexed: usize) -> Self {
        let mut app = App {
            lang: Lang::English,
            // The index is this process's own, so the header says the name
            // of the program that owns it.
            brand: Brand::Index,
            // The entry points that name no theme keep the palette this
            // interface was drawn in.
            palette: Palette::of(Theme::MIDNIGHT),
            live: Some(live),
            root,
            indexed: Some(indexed),
            input: String::new(),
            caret: 0,
            results: Vec::new(),
            matched: Vec::new(),
            total: 0,
            filtered: false,
            selected: 0,
            top: 0,
            page: 1,
            painted: Vec::new(),
            painted_width: 0,
            meter: LatencyMeter::new(),
            source: None,
            notice: None,
        };
        app.search();
        app
    }

    /// An app that asks a source instead of the index in this process.
    fn with_source(source: Box<dyn rgy_launcher::Source>, root: String) -> Self {
        // The count is the source's: the daemon says how many entries its
        // index holds, and a source that cannot say leaves the header
        // saying that the count is not known. It is asked here and asked
        // again after every search, because a daemon that was not there
        // when the launcher started can be there by the first keystroke.
        let indexed = source.indexed();
        let mut app = App {
            lang: Lang::English,
            // The source is the daemon's, and this is the launcher's
            // terminal face: the header says the launcher's name, which
            // is the name the floating window's title carries too.
            brand: Brand::Launcher,
            // The palette is overwritten by the entry point that names a
            // theme; this one is the default the others keep.
            palette: Palette::of(Theme::MIDNIGHT),
            live: None,
            root,
            indexed,
            input: String::new(),
            caret: 0,
            results: Vec::new(),
            matched: Vec::new(),
            total: 0,
            filtered: false,
            selected: 0,
            top: 0,
            page: 1,
            painted: Vec::new(),
            painted_width: 0,
            meter: LatencyMeter::new(),
            source: Some(source),
            notice: None,
        };
        app.search();
        app
    }

    /// Re-scan the whole index for the current query and time it.
    ///
    /// Matching goes through [`Searcher`] — the same fzf-v1 + one-typo
    /// matcher `searchctl` runs — so the UI and the CLI cannot drift apart.
    fn search(&mut self) {
        // A command line is not a search: the source would look for the
        // literal text of the line — ">folder" — and answer nothing,
        // while the line is a command. The terminal takes it before the
        // source sees it, the same rule the window follows.
        if self.is_command() {
            self.filtered = false;
            return;
        }

        let started = Instant::now();
        self.filtered = !self.input.trim().is_empty();

        // A source answers the whole question: the paths, and the span
        // that earned each one. The question asks for the screens the
        // arrows can page through and not for the source's whole index:
        // the answer travels the wire and the terminal waits for it on the
        // thread that draws.
        let limit = self.request_limit();
        let question = self.input.clone();
        if let Some(source) = self.source.as_mut() {
            let answer = source.search(&question, limit);
            self.results = answer.paths;
            self.matched = answer.spans;
            self.total = self.results.len();
            self.selected = 0;
            self.top = 0;
            // The count is the source's, and the source can learn it: the
            // daemon answers with the size of its index, and the header
            // has to say it as soon as the daemon has said it.
            self.indexed = source.indexed();
            self.meter.record(started.elapsed().as_micros());
            return;
        }

        // The UI starts with an empty box and must show the world, not a
        // parse error. `Alternatives::parse("")` matches everything, but
        // listing every path through the index is cheaper than scoring.
        if !self.filtered {
            self.results.clear();
            self.matched.clear();
            let live = self
                .live
                .as_ref()
                .expect("the in-process path has an index");
            self.total = live.base.n.saturating_sub(1); // skip synthetic root
            let mut out = Vec::new();
            // Entry 0 is the synthetic root; the cap is on the range rather
            // than a counter checked inside the loop.
            for i in 1..live.base.n.min(MAX_RESULTS + 1) {
                live.base.path(i, &mut out);
                self.results.push(rgy_engine::console::spell(&out));
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
        let mut tokens: Vec<rgy_name_core::name_query::Token> = Vec::new();
        let live = self
            .live
            .as_ref()
            .expect("the in-process path has an index");
        if let Ok(alts) = Alternatives::parse(&self.input, &home) {
            let searcher = Searcher { live };
            hits = searcher.search_alternatives(&alts.alts, MAX_RESULTS);
            if let [one] = alts.alts.as_slice() {
                tokens = one.tokens.clone();
            }
        }

        let mut out = Vec::new();
        self.results = hits
            .iter()
            .map(|h| {
                if let Some(p) = h.over.clone() {
                    return rgy_engine::console::spell(&p);
                }
                live.base.path(h.idx as usize, &mut out);
                rgy_engine::console::spell(&out)
            })
            .collect();
        // The characters each result matched, so the row can show the span.
        // With more than one alternative a hit does not say which one it
        // came from, so the span would be a guess and it is left out.
        self.matched = if tokens.is_empty() {
            vec![Vec::new(); self.results.len()]
        } else {
            self.results
                .iter()
                .map(|p| {
                    let name = name_of(p);
                    rgy_name_core::name_query::matched_chars(name.as_bytes(), &tokens)
                })
                .collect()
        };
        self.total = self.results.len();
        self.selected = 0;
        self.top = 0;

        self.meter.record(started.elapsed().as_micros());
    }

    /// How many results one question asks the source for.
    ///
    /// The terminal draws a page of rows and the arrows walk the answer it
    /// was given, so a question asks for the pages they can reach. It is a
    /// limit and not a page: `PgDn` walks the answer, and the next
    /// keystroke asks again.
    ///
    /// The bound is the screen's: before the first frame the page is one
    /// row, and the launcher's first question is the empty one — the
    /// first frame then says how tall the list is, and every question the
    /// user types after it is asked at that height.
    fn request_limit(&self) -> usize {
        self.page
            .max(1)
            .saturating_mul(PAGE_WINDOW)
            .min(MAX_RESULTS)
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

    /// Whether the query is a command line.
    ///
    /// A command line is not a search: the daemon would look for the
    /// literal text of the line — ">folder" — and answer nothing, while
    /// the line is a command.
    fn is_command(&self) -> bool {
        rgy_launcher::commands::is_command(&self.input)
    }

    /// The action the command line names, if it names one.
    fn command(&self) -> Option<LauncherAction> {
        rgy_launcher::commands::command_for(&self.input).map(|c| c.action)
    }

    /// Do one of the launcher's actions to the selected result.
    ///
    /// `true` when the session is over: `Open` is the one action that
    /// ends it, because the terminal has no window to open a file in and
    /// prints the path instead — which is what `Enter` does too.
    fn run_action(&mut self, action: LauncherAction) -> bool {
        // The settings are nobody's row here either: the terminal has no
        // panel, and the file is where they are. The notice says the
        // true path, so the user can go and open it.
        if action == LauncherAction::Settings {
            let text = Text::for_lang(self.lang);
            let where_ = rgy_proto::endpoint::cache_dir()
                .map(|cache| {
                    let path = cache.join("rgy").join("settings.tsv");
                    path.to_string_lossy().into_owned()
                })
                .unwrap_or_else(|| "?".to_string());
            self.notice = Some(Notice {
                title: text.terminal_settings.to_string(),
                lines: vec![text.terminal_settings_file.replace("{}", &where_)],
            });
            return false;
        }
        // The menu is not an action **on** a row: it is the list of the
        // actions and the keys that do them, and it is the launcher's only
        // legend. Asked for a row it does not need, it answered "nothing
        // to act on" -- which is the one thing it can never be, and said it
        // exactly where the user goes to learn the keys: the empty box the
        // launcher opens with.
        if action == LauncherAction::Menu {
            self.notice = Some(Notice::menu(self.lang));
            return false;
        }
        let Some(path) = self.selected_path() else {
            // Nothing to act on is something to say: a command that does
            // nothing and says nothing is a launcher the user cannot
            // tell from a frozen one.
            self.notice = Some(Notice::nothing(self.lang));
            return false;
        };
        match action {
            LauncherAction::Open => true,
            LauncherAction::OpenFolder => {
                self.notice = Some(Notice::folder(&path, self.lang));
                false
            }
            LauncherAction::CopyPath => {
                self.notice = Some(Notice::copy(&path, self.lang));
                false
            }
            LauncherAction::Preview => {
                self.notice = Some(Notice::preview(&path, self.lang));
                false
            }
            LauncherAction::Menu => {
                // Taken above, before the row is asked for: the menu is a
                // legend and not an action on a row. The arm is here
                // because the match is on the whole action.
                self.notice = Some(Notice::menu(self.lang));
                false
            }
            // Taken above, before the row is asked for: the settings are
            // nobody's row. The arm is here because the match is on the
            // whole action, and a setting that somehow reached it answers
            // the way the rowless path above does.
            LauncherAction::Settings => false,
        }
    }

    /// Say that the line names a command that does not exist.
    fn say_unknown(&mut self) {
        self.notice = Some(Notice::unknown(&self.input, self.lang));
    }

    /// Whether a notice is on screen.
    fn has_notice(&self) -> bool {
        self.notice.is_some()
    }

    /// Take the notice away, if there is one.
    ///
    /// `Escape` closes the panel before it closes the session — the
    /// order the window uses.
    fn close_notice(&mut self) {
        self.notice = None;
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
            let mut screen = term::Screen::new(&mut *out);
            screen.move_to(0, 0)?;
            screen.clear_all()?;
            screen.flush()?;
            return self.write_line(
                out,
                0,
                MIN_WIDTH,
                &format!("terminal {width}x{height} is too small — need {MIN_WIDTH}x{MIN_HEIGHT}")
                    .with(self.palette.dim)
                    .to_string(),
            );
        }

        // Header, prompt, rule and footer leave four rows for the list.
        let list_height = height.saturating_sub(4);
        self.page = list_height;
        self.clamp_selection();

        let mut rows: Vec<String> = Vec::with_capacity(height);

        // The layout: brand in bold white, two spaces, then a dim
        // subtitle that carries the volume and the entry count.
        //
        // The count is the source's own: the daemon says how many entries
        // its index holds, and a source that cannot say leaves the header
        // saying that the count is not known. The sentence is not there
        // for the looks — a header that says "0" about an index of a
        // thousand entries is a wrong fact, and a wrong fact is worse
        // than no fact.
        let text = Text::for_lang(self.lang);
        let count = match self.indexed {
            Some(n) => text
                .terminal_files_and_folders
                .replace("{}", &thousands(n, self.lang)),
            None => text.terminal_count_unknown.to_string(),
        };
        let subtitle = format!(
            "{} · {}",
            self.root.as_str().with(self.palette.dim),
            count.as_str().with(self.palette.dim)
        );
        rows.push(format!(
            "{}  {}",
            self.brand.word(text).with(self.palette.ink).bold(),
            subtitle
        ));

        // The per-keystroke latency is the headline number. No label: the
        // video puts a green "8.1 ms" flush right of the prompt, with no word
        // before it.
        let latency = match self.meter.last() {
            Some(us) => duration(us).with(self.palette.good).to_string(),
            None => String::new(),
        };
        rows.push(self.prompt_line(width, &latency));
        rows.push("─".repeat(width).with(self.palette.dim).to_string());

        // The notice, when there is one, is a panel over the results:
        // the same place the window puts the preview and the menu.
        let notice = self.notice.as_ref().map(|n| n.rows(width, &self.palette));

        let first = self.top.min(self.results.len());
        let last = (first + list_height).min(self.results.len());

        // The name flows flush left, the parent path dimmed after a
        // two-space gap, with no column rule between them. Every row of the
        // list is present even when empty: a shrinking list would otherwise
        // leave the previous frame's entries on screen below the last match.
        //
        // What the empty list says depends on why it is empty. An empty box
        // is not a question — the window's rule, and the product's — so
        // "no matches" there is the answer to a question nobody asked; the
        // row says that there is nothing to type. A question that was asked
        // and found nothing says "no result".
        for offset in 0..list_height {
            let row = first + offset;
            rows.push(match &notice {
                Some(panel) => panel.get(offset).cloned().unwrap_or_default(),
                None => {
                    if self.results.is_empty() && offset == 0 {
                        let said = if self.input.trim().is_empty() && !self.is_command() {
                            text.terminal_type_to_search
                        } else {
                            text.no_result
                        };
                        format!("  {said}").with(self.palette.dim).to_string()
                    } else if row < last {
                        self.result_cell(row, width)
                    } else {
                        String::new()
                    }
                }
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
                let mut screen = term::Screen::new(&mut *out);
                screen.move_to(0, row as u16)?;
                screen.background(self.palette.bg)?;
                screen.clear_line()?;
                screen.print(cell)?;
                screen.flush()?;
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
        // readable rather than being overwritten or inverted. The roles are
        // the theme's now: the accent is the recording's blue, the ink its
        // white.
        let mut line = "❯".with(self.palette.accent).to_string();
        line.push(' ');
        line.push_str(&before.with(self.palette.ink).bold().to_string());
        line.push_str(&"▌".with(self.palette.accent).to_string());
        line.push_str(&after.with(self.palette.ink).bold().to_string());

        if !latency.is_empty() {
            let used = display_width(&line);
            line.push_str(&" ".repeat(width.saturating_sub(used + latency_width)));
            line.push_str(latency);
        }
        line
    }

    /// Each row is laid out as: white name, two spaces, dim parent.
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
            name = self.styled_name(row, name, name_visible),
            dir = elide_tail(&shorten_home(parent_of(path)), dir_width).with(self.palette.dim)
        );
        if row == self.selected {
            cell.reverse().to_string()
        } else {
            cell
        }
    }

    /// The name, with the characters the query matched in the matched
    /// colour and the rest in the ink.
    ///
    /// The matched characters come in runs, and a run is one styled piece:
    /// an escape sequence per character would cost the frame more than the
    /// search costs the keystroke.
    fn styled_name(&self, row: usize, name: &str, visible: usize) -> String {
        let matched = self.matched.get(row).map(|m| m.as_slice()).unwrap_or(&[]);
        if matched.is_empty() {
            return fit(name, visible).with(self.palette.ink).to_string();
        }
        let mut out = String::new();
        let mut run: Vec<char> = Vec::new();
        let mut run_matched = false;
        let mut chars = 0usize;
        for c in name.chars() {
            if chars >= visible {
                break;
            }
            let is_matched = matched.binary_search(&(chars as u32)).is_ok();
            if is_matched != run_matched && !run.is_empty() {
                out.push_str(&style_run(&run, run_matched, &self.palette));
                run.clear();
            }
            run_matched = is_matched;
            run.push(c);
            chars += 1;
        }
        if !run.is_empty() {
            out.push_str(&style_run(&run, run_matched, &self.palette));
        }
        out.extend(std::iter::repeat_n(' ', visible.saturating_sub(chars)));
        out
    }

    /// Footer: the prose, the chart, first/p50/p90. The video has no match
    /// count on the right edge — count info is already in the header.
    ///
    /// As the terminal narrows the prose is the first thing to go, then the
    /// chart; the percentile numbers stay. The cold first query is reported
    /// separately on purpose — it is the outlier the sparkline exists to
    /// show, and it must not sit inside p50/p90.
    fn footer_line(&self, width: usize) -> String {
        // The prose and the figures are the vocabulary's and the meter's,
        // and the terminal says them the way the window says them: one
        // table, one meter, two faces.
        let text = Text::for_lang(self.lang);
        // A source that cannot say how big its index is has not answered
        // yet, and the first thing the footer has to say is that there is
        // no daemon answering: a screen that says "no result" over a daemon
        // that is not there is the one lie the launcher cannot afford,
        // because the sentence the window shows under the field is this
        // one. The daemon's own words — the endpoint, and what the content
        // layer can do without it — would need a `warnings` accessor on the
        // `Source` trait, which is not this file.
        let prose = if self.source.is_some() && self.indexed.is_none() {
            text.no_daemon
        } else {
            text.terminal_prose_search
        };
        let stats = self.meter.stats(text).unwrap_or_default();

        let label_width = prose.chars().count();
        let stats_width = stats.chars().count();

        let mut line = String::new();
        let mut spark_width = 0usize;

        // Prose + sparkline + stats. As the budget shrinks the prose goes
        // first, then the chart; the numbers stay.
        if width >= label_width + stats_width + 4 {
            line.push_str(&prose.with(self.palette.dim).to_string());
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
                    .with(self.palette.good)
                    .to_string(),
            );
        }
        if !stats.is_empty() {
            if !line.is_empty() {
                line.push_str("  ");
            }
            line.push_str(&stats.with(self.palette.good).to_string());
        }

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
        let mut screen = term::Screen::new(out);
        screen.move_to(0, row)?;
        screen.background(self.palette.bg)?;
        screen.clear_line()?;
        screen.print(&truncate_visible(cell, width))?;
        screen.flush()
    }
}

/// Apply one key press, re-scanning only when the query text actually changed.
fn handle_key(app: &mut App, key: KeyEvent) -> Action {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    let action = match key.code {
        KeyCode::Esc => {
            // The panel goes before the session does: the same order the
            // window uses.
            if app.has_notice() {
                app.close_notice();
                Action::None
            } else {
                Action::Quit
            }
        }
        // The launcher's keys do the launcher's actions in the terminal
        // too: `Control+P` previews the file and `Control+K` opens the
        // actions. `Control+C` is the one that is not: in a terminal it
        // is how a user leaves, so it leaves this one — a key that opens
        // a panel the user cannot leave from is a trap, and the
        // terminal's copy is the `>copy` command.
        KeyCode::Char('c') if ctrl => Action::Quit,
        KeyCode::Char('p') if ctrl => Action::Run(LauncherAction::Preview),
        KeyCode::Char('k') if ctrl => Action::Run(LauncherAction::Menu),
        KeyCode::Enter if ctrl => Action::Run(LauncherAction::OpenFolder),
        KeyCode::Enter => {
            // A command line runs the command; anything else opens the
            // selected result.
            if app.is_command() {
                match app.command() {
                    Some(action) => Action::Run(action),
                    None => Action::Unknown,
                }
            } else {
                Action::Run(LauncherAction::Open)
            }
        }

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

    // No search here: this applies one key and says what it did. Whether to
    // re-run the search is the caller's to decide, because a burst of keys
    // wants exactly one search at the end of it (see [`run`]).
    app.clamp_selection();

    // An edit takes the notice away: the user is doing something else now,
    // and a panel that stays over the results is a panel about something
    // that is no longer true. It goes here and not after the burst, so that
    // a command typed and confirmed in one go keeps what it has to say.
    if action == Action::Edited {
        app.close_notice();
    }

    action
}

/// Whether the drain goes on after this event.
///
/// The drain reads the keys already queued behind the one it has, so that
/// a burst of typing costs one search and not one per letter. Windows
/// hands over a release for every press, and a drain that stopped at the
/// first release would stop at the first key — a drain that never fires,
/// and a burst of typing that is a burst of whole-index searches, each
/// one paid for in full and then thrown away.
///
/// A release carries no news for the query, so the drain goes on; a
/// resize does, because what is on screen no longer matches the terminal.
fn drain_goes_on(event: &term::Event) -> bool {
    matches!(event, term::Event::Key(_))
}

/// Run the interactive search. Enter prints the selected path to stdout after
/// the screen is restored, so the result can be piped onward.
pub fn run(live: Live, root: String, indexed: usize) -> Result<(), String> {
    run_app(App::new(live, root, indexed))
}

/// Run the interactive search, in the language the launcher speaks.
///
/// The language is the caller's: the terminal is the launcher's other
/// face, and a launcher that speaks two languages at once is a launcher
/// the user has to read twice.
pub fn run_with_lang(live: Live, root: String, indexed: usize, lang: Lang) -> Result<(), String> {
    let mut app = App::new(live, root, indexed);
    app.lang = lang;
    run_app(app)
}

/// Run the interactive search over a source.
///
/// The source is the daemon, or anything else that answers a query: the
/// same terminal UI over either, and the caller is the one that chooses.
/// This is the launcher's fallback — the same interface, in a terminal,
/// on a machine with no desktop.
pub fn run_with_source(source: Box<dyn rgy_launcher::Source>, root: String) -> Result<(), String> {
    run_app(App::with_source(source, root))
}

/// Run the interactive search over a source, in the language the launcher
/// speaks.
pub fn run_with_source_and_lang(
    source: Box<dyn rgy_launcher::Source>,
    root: String,
    lang: Lang,
) -> Result<(), String> {
    let mut app = App::with_source(source, root);
    app.lang = lang;
    run_app(app)
}

/// Run the interactive search over a source, in the language the launcher
/// speaks and the colours the caller named.
///
/// The theme is the caller's the way the language is: the terminal is one
/// face of the launcher and the theme is an answer every face gives, so
/// the palette a user chose follows the search from the window to the
/// terminal. The roles the terminal has no use for (`selected`, `border`)
/// are the theme's business and not this face's — reverse video stands in
/// for the selected band, and the terminal draws no border.
pub fn run_with_source_lang_and_theme(
    source: Box<dyn rgy_launcher::Source>,
    root: String,
    lang: Lang,
    theme: Theme,
) -> Result<(), String> {
    let mut app = App::with_source(source, root);
    app.lang = lang;
    app.palette = Palette::of(theme);
    run_app(app)
}

/// The runner, over an app that is already built.
fn run_app(mut app: App) -> Result<(), String> {
    // The console decides how the UTF-8 bytes are decoded, so it is put
    // into UTF-8 before the first frame is painted: a
    // name with an accent has to reach the screen as itself.
    rgy_engine::console::init();

    // Which view of an adapter-claimed file a search reads. rgy answers to
    // RGY_ADAPTER_VIEW and falls back to rgx's RGX_ADAPTER_VIEW, so a shell
    // configured for rgx is configured for rgy too.
    rgy_engine::adapter_view::apply();

    let mut out = rgy_engine::console::stdout();
    let _guard = TerminalGuard::enter(&mut out, app.palette.bg)?;

    let mut chosen = None;

    loop {
        let size = term::size().map_err(|e| format!("cannot read the terminal size: {e}"))?;
        app.draw(&mut out, size)
            .map_err(|e| format!("cannot draw: {e}"))?;

        // Read one key, then every key already queued behind it, and search
        // once for the lot. Without this a burst of typing is a burst of
        // whole-index searches -- one per letter -- even though only the state
        // after the last key is ever drawn. The terminal hands over keys far
        // faster than a large index can be searched, so the queue is where
        // that work piles up and then gets thrown away.
        let mut edited = false;
        let mut outcome = Outcome::None;
        let mut block = true;
        loop {
            if !block
                && !term::poll(std::time::Duration::ZERO)
                    .map_err(|e| format!("cannot poll the keyboard: {e}"))?
            {
                break;
            }
            block = false;
            let event = term::read().map_err(|e| format!("cannot read the keyboard: {e}"))?;
            // A key coming up is not a reason to stop the drain: the
            // console hands over a release for every press, and a drain
            // that stops at the first release stops at the first key --
            // a burst of typing would then be a burst of whole-index
            // searches, one per letter, each one paid for in full and
            // then thrown away.
            if !drain_goes_on(&event) {
                break;
            }
            match event {
                // Windows can report releases as well as presses; ignore
                // anything that is not a key going down.
                term::Event::Key(key) if key.kind == KeyEventKind::Press => {
                    match handle_key(&mut app, key) {
                        Action::Quit => {
                            outcome = Outcome::Quit;
                            break;
                        }
                        Action::Run(action) => {
                            // `Open` is the one action that ends the
                            // session: the terminal has no window to open
                            // a file in, so it prints the path — which is
                            // what `Enter` does too.
                            if app.run_action(action) {
                                outcome = Outcome::Chosen;
                                break;
                            }
                        }
                        Action::Unknown => app.say_unknown(),
                        Action::Edited => edited = true,
                        Action::None | Action::Moved => {}
                    }
                }
                // A key coming up carries no news for the query: the
                // console sends one for every press, and the drain goes
                // on.
                term::Event::Key(_) => {}
                // A resize and anything else want the next frame, not more
                // keys: what is on screen no longer matches the terminal.
                _ => break,
            }
        }
        if edited {
            app.search();
        }
        app.clamp_selection();

        match outcome {
            Outcome::Quit => break,
            Outcome::Chosen => {
                chosen = app.selected_path();
                break;
            }
            Outcome::None => {}
        }
    }

    drop(_guard);
    if let Some(path) = chosen {
        // The path goes to stdout so it can be piped onward: one path,
        // one line, and a path that carries a line break is not one line.
        if rgy_pick::carries_line(path.as_bytes()) {
            rgy_engine::outln!("{path}");
        } else {
            eprintln!("search-ui: {}", rgy_pick::uncarryable_paths(1));
        }
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
    // The home is a path on the platform the launcher runs on: a Windows
    // home ends in a backslash and a unix one in a slash. The terminal is
    // the unix face, and a shortening that only knows `\` shortens nothing
    // there -- the header then shows `/home/who` where the launcher's own
    // recording shows `~`.
    let home = home.trim_end_matches(['\\', '/']);
    if home.is_empty() {
        return path.to_string();
    }
    if path == home {
        return "~".to_string();
    }
    // Strip only at a separator boundary, so `C:\Users\me` never eats into
    // `C:\Users\meta` -- and `/home/me` never eats into `/home/meta`.
    match path.strip_prefix(home) {
        Some(rest) if rest.starts_with('\\') || rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_string(),
    }
}

/// The profile directory as a path prefix (`C:\Users\me`), if one is known.
fn home_dir() -> Option<String> {
    // `USERPROFILE` is the Windows home; `HOME` covers anything that sets it
    // instead. Both are absolute paths without a trailing separator.
    for key in ["USERPROFILE", "HOME"] {
        if let Ok(value) = std::env::var(key) {
            let trimmed = value.trim_end_matches(['\\', '/']);
            if trimmed.len() > 2 {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

/// A run of characters of a name, in the matched colour when the query
/// matched them and in the ink when it did not. The palette comes in
/// because the run is not the app's: it is a piece of a row, and the
/// colours are the frame's.
fn style_run(run: &[char], matched: bool, palette: &Palette) -> String {
    let text: String = run.iter().collect();
    if matched {
        text.with(palette.matched).to_string()
    } else {
        text.with(palette.ink).to_string()
    }
}

/// How many cells a character takes on the screen.
///
/// The East Asian Wide and Fullwidth characters take two cells: a CJK
/// ideograph, a kana, a Hangul syllable, an emoji. The non-spacing marks
/// take none: a decomposed name is one character on the screen, not two.
/// Everything else takes one.
///
/// The Ambiguous characters are counted as one cell. The tree draws its rule
/// and its sparkline with them (`─`, `▁`..`█`), and a terminal that gave
/// them two would draw the rule twice as wide as the window it belongs to.
/// The Western width is the one the tree's own layout assumes.
fn cell_width(c: char) -> usize {
    let cp = c as u32;
    if is_combining(cp) {
        0
    } else if is_wide(cp) {
        2
    } else {
        1
    }
}

/// The cells a piece of text takes on the screen.
fn cells(s: &str) -> usize {
    s.chars().map(cell_width).sum()
}

/// Whether the character takes two cells: the East Asian Wide and Fullwidth
/// blocks.
fn is_wide(cp: u32) -> bool {
    matches!(
        cp,
        0x1100..=0x115F   // Hangul Jamo
        | 0x2E80..=0x303E // CJK radicals, Kangxi, CJK symbols
        | 0x3041..=0x33FF // kana, Hangul compatibility jamo, CJK compatibility
        | 0x3400..=0x4DBF // CJK unified ideographs extension A
        | 0x4E00..=0x9FFF // CJK unified ideographs
        | 0xA000..=0xA4CF // Yi
        | 0xAC00..=0xD7A3 // Hangul syllables
        | 0xF900..=0xFAFF // CJK compatibility ideographs
        | 0xFE10..=0xFE19 // vertical forms
        | 0xFE30..=0xFE6F // CJK compatibility forms, small form variants
        | 0xFF00..=0xFF60 // fullwidth forms
        | 0xFFE0..=0xFFE6 // fullwidth signs
        | 0x1F300..=0x1F64F // emoji
        | 0x1F900..=0x1F9FF // supplemental symbols and pictographs
        | 0x20000..=0x3FFFD // CJK unified ideographs extension B and beyond
    )
}

/// Whether the character is a non-spacing mark: it takes no cell of its own.
///
/// The marks of the scripts a file name is likely to carry. A mark of a
/// rarer script is counted as one cell, which makes the line one cell too
/// wide rather than one too narrow.
fn is_combining(cp: u32) -> bool {
    matches!(
        cp,
        0x0300..=0x036F // combining diacritical marks: the decomposed Latin a macOS name carries
        | 0x0483..=0x0489 // combining Cyrillic
        | 0x0591..=0x05BD // Hebrew points
        | 0x05BF
        | 0x05C1..=0x05C2
        | 0x05C4..=0x05C5
        | 0x05C7
        | 0x0610..=0x061A // Arabic signs
        | 0x064B..=0x065F // Arabic marks
        | 0x0670
        | 0x06D6..=0x06DC
        | 0x0900..=0x0902 // Devanagari
        | 0x093A..=0x093C
        | 0x093E..=0x094F
        | 0x0951..=0x0957
        | 0x0962..=0x0963
        | 0x0E31 // Thai
        | 0x0E34..=0x0E3A
        | 0x0E47..=0x0E4E
        | 0x20D0..=0x20F0 // combining marks for symbols
        | 0x3099..=0x309A // combining katakana marks
        | 0xFE00..=0xFE0F // variation selectors
        | 0xFE20..=0xFE2F // combining half marks
    )
}

/// Pad `s` to exactly `width` cells, trimming from the left when it is longer.
fn fit(s: &str, width: usize) -> String {
    let len = cells(s);
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

/// Break a line into pieces that fit the width.
///
/// A path is longer than a terminal often, and a path cut in half is a
/// path the user cannot use: a word that does not fit on a line of its
/// own is broken, and the rest of the line is broken on its spaces.
///
/// The width is in cells, so a wide word is broken sooner than a narrow
/// one, and the break is on a character boundary: half a character is not a
/// character.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut line_cells = 0usize;
    for word in text.split(' ') {
        let word_cells = cells(word);
        let fits = line_cells + 1 + word_cells <= width;
        if !line.is_empty() && fits {
            line.push(' ');
            line.push_str(word);
            line_cells += 1 + word_cells;
            continue;
        }
        if !line.is_empty() {
            out.push(std::mem::take(&mut line));
        }
        let mut rest = word;
        while cells(rest) > width {
            let cut = cut_at_cells(rest, width);
            out.push(rest[..cut].to_string());
            rest = &rest[cut..];
        }
        line = rest.to_string();
        line_cells = cells(rest);
    }
    if !line.is_empty() {
        out.push(line);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Where `s` stops filling `width` cells, as a byte offset.
///
/// The cut is on a character boundary: a wide character that would need two
/// cells and has one left goes whole to the next piece, because half a
/// character is not a character.
fn cut_at_cells(s: &str, width: usize) -> usize {
    let mut used = 0usize;
    for (i, c) in s.char_indices() {
        let w = cell_width(c);
        if used + w > width {
            // A character too wide for the whole line is still taken, so
            // the cut always makes progress.
            return if i == 0 { c.len_utf8() } else { i };
        }
        used += w;
    }
    s.len()
}

/// The tail of `s` that fills at most `width` cells, cut on a character
/// boundary.
fn tail_at_cells(s: &str, width: usize) -> &str {
    let mut used = 0usize;
    let mut start = s.len();
    for (i, c) in s.char_indices().rev() {
        let w = cell_width(c);
        if used + w > width {
            break;
        }
        used += w;
        start = i;
    }
    &s[start..]
}

/// Shorten to `width` cells keeping the tail. Paths are read from the end, so
/// the directory that gave a file its meaning must survive the cut.
fn elide_tail(s: &str, width: usize) -> String {
    if cells(s) <= width {
        return s.to_string();
    }
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        return "…".to_string();
    }
    // The ellipsis takes a cell of its own, and the tail is what is left.
    format!("…{}", tail_at_cells(s, width - 1))
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
///
/// The width is in cells, not characters: a CJK ideograph is two cells
/// wide, so a line measured in characters is a line twice as wide as the
/// terminal it was drawn for.
fn display_width(s: &str) -> usize {
    cells(&strip_ansi(s))
}

/// Clip a styled string to `width` visible cells, keeping its escape
/// sequences so styling survives the cut.
///
/// Width is counted on the text, not the escapes: measuring the raw character
/// count would make every coloured cell look far too long and clip the content
/// the caller had just laid out.
///
/// A wide character that would need two cells and has one left is dropped
/// whole: half a character is not a character, and a character drawn across
/// the edge of the terminal is a character the terminal cannot show.
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
        let w = cell_width(c);
        if visible + w > width {
            out.push_str(term::RESET);
            return out;
        }
        out.push(c);
        visible += w;
    }
    out
}

/// Group digits in threes: 5151234 -> "5,151,234".
///
/// The separator is the language's: English groups with a comma and
/// Portuguese with a dot. A number spelled the other way round is a
/// number the reader has to read twice.
fn thousands(n: usize, lang: Lang) -> String {
    let separator = match lang {
        Lang::English => ',',
        Lang::Portuguese => '.',
    };
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(separator);
        }
        out.push(c);
    }
    out
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
    use rgy_discovery::walk::{KIND_DIR, KIND_FILE, Listing, NONE, RawEnt};

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
        let idx = rgy_name_core::name_index::Index::build(sample_listings(), 0, 0, b"");
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

    /// Applying a key edits the query; recomputing the matches is the
    /// caller's call. They have to be separate steps for a burst of typing to
    /// cost one search instead of one per letter, which is what `run` does.
    #[test]
    fn applying_a_key_does_not_research() {
        let mut app = app_new();
        let listed = app.results.len();
        assert!(listed > 1, "the fixture must start with something listed");

        let key = KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE);
        assert_eq!(handle_key(&mut app, key), Action::Edited);
        assert_eq!(app.input, "m", "the key must be applied");
        assert_eq!(
            app.results.len(),
            listed,
            "the results must not move until the caller searches; otherwise \
             every key in a burst pays for a whole search"
        );
    }

    /// A key coming up does not end the drain.
    ///
    /// The console hands over a release for every press, and the drain
    /// reads the queue until it is empty, so that a burst of typing costs
    /// one search. A drain that stopped at the first release would stop at
    /// the first key — so the burst would be a search per letter, each one
    /// paid for in full and then thrown away, which is the whole thing the
    /// drain is there to avoid.
    #[test]
    fn a_key_coming_up_does_not_end_the_drain() {
        let press = term::Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        let mut release = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert!(drain_goes_on(&press), "a press keeps the drain going");
        assert!(
            drain_goes_on(&term::Event::Key(release)),
            "and a release does too: the console sends one for every press"
        );
        assert!(
            !drain_goes_on(&term::Event::Resize(80, 24)),
            "a resize ends it: the frame on screen is the old one"
        );
    }

    /// The width is in cells, and not in characters.
    ///
    /// A CJK ideograph takes two cells and a combining mark takes none, so
    /// a row measured in characters is a row twice as wide as the terminal
    /// it was drawn for: the terminal wraps it, and the frame that was one
    /// line becomes two. A name with an accent in it is the other way
    /// round — one cell for two characters — and a row measured in
    /// characters is then a row too narrow.
    #[test]
    fn the_width_is_in_cells_and_not_in_characters() {
        // Two ideographs: two characters, four cells.
        let wide = "\u{4e00}\u{4e00}";
        assert_eq!(wide.chars().count(), 2, "the fixture is two characters");
        assert_eq!(cells(wide), 4, "and four cells");

        // A decomposed name: two characters, one cell.
        let decomposed = "e\u{0301}";
        assert_eq!(
            decomposed.chars().count(),
            2,
            "the fixture is two characters"
        );
        assert_eq!(cells(decomposed), 1, "and one cell");

        // The row is the width, in cells.
        assert_eq!(
            cells(&fit(wide, 4)),
            4,
            "a wide name fills the row in cells"
        );
        assert_eq!(
            cells(&fit(wide, 3)),
            3,
            "and a name that does not fit is cut to it"
        );
        assert_eq!(
            cells(&fit(decomposed, 3)),
            3,
            "a decomposed name fills it too"
        );

        // The cut is on a character boundary: half a character is not a
        // character, and a character drawn across the edge of the
        // terminal is a character the terminal cannot show.
        let cut = fit(wide, 3);
        assert!(
            !cut.contains('\u{4e00}') || cut.chars().all(|c| c == '\u{4e00}' || c == '…'),
            "the cut is whole characters: {cut:?}"
        );
        assert_eq!(cells(&cut), 3, "and the row is still the width");
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

    /// MIDNIGHT is the palette this interface was drawn in, and both of its
    /// spellings are pinned: the RGB values for a terminal that honours
    /// 24-bit colour, and the ANSI-16 fallbacks for one that cannot.
    ///
    /// The resolution is forced per branch because the environment decides
    /// which one the app runs in — once, before the first test — so a test
    /// that only read [`Palette::of`] would pin whichever branch this
    /// host's environment happens to give.
    #[test]
    fn midnight_is_the_palette_the_interface_was_drawn_in() {
        // The detection is read once and is stable across calls: the
        // environment does not change under a program.
        assert_eq!(palette::truecolor(), palette::truecolor());

        let truecolor = Palette::resolved(Theme::MIDNIGHT, true);
        assert_eq!(
            truecolor.bg,
            Color::Rgb {
                r: 19,
                g: 18,
                b: 21
            }
        );
        assert_eq!(
            truecolor.ink,
            Color::Rgb {
                r: 255,
                g: 255,
                b: 255
            }
        );
        assert_eq!(
            truecolor.dim,
            Color::Rgb {
                r: 115,
                g: 114,
                b: 119
            }
        );
        assert_eq!(
            truecolor.accent,
            Color::Rgb {
                r: 114,
                g: 170,
                b: 255
            }
        );
        assert_eq!(
            truecolor.matched,
            Color::Rgb {
                r: 233,
                g: 184,
                b: 92
            }
        );
        assert_eq!(
            truecolor.good,
            Color::Rgb {
                r: 119,
                g: 217,
                b: 139
            }
        );

        // Without a truecolor advertisement the ANSI-16 fallback must be
        // used, otherwise the Windows console quantizes RGB to something
        // that just looks like the old colours on a black background. No
        // Rgb may leak through when the terminal cannot show it — and the
        // fallbacks are the hues of the theme at the 16-colour palette's
        // saturation.
        let ansi = Palette::resolved(Theme::MIDNIGHT, false);
        assert_eq!(ansi.bg, Color::Black);
        assert_eq!(ansi.ink, Color::White);
        assert_eq!(ansi.dim, Color::DarkGrey);
        assert_eq!(ansi.accent, Color::Blue);
        assert_eq!(ansi.matched, Color::Yellow);
        assert_eq!(ansi.good, Color::Green);
    }

    /// A different theme changes the bytes a frame writes.
    ///
    /// The palette is a value now: the colours come from the theme the
    /// caller named, and a frame drawn in LIGHT is not a frame drawn in
    /// MIDNIGHT. The truecolor resolution is forced on because the escape
    /// parameters are where the difference is visible — without 24-bit
    /// colour both themes fall back to the same ANSI-16 roles, which is the
    /// fallback doing its job and not a missing difference.
    #[test]
    fn a_different_theme_changes_the_bytes_a_frame_writes() {
        let mut midnight = app_with("maintenance");
        midnight.palette = Palette::resolved(Theme::MIDNIGHT, true);
        let mut midnight_frame = Vec::new();
        midnight.draw(&mut midnight_frame, FRAME).expect("draws");

        let mut light = app_with("maintenance");
        light.palette = Palette::resolved(Theme::LIGHT, true);
        let mut light_frame = Vec::new();
        light.draw(&mut light_frame, FRAME).expect("draws");

        assert_ne!(
            midnight_frame, light_frame,
            "the theme has to reach the bytes the terminal reads"
        );
        let said = String::from_utf8_lossy(&light_frame);
        // LIGHT's ink is a dark grey (40, 40, 42), and MIDNIGHT's white
        // (255, 255, 255) is not what a light frame says.
        assert!(said.contains("38;2;40;40;42"), "{said:?}");
        assert!(
            !said.contains("38;2;255;255;255"),
            "the default's ink is in the light frame: {said:?}"
        );
    }

    /// A keystroke asks for the screens the arrows can page through, not
    /// for every path the source holds.
    ///
    /// The number is the launcher's half of the cost of a keystroke: the
    /// daemon pays for the answer on the wire, the terminal pays for it on
    /// the thread that draws, and both are measured on the page the user
    /// can see. Measured against the daemon on a 971,675-entry index on the
    /// CachyOS box: twenty thousand hits took 1,255 ms of one keystroke.
    #[test]
    fn a_keystroke_asks_for_the_screens_the_arrows_can_page() {
        let recorder = Recorder::default();
        let mut app = App::with_source(Box::new(recorder.clone()), "/tmp".to_string());
        // The first frame is drawn before the user types, and it is what
        // says how tall the list is.
        let _ = frame(&mut app, 100, 30);
        let page = app.page;
        assert!(page > 1, "the frame has a page of rows");
        for ch in "invoice".chars() {
            app.insert(ch);
        }
        app.search();

        let limits = recorder.limits();
        let asked = *limits.last().expect("the source was asked");
        assert_eq!(asked, page * PAGE_WINDOW, "the paging window");
        assert!(
            asked < MAX_RESULTS,
            "the answer is a window and not the world: {asked}"
        );
        // Before the first frame there is one row, so the launcher's own
        // first question — the empty one — asks for a window of eight.
        assert_eq!(limits[0], PAGE_WINDOW, "the first question: {limits:?}");
    }

    /// The rows on the screen are drawn from the answer it was given, and
    /// the answer is a window: the extra pages are still scrollable, which
    /// is what `PgDn` walks.
    #[test]
    fn the_answer_is_more_than_one_page_and_still_finite() {
        let mut app = app_new();
        let _ = frame(&mut app, 100, 30);
        assert_eq!(app.page, 26);
        assert_eq!(app.request_limit(), 26 * PAGE_WINDOW);
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
            shorten_home_within("C:\\Users\\dana\\Developer\\Tools\\Search", home),
            "~\\Developer\\Tools\\Search"
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

    /// The terminal is the unix face, and the home there ends in `/`: a
    /// shortening that only knew a backslash shortened nothing on the
    /// machine the terminal interface is for.
    #[test]
    fn home_collapses_to_a_tilde_on_unix_too() {
        let home = "/home/dana";
        assert_eq!(shorten_home_within(home, home), "~");
        assert_eq!(
            shorten_home_within("/home/dana/Documents/notes.txt", home),
            "~/Documents/notes.txt"
        );
        // The same boundary rule, on the separator the unix paths carry.
        assert_eq!(
            shorten_home_within("/home/dana2/Notes", home),
            "/home/dana2/Notes"
        );
        assert_eq!(
            shorten_home_within("/var/log/syslog", home),
            "/var/log/syslog"
        );
        // A home spelled with its trailing separator is the same home.
        assert_eq!(shorten_home_within("/home/dana/x", "/home/dana/"), "~/x");
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
        assert_eq!(thousands(0, Lang::English), "0");
        assert_eq!(thousands(999, Lang::English), "999");
        assert_eq!(thousands(1_000, Lang::English), "1,000");
        assert_eq!(thousands(5_151_234, Lang::English), "5,151,234");
        // The separator is the language's: a number spelled the other way
        // round is a number the reader has to read twice.
        assert_eq!(thousands(1_226, Lang::Portuguese), "1.226");
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
        let line = app.prompt_line(60, &"3.5 ms".with(app.palette.good).to_string());
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
        let line = app.prompt_line(30, &"2.0 ms".with(app.palette.good).to_string());
        assert_eq!(display_width(&line), 30, "never exceeds the terminal width");
    }

    /// Walk a rendered frame and pair every painted row with the text a user sees.
    ///
    /// The terminal backend writes each row as a cursor move, a clear and the
    /// styled text, so the cursor moves are the row boundaries and everything
    /// else is either styling to strip or payload to keep.
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
        assert!(rows[3].1.contains("no result"), "{}", rows[3].1);
        // The footer carries the prose, the chart, and `p50 N ms` — no
        // count. The "0 matches" assertion was a holdover from the count
        // line that used to sit on the right.
    }

    /// An empty box is not a question, so the empty list is not "no
    /// matches": it says there is nothing typed yet. The words are the
    /// vocabulary's, and the two languages say them.
    #[test]
    fn an_empty_box_says_there_is_nothing_typed_yet() {
        for (lang, said) in [
            (Lang::English, "type to search"),
            (Lang::Portuguese, "escreva para procurar"),
        ] {
            // The launcher's face is the daemon's: a source whose index is
            // not in this process answers the empty query with nothing, and
            // the empty box says there is nothing typed yet. (The terminal
            // search UI with an index of its own lists what it already
            // holds, and the row is one of those entries.)
            let mut app = App::with_source(Box::new(Recorder::default()), "/tmp".to_string());
            app.lang = lang;
            let rows = frame(&mut app, 60, 10);
            assert!(
                rows[3].1.contains(said),
                "{lang:?}: the empty box says {:?}",
                rows[3].1
            );
            assert!(
                !rows[3].1.contains("no result"),
                "{lang:?}: an empty box is not an empty answer: {:?}",
                rows[3].1
            );
        }
    }

    /// The footer says the daemon is not answering, in the language of the
    /// launcher: the same sentence the window shows under the field.
    #[test]
    fn a_source_that_cannot_say_its_count_says_the_daemon_is_not_there() {
        for (lang, said) in [
            (Lang::English, "no connection to the daemon"),
            (Lang::Portuguese, "sem ligação ao daemon"),
        ] {
            let recorder = Recorder::default();
            let mut app = App::with_source(Box::new(recorder), "/tmp".to_string());
            app.lang = lang;
            let footer = strip_ansi(&app.footer_line(100));
            assert!(footer.contains(said), "{lang:?}: {footer}");

            // A source that says how big its index is has answered, and
            // the footer says the launcher's own sentence again.
            let recorder = Recorder::with_count(7);
            let mut app = App::with_source(Box::new(recorder), "/tmp".to_string());
            app.lang = lang;
            let footer = strip_ansi(&app.footer_line(100));
            assert!(!footer.contains(said), "{lang:?}: {footer}");
        }
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
        // The name is painted in two pieces when the query matched part of
        // it, so the row is read without the colours.
        let text = strip_ansi(&cell);
        assert!(text.contains("maintenance.rs"), "{text}");
        assert!(text.contains("/src"), "{text}");
    }

    /// The row knows which characters of the name the query matched, so it
    /// can paint the span that earned the score.
    #[test]
    fn the_result_row_knows_which_characters_matched() {
        let app = app_with("maintenance");
        assert!(!app.matched.is_empty(), "the positions are there");
        assert!(
            app.matched[0].contains(&0),
            "the first character matched: {:?}",
            app.matched[0]
        );
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

    /// A source that remembers what it was asked.
    ///
    /// The terminal's contract with the daemon is that a command line
    /// never reaches it: the daemon would look for the literal text of
    /// the line and answer nothing, and a test that cannot see the
    /// questions cannot keep that honest.
    #[derive(Clone, Default)]
    struct Recorder {
        queries: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
        /// How many results each question asked for.
        ///
        /// The number is the launcher's half of the cost of a keystroke:
        /// the daemon pays for it on the wire and the terminal pays for it
        /// on the thread that draws.
        limits: std::rc::Rc<std::cell::RefCell<Vec<usize>>>,
        /// What the source says about the size of its index.
        ///
        /// `None` is the default: a source that cannot say does not have
        /// to, and the header that shows it has to cope with the not
        /// knowing. The cell is shared so a test can let the source learn
        /// the count later — a daemon that was not there when the
        /// launcher started can be there by the first keystroke.
        indexed: std::rc::Rc<std::cell::Cell<Option<usize>>>,
    }

    impl Recorder {
        fn asked(&self) -> Vec<String> {
            self.queries.borrow().clone()
        }

        /// The limit each question asked for, in order.
        fn limits(&self) -> Vec<usize> {
            self.limits.borrow().clone()
        }

        /// A source that says how many entries its index holds.
        fn with_count(count: usize) -> Self {
            Recorder {
                indexed: std::rc::Rc::new(std::cell::Cell::new(Some(count))),
                ..Recorder::default()
            }
        }
    }

    impl rgy_launcher::Source for Recorder {
        fn search(&mut self, query: &str, limit: usize) -> rgy_launcher::Answer {
            self.queries.borrow_mut().push(query.to_string());
            self.limits.borrow_mut().push(limit);
            rgy_launcher::Answer::empty()
        }

        fn indexed(&self) -> Option<usize> {
            self.indexed.get()
        }
    }

    /// The header row off a screen.
    ///
    /// The header is the first row, and a frame is a difference — so the
    /// screen is what a run of frames leaves, and the row is read from it.
    fn header_row(screen: &Screen) -> String {
        screen
            .rows()
            .into_iter()
            .find(|(row, _)| *row == 1)
            .map(|(_, text)| text)
            .expect("the header is the first row")
    }

    /// The terminal face of the launcher says the launcher's name.
    ///
    /// The floating window's title says "rgxy", and the terminal stands in
    /// for that window where there is no desktop: two faces of the same
    /// launcher that carry two names are a launcher the user has to learn
    /// twice.
    ///
    /// The tool that keeps an index in its own process says its own name
    /// instead — it is a different program, and it is the one whose index
    /// the header counts.
    #[test]
    fn the_launchers_terminal_face_says_the_launchers_name() {
        let mut app = App::with_source(Box::new(Recorder::default()), "C:\\".to_string());
        let mut screen = Screen::default();
        screen.apply(&frame(&mut app, 100, 10));
        let header = header_row(&screen);
        assert!(header.contains("rgxy"), "{header}");
        assert!(!header.contains("zen-rs"), "{header}");

        // And the tool that owns the index says its own.
        let mut app = app_new();
        let mut screen = Screen::default();
        screen.apply(&frame(&mut app, 100, 10));
        let header = header_row(&screen);
        assert!(header.contains("zen-rs"), "{header}");
        assert!(!header.contains("rgxy"), "{header}");
    }

    /// The header says the count the source gives.
    ///
    /// The daemon knows how many entries its index holds, and the header
    /// is where the user reads it: a header that says "0" under an index
    /// of a thousand entries is a wrong fact, and nobody reads a number
    /// looking for a lie.
    #[test]
    fn the_header_says_the_count_the_source_gives() {
        let mut app = App::with_source(Box::new(Recorder::with_count(1226)), "C:\\".to_string());
        let mut screen = Screen::default();
        screen.apply(&frame(&mut app, 100, 10));
        let header = header_row(&screen);
        assert!(header.contains("1,226 files and folders"), "{header}");

        // And the number is spelled the language's way.
        app.lang = Lang::Portuguese;
        let mut screen = Screen::default();
        screen.apply(&frame(&mut app, 100, 10));
        let header = header_row(&screen);
        assert!(header.contains("1.226 ficheiros e pastas"), "{header}");
    }

    /// A source that cannot say how big its index is does not say zero.
    ///
    /// Zero is a fact — "there is nothing here" — and a source that does
    /// not know the size of its index must not be read as an empty one.
    /// The sentence that says the count is not known is better than a
    /// number that is not true.
    #[test]
    fn a_source_that_cannot_say_does_not_say_zero() {
        let mut app = App::with_source(Box::new(Recorder::default()), "C:\\".to_string());
        let mut screen = Screen::default();
        screen.apply(&frame(&mut app, 100, 10));
        let header = header_row(&screen);
        assert!(header.contains("count not known"), "{header}");
        assert!(!header.contains("0 files and folders"), "{header}");
    }

    /// The header says the count as soon as the source knows it.
    ///
    /// A daemon that is not there when the launcher starts can be there by
    /// the first keystroke, and the header has to learn the count then: a
    /// header that keeps saying "not known" under a daemon that has
    /// answered is a header that is wrong the other way.
    #[test]
    fn the_header_follows_the_source_when_it_learns_the_count() {
        let source = Recorder::default();
        let mut app = App::with_source(Box::new(source.clone()), "C:\\".to_string());
        let mut screen = Screen::default();
        screen.apply(&frame(&mut app, 100, 10));
        assert!(
            header_row(&screen).contains("count not known"),
            "{}",
            header_row(&screen)
        );

        // The daemon answered: it says how many entries its index holds.
        source.indexed.set(Some(1226));
        app.search();
        screen.apply(&frame(&mut app, 100, 10));
        assert!(
            header_row(&screen).contains("1,226 files and folders"),
            "{}",
            header_row(&screen)
        );
    }

    /// A command line is not a question for the daemon.
    ///
    /// The daemon would look for the literal text of the line — ">folder"
    /// — and answer nothing, while the line is a command. The terminal
    /// takes it before the daemon sees it.
    #[test]
    fn a_command_line_never_reaches_the_daemon() {
        let recorder = Recorder::default();
        let mut app = App::with_source(Box::new(recorder.clone()), "C:\\".to_string());
        for ch in ">folder".chars() {
            app.insert(ch);
        }
        app.search();

        let asked = recorder.asked();
        assert!(
            !asked.iter().any(|q| q.starts_with('>')),
            "the daemon was asked for the command line: {asked:?}"
        );
    }

    /// The query is still a query: the command line did not break the
    /// search that came before it.
    #[test]
    fn a_plain_query_is_still_asked_of_the_daemon() {
        let recorder = Recorder::default();
        let mut app = App::with_source(Box::new(recorder.clone()), "C:\\".to_string());
        for ch in "invoice".chars() {
            app.insert(ch);
        }
        app.search();

        let asked = recorder.asked();
        assert!(
            asked.iter().any(|q| q == "invoice"),
            "the daemon was not asked the question: {asked:?}"
        );
    }

    #[test]
    fn a_command_line_names_the_action_it_means() {
        let mut app = app_new();
        for ch in ">folder".chars() {
            app.insert(ch);
        }
        assert!(app.is_command());
        assert_eq!(app.command(), Some(LauncherAction::OpenFolder));

        // And a word without the prefix is a search, not a command.
        let mut app = app_new();
        for ch in "folder".chars() {
            app.insert(ch);
        }
        assert!(!app.is_command());
        assert_eq!(app.command(), None);
    }

    /// The launcher's keys do the launcher's actions in the terminal too
    /// — the ones that are not the way out.
    #[test]
    fn the_launcher_keys_that_are_not_control_c_do_their_actions() {
        let mut app = app_new();
        let ctrl = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        assert_eq!(
            handle_key(&mut app, ctrl('p')),
            Action::Run(LauncherAction::Preview)
        );
        assert_eq!(
            handle_key(&mut app, ctrl('k')),
            Action::Run(LauncherAction::Menu)
        );
        assert_eq!(
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL)
            ),
            Action::Run(LauncherAction::OpenFolder)
        );
    }

    /// `Control+C` is how a user quits a terminal, so it quits this one.
    ///
    /// A key that opens a panel the user cannot leave from is a trap:
    /// the user presses it to leave and stays.
    #[test]
    fn control_c_quits_the_terminal() {
        let mut app = app_new();
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(handle_key(&mut app, ctrl_c), Action::Quit);
    }

    /// The copy is the `>copy` command in the terminal: it is how the
    /// path is copied now that `Control+C` quits.
    #[test]
    fn the_copy_is_the_command_in_the_terminal() {
        let mut app = app_new();
        for ch in ">copy".chars() {
            let key = KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE);
            assert_eq!(handle_key(&mut app, key), Action::Edited);
        }
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            handle_key(&mut app, enter),
            Action::Run(LauncherAction::CopyPath)
        );
    }

    /// The terminal's menu does not offer the key that quits: a menu
    /// that sends the user to the wrong key is a menu that lies.
    #[test]
    fn the_terminal_menu_does_not_offer_the_key_that_quits() {
        let mut app = app_new();
        assert!(!app.run_action(LauncherAction::Menu));

        let notice = app.notice.as_ref().expect("the notice is there");
        let text = notice.lines.join("\n");
        assert!(text.contains(">copy"), "{text}");
        assert!(!text.contains("Ctrl+C"), "{text}");
    }

    /// The list still moves with the keys it moved with: the arrows and
    /// `Control+N`.
    #[test]
    fn the_list_still_moves_with_control_n_and_the_arrows() {
        let mut app = app_with("main");
        let first = app.selected;
        let ctrl_n = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL);
        assert_eq!(handle_key(&mut app, ctrl_n), Action::Moved);
        assert_eq!(app.selected, first + 1);

        let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(handle_key(&mut app, up), Action::Moved);
        assert_eq!(app.selected, first);
    }

    /// The terminal owns no clipboard, so it says so instead of pretending
    /// that it copied anything.
    #[test]
    fn the_copy_command_prints_the_path_and_says_it_has_no_clipboard() {
        let mut app = app_with("main");
        let selected = app.selected_path().expect("a result is selected");
        assert!(!app.run_action(LauncherAction::CopyPath));

        let notice = app.notice.as_ref().expect("the notice is there");
        let text = notice.lines.join("\n");
        assert!(text.contains(&selected), "{text}");
        assert!(text.contains("clipboard"), "{text}");
    }

    /// The terminal's menu does not send the user to a key the terminal
    /// cannot send: in the raw mode a terminal puts the keyboard in,
    /// Control+Enter arrives as the same carriage return as Enter, so the
    /// folder is the `>folder` command there.
    #[test]
    fn the_terminal_menu_does_not_advertise_control_enter() {
        let mut app = app_new();
        assert!(!app.run_action(LauncherAction::Menu));
        let notice = app.notice.as_ref().expect("the notice is there");
        let text = notice.lines.join("\n");
        assert!(!text.contains("Ctrl+Enter"), "{text}");
        assert!(text.contains(">folder"), "{text}");
    }

    /// A command with nothing to act on says so: it does not do nothing,
    /// and it does not search for the literal text of the line.
    #[test]
    fn a_command_with_nothing_to_act_on_says_so() {
        let mut app = app_with("zzzz");
        assert!(app.results.is_empty());
        assert!(!app.run_action(LauncherAction::CopyPath));

        let notice = app.notice.as_ref().expect("the notice is there");
        assert!(
            notice.lines.join("\n").contains("no result"),
            "{:?}",
            notice.lines
        );
    }

    /// The notices are the vocabulary's, like every other sentence: a
    /// Portuguese launcher says its panels in Portuguese.
    #[test]
    fn the_notices_speak_the_launchers_language() {
        let mut app = app_with("main");
        app.lang = Lang::Portuguese;

        assert!(!app.run_action(LauncherAction::CopyPath));
        let notice = app.notice.as_ref().expect("the notice is there");
        assert_eq!(notice.title, "copiar", "{}", notice.title);
        assert!(
            notice.lines.join("\n").contains("área de transferência"),
            "{:?}",
            notice.lines
        );

        assert!(!app.run_action(LauncherAction::OpenFolder));
        let notice = app.notice.as_ref().expect("the notice is there");
        assert_eq!(notice.title, "pasta", "{}", notice.title);
        assert!(
            notice.lines.join("\n").contains("não abre uma pasta"),
            "{:?}",
            notice.lines
        );

        assert!(!app.run_action(LauncherAction::Preview));
        let notice = app.notice.as_ref().expect("the notice is there");
        assert!(
            notice.title.starts_with("pré-visualização — "),
            "{}",
            notice.title
        );

        // And the rowless notice: the words are the table's.
        let mut app = app_with("zzzz");
        app.lang = Lang::Portuguese;
        assert!(!app.run_action(LauncherAction::CopyPath));
        let notice = app.notice.as_ref().expect("the notice is there");
        assert_eq!(notice.title, "nada sobre o que agir", "{}", notice.title);
        assert!(
            notice
                .lines
                .join("\n")
                .contains("nenhum resultado selecionado"),
            "{:?}",
            notice.lines
        );
    }

    #[test]
    fn the_menu_command_lists_the_actions_with_their_keys() {
        let mut app = app_new();
        assert!(!app.run_action(LauncherAction::Menu));

        let notice = app.notice.as_ref().expect("the notice is there");
        let text = notice.lines.join("\n");
        for command in rgy_launcher::COMMANDS {
            assert!(text.contains(&format!(">{}", command.name)), "{text}");
        }
        assert!(text.contains("Ctrl+P"), "{text}");
    }

    /// The menu's two columns are a table: the command, a space, the key,
    /// and then what it does.
    ///
    /// `>settings` is the longest command and it used to eat the space
    /// between the columns: the row read `>settings>settings`, a command
    /// nobody can type, beside the key that does not do it.
    #[test]
    fn the_menu_rows_have_a_space_between_the_command_and_its_key() {
        let mut app = app_new();
        assert!(!app.run_action(LauncherAction::Menu));

        let notice = app.notice.as_ref().expect("the notice is there");
        assert_eq!(notice.lines.len(), rgy_launcher::COMMANDS.len());
        for (line, command) in notice.lines.iter().zip(rgy_launcher::COMMANDS.iter()) {
            let name = format!(">{}", command.name);
            assert!(line.starts_with(&name), "{line}");
            let rest = &line[name.len()..];
            assert!(
                rest.starts_with(' '),
                "the command and its key ran together: {line:?}"
            );
            let key = shortcut(command.action);
            assert!(
                rest.trim_start().starts_with(key),
                "the key column does not carry {key}: {line:?}"
            );
        }
    }

    /// The menu is the launcher's legend, and it opens where the legend is
    /// wanted: the empty box the launcher starts with, before there is any
    /// row to act on. It acts on no row, so "nothing to act on" is the one
    /// answer it cannot give.
    #[test]
    fn the_menu_opens_with_no_result_selected() {
        let mut app = app_with("zzzz");
        assert!(app.results.is_empty());
        assert!(!app.run_action(LauncherAction::Menu));
        let notice = app.notice.as_ref().expect("the menu is there");
        assert_eq!(notice.title, Text::for_lang(Lang::English).notice_actions);
        assert!(notice.lines.join("\n").contains(">copy"));

        // And the title is the language's, like every other word the two
        // faces say.
        let mut app = app_with("zzzz");
        app.lang = Lang::Portuguese;
        assert!(!app.run_action(LauncherAction::Menu));
        let notice = app.notice.as_ref().expect("the menu is there");
        assert_eq!(
            notice.title,
            Text::for_lang(Lang::Portuguese).notice_actions
        );
    }

    #[test]
    fn the_preview_command_shows_the_head_of_the_file() {
        let path =
            std::env::temp_dir().join(format!("rgy-search-ui-notas-{}.txt", std::process::id()));
        std::fs::write(&path, "first line\nsegunda linha\n").expect("the file is written");
        let mut app = app_new();
        app.results = vec![path.to_string_lossy().into_owned()];
        assert!(!app.run_action(LauncherAction::Preview));
        let _ = std::fs::remove_file(&path);

        let notice = app.notice.as_ref().expect("the notice is there");
        let text = notice.lines.join("\n");
        assert!(text.contains("first line"), "{text}");
    }

    /// The notice is drawn where the results were: the panel is over the
    /// list, the way the window's preview is over the window's results.
    #[test]
    fn the_notice_is_drawn_where_the_results_were() {
        let mut app = app_with("main");
        app.run_action(LauncherAction::CopyPath);
        let rows = frame(&mut app, 60, 10);
        let text: String = rows.iter().map(|(_, t)| t.as_str()).collect();
        assert!(text.contains("clipboard"), "{text}");
    }

    /// `Escape` closes the panel before it closes the session — the
    /// order the window uses.
    #[test]
    fn escape_closes_the_notice_before_the_session() {
        let mut app = app_with("main");
        app.run_action(LauncherAction::CopyPath);
        assert!(app.has_notice());

        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(handle_key(&mut app, esc), Action::None);
        assert!(!app.has_notice());
        assert_eq!(handle_key(&mut app, esc), Action::Quit);
    }

    /// An edit takes the notice away: the user is doing something else
    /// now, and the panel was about the last thing.
    #[test]
    fn an_edit_takes_the_notice_away() {
        let mut app = app_with("main");
        app.run_action(LauncherAction::CopyPath);
        assert!(app.has_notice());

        let key = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(handle_key(&mut app, key), Action::Edited);
        assert!(!app.has_notice());
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
