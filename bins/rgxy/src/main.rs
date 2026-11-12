// SPDX-License-Identifier: Apache-2.0

//! rgxy — the launcher that sits on top of the rgy search daemon.
//!
//! It is a **client**. The index lives in the daemon, the daemon keeps it
//! warm, and a keystroke is a question sent down a pipe. That is the
//! whole reason the window is there in a few milliseconds: it is not
//! building an index, it is asking one that is already built.
//!
//! # What it is
//!
//! | | |
//! |---|---|
//! | window | a floating overlay, over whatever is on screen |
//! | hotkey | Control twice — a low-level hook, because Control alone is not a hotkey the platform will register |
//! | keys | everything: arrows, Enter, Control+Enter, Escape, and the actions |
//! | filters | `file:` `folder:` `doc:` `pic:` `video:` `audio:` |
//! | commands | `>open` `>folder` `>copy` `>preview` `>menu` |
//! | content | `grep:` — the second layer, over the files the first layer chose |
//!
//! # Where it runs
//!
//! The window needs a desktop. A machine without one gets the terminal
//! interface instead, over the same source: the same query box, the same
//! list, the same answers — in a terminal.

mod daemon;
// The language the launcher speaks: read once, at start-up, and handed
// down to everything that has a word to say.
mod lang;
// The memory of what the user used. The window is the only thing that
// records a use; on a host with neither window the module is compiled —
// and tested — with nothing calling it yet.
#[cfg_attr(
    not(any(windows, target_os = "linux", target_os = "freebsd")),
    allow(dead_code)
)]
mod mru;
#[cfg(any(windows, target_os = "linux", target_os = "freebsd"))]
mod preview;
// The name the window driver knows the platform by.
mod platform;
mod rgx;
mod tools;
// The machine behind the terminal interface's actions: the same open,
// open-folder, clipboard and settings the window uses, reached through
// the platform backend where there is one.
mod tui_actions;
// The help text, in the language the launcher is speaking.
mod usage;

#[cfg(any(windows, target_os = "linux", target_os = "freebsd"))]
mod gui;
// The settings file: the one place the launcher's preferences live,
// written by the panel and edited by hand.
//
// The file is read on every platform — the terminal interface and the
// picker take their colours and their arguments from it — and only the
// panel writes it, and the panel is a window. So on the platforms without
// one the writer and the helpers it needs are compiled and never called,
// and the lint that says so is silenced the way the tree says it: with the
// reason next to it.
#[cfg_attr(
    not(any(windows, target_os = "linux", target_os = "freebsd")),
    allow(dead_code)
)]
mod settings_store;

use std::io::{IsTerminal, Write};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use rgy_launcher::text::{Lang, Text};
use rgy_launcher::{Answer, Source};
use rgy_proto::endpoint::Endpoint;

use daemon::DaemonSource;

/// What the command line said.
#[derive(Debug, Clone)]
pub struct Options {
    /// Run the terminal interface instead of the window.
    pub tui: bool,
    /// One search, printed, and out.
    pub query: Option<String>,
    /// How many results to ask for.
    pub limit: usize,
    /// Where the daemon listens.
    pub endpoint: Option<String>,
    /// Whether the global hotkey is installed.
    pub hook: bool,
    /// Whether the window opens by itself.
    pub show: bool,
    /// Hand the results to a picker and print what the user chose.
    pub pick: bool,
    /// The picker to run.
    pub fzf_binary: Option<String>,
    /// One argument for the picker, repeatable.
    pub fzf_arg: Vec<String>,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            tui: false,
            query: None,
            limit: 50,
            endpoint: None,
            hook: true,
            show: false,
            pick: false,
            fzf_binary: None,
            fzf_arg: Vec::new(),
        }
    }
}

/// The usage text.
///
/// The text is in the language the launcher is speaking: the help is the
/// program's own description of itself, and the user reads it in the
/// language the user reads.
fn usage(lang: Lang) -> String {
    usage::text(lang)
}

fn main() -> ExitCode {
    // The language is read once, here: the launcher speaks one language
    // for the whole of its life, and a launcher that changed its language
    // in the middle of a sentence would say two things at once.
    let lang = lang::from_env();

    let argv: Vec<String> = std::env::args().skip(1).collect();

    // A resident launch gives the console it was started with back,
    // before anything has been said on the streams: the library reads
    // each standard handle once, and a write to a handle whose console
    // has been given back is a panic. The launcher's face is the
    // notification area, and the console under it would be a window the
    // user has to close by hand. The guard is the call's own: a console
    // somebody else's — a terminal's — stays where it is.
    #[cfg(windows)]
    if resident_launch(&argv, lang) {
        // The answer is nobody's to read: the console is either gone or
        // shared with a terminal, and the streams are about to point at
        // the null device. The reason was read where the call was, and
        // there is nobody here to hear what it said.
        let _ = rgy_launcher_windows::put_the_console_away();
    }

    // The console decides how the bytes are decoded, and a path with an
    // accent has to reach the screen as itself.
    rgy_engine::console::init();

    match run(&argv, lang) {
        Ok(code) => code,
        Err(reason) => {
            eprintln!("rgxy: {reason}");
            ExitCode::from(1)
        }
    }
}

/// What the command line asked for.
#[derive(Debug)]
enum Request {
    /// The text of the help.
    Help,
    /// The version.
    Version,
    /// The launcher, with the options the command line gave.
    Run(Options),
}

/// Read the command line.
///
/// An **empty** command line is the launcher: the resident window with
/// the hotkey, hidden until the hotkey says otherwise. The usage text
/// is what `--help` asks for — the manual, the README and the login
/// entry all say that a bare `rgxy` is the launcher, and a program that
/// answers a question nobody asked is a program that says what it is
/// not.
fn request(argv: &[String], lang: Lang) -> Result<Request, String> {
    match argv.first().map(String::as_str) {
        Some("-h") | Some("--help") | Some("help") => Ok(Request::Help),
        Some("-V") | Some("--version") => Ok(Request::Version),
        _ => Ok(Request::Run(parse(argv, lang)?)),
    }
}

/// Whether the command line asks for the resident launcher.
///
/// The resident launch is the one whose face is the window and the
/// notification area — not the text modes, which answer on the console
/// they were given and keep it. A command line that does not read at
/// all is not a resident launch either: a refusal has to be said on the
/// console it refused on.
///
/// The question is portable and the answer is Windows-only: the call
/// that puts the console away is a Windows thing, and the function is
/// called from there. The test is portable and runs everywhere, so the
/// lint is silenced with the reason next to it.
#[cfg_attr(not(windows), allow(dead_code))]
fn resident_launch(argv: &[String], lang: Lang) -> bool {
    match request(argv, lang) {
        Ok(Request::Run(options)) => options.query.is_none() && !options.tui && !options.pick,
        _ => false,
    }
}

/// Run the command. `Err` is a sentence for stderr and exit code 1.
fn run(argv: &[String], lang: Lang) -> Result<ExitCode, String> {
    let options = match request(argv, lang)? {
        Request::Help => {
            println!("{}", usage(lang));
            return Ok(ExitCode::SUCCESS);
        }
        Request::Version => {
            println!("rgxy {}", env!("CARGO_PKG_VERSION"));
            return Ok(ExitCode::SUCCESS);
        }
        Request::Run(options) => options,
    };
    if options.pick && options.query.is_none() {
        // A picker filters the results of a search: without a question
        // there is no list to filter.
        return Err(Text::for_lang(lang).pick_needs_a_query.to_string());
    }
    let endpoint = match &options.endpoint {
        Some(text) => endpoint_of(text, lang)?,
        None => Endpoint::registered()
            .ok_or_else(|| Text::for_lang(lang).daemon_address_unknown.to_string())?,
    };
    let rgx = tools::rgx();

    if let Some(query) = &options.query {
        // The command line waits as long as the client always did.
        let source = command_source(endpoint, rgx, lang);
        if options.pick {
            return pick(source, query, options.limit, &options, lang);
        }
        return one_shot(source, query, options.limit, lang);
    }
    if options.tui {
        return run_tui(DaemonSource::new(endpoint, rgx, lang), None, lang);
    }
    run_window(endpoint, rgx, &options, lang)
}

/// The source for the command line.
///
/// The window is the launcher's and the command line is the client's:
/// a search box that stops answering the keyboard is broken, while a
/// command line waiting for a cold daemon may wait as long as the
/// client always did.
fn command_source(endpoint: Endpoint, rgx: Option<String>, lang: Lang) -> DaemonSource {
    DaemonSource::with_window(endpoint, rgx, rgy_proto::client::read_timeout(), lang)
}

/// Read the options.
fn parse(argv: &[String], lang: Lang) -> Result<Options, String> {
    let text = Text::for_lang(lang);
    let mut options = Options::default();
    let mut index = 0;
    while index < argv.len() {
        match argv[index].as_str() {
            "--tui" => options.tui = true,
            "--no-hook" => options.hook = false,
            "--show" => options.show = true,
            "--pick" => options.pick = true,
            "--fzf-binary" => {
                index += 1;
                options.fzf_binary =
                    Some(argv.get(index).ok_or(text.fzf_binary_needs_a_path)?.clone());
            }
            "--fzf-arg" => {
                index += 1;
                options.fzf_arg.push(
                    argv.get(index)
                        .ok_or(text.fzf_arg_needs_an_argument)?
                        .clone(),
                );
            }
            "--query" => {
                index += 1;
                options.query = Some(argv.get(index).ok_or(text.query_needs_text)?.clone());
            }
            "--limit" => {
                index += 1;
                let value = argv.get(index).ok_or(text.limit_needs_a_number)?;
                options.limit = value
                    .parse::<usize>()
                    .map_err(|_| text.limit_is_not_a_number.replace("{value}", value))?;
            }
            "--endpoint" => {
                index += 1;
                options.endpoint = Some(
                    argv.get(index)
                        .ok_or(text.endpoint_needs_an_address)?
                        .clone(),
                );
            }
            other => {
                let reason = text.unknown_option.replace("{other}", other);
                return Err(format!("{reason}\n\n{}", usage(lang)));
            }
        }
        index += 1;
    }
    Ok(options)
}

/// The endpoint the command line named.
fn endpoint_of(text: &str, lang: Lang) -> Result<Endpoint, String> {
    if let Some(path) = text.strip_prefix("uds:") {
        return Ok(Endpoint::Socket(std::path::PathBuf::from(path)));
    }
    if let Some(name) = text.strip_prefix("pipe:") {
        return Ok(Endpoint::Pipe(name.to_string()));
    }
    if text == "stdio" {
        return Ok(Endpoint::Stdio);
    }
    Err(Text::for_lang(lang)
        .unknown_endpoint
        .replace("{text}", text))
}

/// One search, printed, and out.
///
/// The exit status is the search-shaped one: 0 for a match, 1 for none.
/// The *reason* a search has no answer goes to stderr, because "no
/// results" and "the daemon is not there" are different answers, and an
/// exit status that cannot tell them apart makes the second one look like
/// the first.
fn one_shot(
    mut source: DaemonSource,
    query: &str,
    limit: usize,
    lang: Lang,
) -> Result<ExitCode, String> {
    let answer = wait_for_daemon(&mut source, query, limit);
    let mut out = std::io::stdout();
    let written = write_paths(&answer.paths, &mut out);
    for warning in source.status().borrow().warnings.clone() {
        eprintln!("rgxy: {warning}");
    }
    if let Err(err) = written {
        // The paths did not reach the caller: the status says the
        // output did not arrive, and it does not say success for
        // output that never arrived.
        return Ok(ExitCode::from(write_failure(&err, lang)));
    }
    Ok(if answer.is_empty() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// The paths, one per line.
///
/// The line is one path, and a path that carries a line break cannot be
/// one line: it is left out, and the launcher says on stderr how many
/// were left out. Half of a path is a path that does not exist, and a
/// reader that cannot tell the halves apart opens nothing.
///
/// The write that fails is the failure: a path that did not reach the
/// caller is a path the caller does not have, and a program that says
/// it printed them anyway is a program that lies. The loop stops at
/// the first failure — the reader is gone, and the rest of the paths
/// would go into the same gone.
fn write_paths(paths: &[String], out: &mut impl Write) -> std::io::Result<()> {
    // The line format: one path per line, and the paths that cannot be
    // one line are left out. The count is said before the first write,
    // so a reader that goes away in the middle does not take the
    // sentence with it.
    let dropped = paths
        .iter()
        .filter(|path| !rgy_pick::carries_line(path.as_bytes()))
        .count();
    if dropped > 0 {
        // A path that is missing from the list is a path the user has to
        // be told about.
        eprintln!("rgxy: {}", rgy_pick::uncarryable_paths(dropped));
    }
    for path in paths {
        if !rgy_pick::carries_line(path.as_bytes()) {
            // The path cannot be one line, and half of it is a path that
            // does not exist.
            continue;
        }
        writeln!(out, "{path}")?;
    }
    out.flush()
}

/// The exit status of an output that never arrived.
///
/// The launcher has three answers: 0 is a search with matches, 1 is a
/// search without them, and this is the third — the search had its
/// answer and the answer did not reach the caller.
const OUTPUT_NOT_DELIVERED: u8 = 2;

/// The exit status of a write that did not go through.
///
/// A **broken pipe** is the reader going away on purpose
/// (`rgxy --query invoice | head -3`), and it is not a sentence on
/// stderr: there is nobody there to read it. It is not success
/// either: a status of 0 would say the paths were printed, and they
/// were not — so the status is the third one, the one that says the
/// output did not arrive.
///
/// Any other failure is the machine's own, and the user hears about
/// it: a write that fails for a reason nobody knows about is a
/// failure nobody can do anything about.
fn write_failure(err: &std::io::Error, lang: Lang) -> u8 {
    if err.kind() != std::io::ErrorKind::BrokenPipe {
        eprintln!(
            "rgxy: {}",
            Text::for_lang(lang)
                .the_write_failed
                .replace("{err}", &err.to_string())
        );
    }
    OUTPUT_NOT_DELIVERED
}

/// The results, through a picker.
///
/// The picker is the user's own — `fzf`, `sk`, `peco` or `fz` — and so
/// are its arguments: each `--fzf-arg` is one argument of the picker,
/// and nothing the user wrote is re-split by a shell.
///
/// The records are the paths, separated by NUL: a path may carry a tab
/// or a newline on unix, and a separator a path can carry is a separator
/// that breaks. The order is rgy's — the picker narrows the list, it
/// does not re-score it, because the ranking is the product.
///
/// The separator follows the picker: `fzf` and `sk` read a NUL-separated
/// list, and a picker that does not know `--read0` — `peco`, `fz` — gets
/// the list one path per line. The picker crate does the translating, so
/// the records here are written once, in NUL.
///
/// The exit status is the picker's own, mapped the way `searchctl` maps
/// it: 0 for a choice, 1 for no choice (the user dismissed the picker,
/// or there was nothing to choose), and 2 for a picker that could not
/// run — which is not a search with no matches, and says so. The choice
/// that does not reach stdout is 2 as well: the output did not arrive,
/// and that is not a choice either.
fn pick(
    mut source: DaemonSource,
    query: &str,
    limit: usize,
    options: &Options,
    lang: Lang,
) -> Result<ExitCode, String> {
    let text = Text::for_lang(lang);
    if !std::io::stdout().is_terminal() {
        // A picker is interactive: without a terminal there is nothing
        // for the user to choose with, and pretending otherwise would
        // print a path nobody chose.
        return Err(text.a_picker_needs_a_terminal.to_string());
    }
    let answer = wait_for_daemon(&mut source, query, limit);
    for warning in source.status().borrow().warnings.clone() {
        eprintln!("rgxy: {warning}");
    }
    if answer.is_empty() {
        // There is nothing to pick, and the picker is not asked.
        eprintln!("rgxy: {}", text.no_result);
        return Ok(ExitCode::from(1));
    }

    // The picker's colours and its free arguments are the settings
    // file's, and the command line is the last word: a file that says
    // `--height 80%` and a caller that says `--height 40%` gets the
    // caller's, because the command line is the one the user typed. A
    // file that cannot be read is the defaults — the picker still runs,
    // in the colours the faces were drawn in before settings existed.
    let settings = settings_store::store_path()
        .and_then(|path| settings_store::load(&path).ok())
        .unwrap_or_default();
    let args = picker_args(&settings, options);
    let req = rgy_pick::Request {
        args: &args,
        binary: options.fzf_binary.as_deref(),
        separator: rgy_pick::Separator::Nul,
        order: rgy_pick::Order::Keep,
    };
    let buffer = records(&answer.paths);
    let search = Box::new(
        move |out: &mut (dyn Write + Send + 'static)| -> Result<bool, String> {
            out.write_all(&buffer).map_err(|e| {
                text.the_results_did_not_arrive
                    .replace("{e}", &e.to_string())
            })?;
            Ok(!buffer.is_empty())
        },
    );
    let (code, chosen) = match rgy_pick::pick_records(search, &req) {
        Ok(picked) => picked,
        Err(reason) => {
            eprintln!("rgxy: {reason}");
            return Ok(ExitCode::from(2));
        }
    };

    // The choice, one path per line: the same contract as `--query`, so
    // a script can read either.
    let mut out = std::io::stdout();
    if let Err(err) = print_selection(&chosen, &mut out) {
        // The choice did not reach the caller: the status says the
        // output did not arrive, and it does not say success for
        // output that never arrived.
        return Ok(ExitCode::from(write_failure(&err, lang)));
    }
    Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)))
}

/// The arguments the picker is invoked with, in the order they win.
///
/// The theme's colours first — and only when the picker is `fzf`,
/// because `--color` is its own vocabulary and a flag a picker does not
/// understand is a picker that refuses to run — then the settings
/// file's free arguments, then the command line's. The last word is the
/// one the user typed, and the picker reads a repeated flag the way
/// every program does: the last one.
fn picker_args(settings: &rgy_launcher::settings::Settings, options: &Options) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    if rgy_pick::takes_colour_flags(options.fzf_binary.as_deref()) {
        args.extend(rgy_launcher::settings::fzf_colour_args(
            &settings.fzf.palette.theme(rgy_launcher::Theme::WARM),
        ));
    }
    args.extend(settings.fzf.args.iter().cloned());
    args.extend(options.fzf_arg.iter().cloned());
    args
}

/// The records rgy writes: the paths, separated by NUL.
///
/// A path may carry a tab or a newline on unix, and a separator a path
/// can carry is a separator that breaks: the NUL byte cannot appear in
/// a path anywhere. A picker that cannot read a NUL-separated list gets
/// them one path per line, and the picker crate does the translating.
fn records(paths: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    for path in paths {
        out.extend_from_slice(path.as_bytes());
        out.push(0);
    }
    out
}

/// Print what the picker chose, one path per line.
///
/// The line format is the one of `--query`: a script that reads either
/// reads them the same way.
///
/// The write that fails is the failure: the choice the user made did
/// not reach the caller, and a status of 0 would say it did.
fn print_selection(chosen: &[Vec<u8>], out: &mut impl Write) -> std::io::Result<()> {
    // The same line format as `--query`: the choice is one path per
    // line, and a choice that cannot be one line is left out.
    let dropped = chosen
        .iter()
        .filter(|record| !rgy_pick::carries_line(record))
        .count();
    if dropped > 0 {
        eprintln!("rgxy: {}", rgy_pick::uncarryable_paths(dropped));
    }
    for record in chosen {
        if !rgy_pick::carries_line(record) {
            continue;
        }
        out.write_all(record)?;
        out.write_all(b"\n")?;
    }
    out.flush()
}

/// How long a one-shot query waits for a daemon that is still starting.
///
/// The daemon opens its pipe only when the index is ready, so a daemon
/// that is a second into its startup has nothing to answer on yet. The
/// window can wait — the next keystroke is a new question — but a one-shot
/// query has exactly one chance, so it waits here.
const DAEMON_WAIT: Duration = Duration::from_secs(2);

/// Ask, and wait a while for a daemon that is not there yet.
///
/// The wait is bounded and the reason is still said out loud at the end:
/// a launcher that waited forever for a daemon that is never coming would
/// be a launcher that hangs.
fn wait_for_daemon(source: &mut DaemonSource, query: &str, limit: usize) -> Answer {
    let deadline = Instant::now() + DAEMON_WAIT;
    let window = source.window();
    loop {
        // The wait is bounded by the deadline AND by the searches it
        // starts: a search that begins with a second left and takes two
        // answers after the wait is over. So the daemon gets what is left
        // of the wait, and never more.
        let left = deadline.saturating_duration_since(Instant::now());
        source.set_window(left.min(window));
        let answer = source.search(query, limit);
        source.set_window(window);
        let connected = source.status().borrow().connected;
        // An answer is an answer, whatever produced it: the `rgx` fallback
        // answers the content layer with the daemon away.
        if connected || !answer.is_empty() || Instant::now() >= deadline {
            return answer;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The terminal interface, over the same source.
///
/// A launcher with no terminal has nowhere to draw: a login entry on a
/// machine with no session, a pipe, a service. There is nothing to say
/// and nowhere to say it, so it says nothing — and it says it by leaving,
/// not by failing, because a login entry that fails on every login is a
/// login entry the user learns to ignore.
///
/// The reason, when there is one, is the reason the window was not
/// chosen: the terminal is the fallback, and the fallback says what it
/// is standing in for.
fn run_tui(mut source: DaemonSource, reason: Option<&str>, lang: Lang) -> Result<ExitCode, String> {
    let terminal = std::io::stdout().is_terminal();
    for notice in notices(&mut source, reason, terminal) {
        eprintln!("rgxy: {notice}");
    }
    if !terminal {
        return Ok(ExitCode::SUCCESS);
    }
    let root = home_dir();
    // The terminal is the launcher's other face: it says the same words in
    // the same language, and a launcher that speaks two languages at once
    // is one the user has to read twice. Its colours are the settings
    // file's — a theme chosen for the terminal in the settings is the
    // one it comes up in — and a file that cannot be read is the palette
    // the interface was drawn in before settings existed.
    let theme = settings_store::store_path()
        .and_then(|path| settings_store::load(&path).ok())
        .map(|settings| settings.tui.theme(rgy_launcher::Theme::MIDNIGHT))
        .unwrap_or(rgy_launcher::Theme::MIDNIGHT);
    // The terminal gets the same machine the window has: opening a result,
    // opening its folder, the clipboard and the settings file are carried
    // out, not just described. Without this the terminal is a launcher that
    // names its options and then declines them.
    let runner = tui_actions::TuiActions::new(lang);
    rgy_search_ui::run_with_source_lang_theme_and_runner(
        Box::new(source),
        root,
        lang,
        theme,
        Box::new(runner),
    )
    .map(|_| ExitCode::SUCCESS)
}

/// The sentences the terminal prints before the interface takes the screen.
///
/// The terminal interface is the same interface over the same source,
/// and it must not be the one that keeps quiet: the sentence the window
/// shows under the field is the sentence the terminal prints — where the
/// daemon is not there, and what the daemon said about itself when it
/// is there but slow.
///
/// A launcher with no terminal has nowhere to draw: a login entry on a
/// machine with no session, a pipe, a service. There is nothing to say
/// and nowhere to say it, so it says nothing — and it says it by
/// leaving, not by failing, because a login entry that fails on every
/// login is a login entry the user learns to ignore.
fn notices(source: &mut DaemonSource, reason: Option<&str>, terminal: bool) -> Vec<String> {
    if !terminal {
        return Vec::new();
    }
    let mut notices = Vec::new();
    if let Some(reason) = reason {
        notices.push(reason.to_string());
    }
    // The daemon is asked who it is: the answer says whether there is a
    // pack store, and the interface shows it.
    notices.extend(source.ask_the_daemon());
    notices
}

/// The window, where there is a desktop to put it on.
///
/// One body for Windows and Linux: the two backends export the same
/// interface, and the name they are reached by is
/// [`platform::backend`]. The single-instance claim, the settings read
/// and the window driver are the same on both — only the crate that owns
/// the window differs.
#[cfg(any(windows, target_os = "linux", target_os = "freebsd"))]
fn run_window(
    endpoint: Endpoint,
    rgx: Option<String>,
    options: &Options,
    lang: Lang,
) -> Result<ExitCode, String> {
    use crate::platform::backend;

    if !backend::session_available() {
        // No desktop: the terminal is the fallback, and it says so.
        return run_tui(
            DaemonSource::new(endpoint, rgx, lang),
            Some(Text::for_lang(lang).no_graphical_screen),
            lang,
        );
    }
    // One launcher per session. The resident launcher holds a global
    // keyboard hook (or an X11 passive grab), an icon in the notification
    // area (or an XEmbed tray icon) and a topmost window, and a second
    // `rgxy` is a second one of each. The claim is taken **before**
    // anything is built, and a start that finds it taken asks the
    // launcher that is there to show itself and leaves.
    let (_claim, claim_note) = match backend::Sole::take() {
        backend::Taken::First(sole) => (Some(sole), None),
        backend::Taken::Other => {
            // The window belongs to the launcher that is running, and the
            // start that arrives while it is still being built waits for
            // it: the claim is taken before the window exists.
            backend::sole::ask_the_running_one_to_show();
            return Ok(ExitCode::SUCCESS);
        }
        backend::Taken::Unknown(reason) => {
            // The platform would not say whether another launcher is
            // there. The launcher runs anyway, and the reason goes to the
            // line under the field, where the launcher says what is true
            // of the machine it is on. On Windows it does not go to
            // stderr: a resident launch gave its console back.
            (None, Some(reason))
        }
    };
    // The settings are the file's, read once, here: the panel writes it,
    // the next start reads it, and a file that cannot be read is the
    // defaults plus a sentence on the line under the field.
    let (settings, settings_note) = match settings_store::store_path() {
        Some(path) => match settings_store::load(&path) {
            Ok(settings) => (settings, None),
            Err(err) => (
                rgy_launcher::settings::Settings::default(),
                Some(
                    Text::for_lang(lang)
                        .settings_not_read
                        .replace("{err}", &err.to_string()),
                ),
            ),
        },
        None => (rgy_launcher::settings::Settings::default(), None),
    };
    // The launcher's own window: the window lives on the thread that
    // draws it, and it cannot wait the client's thirty seconds.
    let settings_note = match (claim_note, settings_note) {
        (Some(claim), Some(settings)) => Some(format!("{claim}; {settings}")),
        (Some(claim), None) => Some(claim),
        (None, settings) => settings,
    };
    gui::run(
        DaemonSource::new(endpoint, rgx, lang),
        options,
        &settings,
        settings_note,
    )
    .map(|_| ExitCode::SUCCESS)
}

/// The window, where there is a desktop to put it on.
///
/// The floating window is a Windows, a Linux and a FreeBSD thing: the
/// last two share the X11 backend. Only macOS (and a host with no known
/// backend) takes the terminal interface here, and it is the same
/// interface over the same source. The double-Control hotkey is a
/// low-level keyboard hook on Windows and an X11 passive grab on Linux
/// and FreeBSD; neither exists here.
#[cfg(not(any(windows, target_os = "linux", target_os = "freebsd")))]
fn run_window(
    endpoint: Endpoint,
    rgx: Option<String>,
    _options: &Options,
    lang: Lang,
) -> Result<ExitCode, String> {
    run_tui(
        DaemonSource::new(endpoint, rgx, lang),
        Some(Text::for_lang(lang).no_window_backend),
        lang,
    )
}

/// The user's home directory, for the header of the terminal interface.
fn home_dir() -> String {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_endpoints_are_the_daemons_own_spelling() {
        assert!(matches!(
            endpoint_of("stdio", Lang::English),
            Ok(Endpoint::Stdio)
        ));
        assert!(matches!(
            endpoint_of("pipe:abc", Lang::English),
            Ok(Endpoint::Pipe(_))
        ));
        assert!(matches!(
            endpoint_of("uds:/tmp/x.sock", Lang::English),
            Ok(Endpoint::Socket(_))
        ));
        assert!(endpoint_of("http://x", Lang::English).is_err());
    }

    #[test]
    fn the_options_are_read_the_way_they_are_written() {
        let argv: Vec<String> = ["--tui", "--limit", "10", "--query", "invoice", "--no-hook"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let options = parse(&argv, Lang::English).expect("the options read");
        assert!(options.tui);
        assert!(!options.hook);
        assert_eq!(options.limit, 10);
        assert_eq!(options.query.as_deref(), Some("invoice"));
    }

    #[test]
    fn an_unknown_option_is_refused() {
        let argv = vec!["--nope".to_string()];
        assert!(parse(&argv, Lang::English).is_err());
    }

    #[test]
    fn a_limit_that_is_not_a_number_is_refused() {
        let argv = vec!["--limit".to_string(), "many".to_string()];
        assert!(parse(&argv, Lang::English).is_err());
    }

    #[test]
    fn the_picker_flags_are_read_the_way_they_are_written() {
        let argv: Vec<String> = [
            "--pick",
            "--fzf-binary",
            "C:/bin/fzf.exe",
            "--fzf-arg",
            "--height",
            "--fzf-arg",
            "--border",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let options = parse(&argv, Lang::English).expect("the options read");
        assert!(options.pick);
        assert_eq!(options.fzf_binary.as_deref(), Some("C:/bin/fzf.exe"));
        // Each `--fzf-arg` is one argument of the picker, and the order
        // is the order they were written in.
        assert_eq!(options.fzf_arg, vec!["--height", "--border"]);
    }

    /// The picker's arguments come in the order they win: the theme's
    /// colours, the file's, the command line's last.
    ///
    /// A repeated flag is read the way every program reads it — the
    /// last one — and the command line is the one the user typed, so it
    /// has the last word over the settings file.
    #[test]
    fn the_picker_s_arguments_come_in_the_order_they_win() {
        let settings = rgy_launcher::settings::Settings {
            fzf: rgy_launcher::settings::Fzf {
                args: vec!["--height".to_string(), "80%".to_string()],
                ..rgy_launcher::settings::Fzf::default()
            },
            ..rgy_launcher::settings::Settings::default()
        };
        let options = Options {
            fzf_binary: Some("C:/bin/fzf.exe".to_string()),
            fzf_arg: vec!["--height".to_string(), "40%".to_string()],
            ..Options::default()
        };
        let args = picker_args(&settings, &options);
        assert!(
            args.first()
                .is_some_and(|first| first.starts_with("--color")),
            "the theme's colours are first: {args:?}"
        );
        assert_eq!(
            args.iter().filter(|arg| arg.starts_with("--color")).count(),
            1,
            "one colour flag: {args:?}"
        );
        // ...then the file's free arguments...
        let height = args.iter().position(|arg| arg == "--height");
        let first_height = height.expect("the file's flag is there");
        assert_eq!(args[first_height + 1], "80%");
        // ...and the command line's last, which wins the repeated flag:
        // the second `--height` is the one the picker reads.
        assert_eq!(args[args.len() - 2], "--height", "{args:?}");
        assert_eq!(args.last(), Some(&"40%".to_string()), "{args:?}");
        assert_eq!(
            args.iter().filter(|arg| *arg == "--height").count(),
            2,
            "both flags are handed over: {args:?}"
        );
    }

    #[test]
    fn a_picker_without_a_search_is_refused() {
        let argv = vec!["--pick".to_string()];
        let reason = run(&argv, Lang::English).expect_err("a picker has nothing to filter");
        assert!(reason.contains("--query"), "{reason}");
    }

    #[test]
    fn the_help_names_the_picker_flags() {
        let text = usage(Lang::English);
        for needle in ["--pick", "--fzf-binary", "--fzf-arg"] {
            assert!(text.contains(needle), "the help needs `{needle}`: {text}");
        }
    }

    /// The command line waits as long as the client always did.
    ///
    /// The two-second window belongs to the window: a search box that
    /// stops answering the keyboard is broken, while a command line
    /// waiting for a cold daemon may wait as long as the client always
    /// did.
    #[test]
    fn the_command_line_waits_as_long_as_the_client() {
        let source = command_source(
            Endpoint::Socket(std::path::PathBuf::from("/tmp/rgy-rgxy-nope.sock")),
            None,
            Lang::English,
        );
        assert_eq!(source.window(), rgy_proto::client::read_timeout());
        assert!(
            source.window() > daemon::LAUNCHER_WINDOW,
            "the command line waits longer than the window"
        );
    }

    /// The window waits the launcher's window.
    ///
    /// The window and the terminal interface are search boxes: they live
    /// on the thread that draws them, and they cannot wait the client's
    /// thirty seconds.
    #[test]
    fn the_window_waits_the_launchers_window() {
        let source = DaemonSource::new(
            Endpoint::Socket(std::path::PathBuf::from("/tmp/rgy-rgxy-nope.sock")),
            None,
            Lang::English,
        );
        assert_eq!(source.window(), daemon::LAUNCHER_WINDOW);
    }

    /// The terminal says the same sentence the window says.
    ///
    /// The terminal interface is the same interface over the same
    /// source, and it must not be the one that keeps quiet: the
    /// sentence the window shows under the field is the sentence the
    /// terminal prints.
    #[test]
    fn the_terminal_says_the_same_sentence_the_window_says() {
        let mut source = DaemonSource::new(
            Endpoint::Socket(std::path::PathBuf::from("/tmp/rgy-rgxy-nope.sock")),
            None,
            Lang::English,
        );
        let notices = notices(&mut source, None, true);
        assert_eq!(notices.len(), 1, "the daemon is not there: {notices:?}");
        assert_eq!(notices[0], source.no_daemon_message());
        assert!(notices[0].contains("no daemon"), "{:?}", notices[0]);
    }

    /// A launcher with no terminal says nothing.
    ///
    /// A login entry on a machine with no session must not fail and must
    /// not print: there is nowhere to say it.
    #[test]
    fn a_launcher_with_no_terminal_says_nothing() {
        let mut source = DaemonSource::new(
            Endpoint::Socket(std::path::PathBuf::from("/tmp/rgy-rgxy-nope.sock")),
            None,
            Lang::English,
        );
        let notices = notices(
            &mut source,
            Some(Text::for_lang(Lang::English).no_graphical_screen),
            false,
        );
        assert!(
            notices.is_empty(),
            "there is nowhere to say it: {notices:?}"
        );
    }

    #[test]
    fn the_help_offers_only_the_pickers_that_can_read_the_list() {
        let text = usage(Lang::English);
        // The default is the picker that reads the NUL-separated list:
        // `peco` and `fz` do not, and offering them is offering a picker
        // that never sees the results.
        assert!(text.contains("default: fzf or sk on the PATH"), "{text}");
        assert!(!text.contains("default: peco"), "{text}");
    }

    /// A bare `rgxy` is the launcher.
    ///
    /// The manual, the README and the login entry all say that a bare
    /// `rgxy` is the resident launcher, and the program used to print
    /// its usage and leave: the help for a question nobody asked, and
    /// an exit status of 0 for a launcher that never started.
    #[test]
    fn an_empty_command_line_is_the_launcher() {
        let Request::Run(options) = request(&[], Lang::English).expect("the command line reads")
        else {
            panic!("an empty command line is the launcher, not the help");
        };
        assert!(options.hook, "the hotkey is installed");
        assert!(!options.show, "the window waits for the hotkey");
        assert!(!options.tui, "the window is the launcher");
        assert!(options.query.is_none(), "no question was asked");
    }

    /// Only a resident launch puts the console away.
    ///
    /// The one-shot, the picker and the terminal interface answer on the
    /// console they were given, and the help is the console's own
    /// question: only the resident launcher — no query, no terminal
    /// interface, no picker — has the notification area for a face.
    #[test]
    fn only_a_resident_launch_puts_the_console_away() {
        let resident = |argv: &[&str]| {
            let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
            resident_launch(&argv, Lang::English)
        };
        assert!(resident(&[]), "a bare rgxy is the resident launcher");
        assert!(
            resident(&["--no-hook"]),
            "the options of the window are the launcher's"
        );
        assert!(
            resident(&["--show"]),
            "the window that opens by itself is the launcher's"
        );
        assert!(
            !resident(&["--query", "invoice"]),
            "the one-shot answers on the console"
        );
        assert!(
            !resident(&["--tui"]),
            "the terminal interface is the console's"
        );
        assert!(
            !resident(&["--pick", "--query", "invoice"]),
            "the picker answers on the console"
        );
        assert!(
            !resident(&["--help"]),
            "the help is the console's own question"
        );
        assert!(
            !resident(&["--nope"]),
            "a refusal is said on the console it refused on"
        );
    }

    /// The usage text is what the help asks for.
    ///
    /// The help belongs to `--help`: a program that prints it when
    /// nobody asked is a program that answers a question nobody made.
    #[test]
    fn the_help_is_only_what_the_help_asks_for() {
        for flag in ["--help", "-h", "help"] {
            let argv = vec![flag.to_string()];
            assert!(
                matches!(
                    request(&argv, Lang::English).expect("the command line reads"),
                    Request::Help
                ),
                "`{flag}` asks for the help"
            );
        }
        let argv = vec!["-V".to_string()];
        assert!(
            matches!(
                request(&argv, Lang::English).expect("the command line reads"),
                Request::Version
            ),
            "`-V` asks for the version"
        );
    }

    /// The help says what a bare `rgxy` does.
    ///
    /// The usage text is the program's own description of itself, and a
    /// line that does not say that the launcher is the launcher is a
    /// line that leaves the reader guessing.
    #[test]
    fn the_help_says_that_a_bare_rgxy_is_the_launcher() {
        let text = usage(Lang::English);
        assert!(text.contains("the resident launcher"), "{text}");
    }

    /// A write that fails is a failure.
    ///
    /// The paths that did not reach the caller are not a search with no
    /// results: the loop stops at the first failure, and the status
    /// says the output did not arrive.
    #[test]
    fn a_write_that_fails_is_not_a_success() {
        let paths = vec![
            "/a/one.txt".to_string(),
            "/a/two.txt".to_string(),
            "/a/three.txt".to_string(),
        ];
        // The writer takes the first path and refuses the second: the
        // reader went away in the middle of the list.
        let mut out = Refusing::new(b"two.txt");
        assert!(write_paths(&paths, &mut out).is_err(), "the write fails");
        let taken = String::from_utf8_lossy(&out.taken).into_owned();
        assert!(taken.contains("one.txt"), "the first path went: {taken}");
        assert!(
            !taken.contains("two.txt"),
            "the refused path did not go: {taken}"
        );
        assert!(
            !taken.contains("three.txt"),
            "the loop stopped at the first failure: {taken}"
        );
    }

    /// The broken pipe is silent, and it is not a success.
    ///
    /// `rgxy --query invoice | head -3` is the reader going away on
    /// purpose: there is nobody to read a sentence about it, and the
    /// status must not say that the paths were printed.
    #[test]
    fn a_broken_pipe_is_silent_and_not_a_success() {
        let err = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "o leitor foi-se embora");
        assert_eq!(write_failure(&err, Lang::English), OUTPUT_NOT_DELIVERED);
        assert_ne!(
            write_failure(&err, Lang::English),
            0,
            "success is for output that arrived"
        );
    }

    /// The choice the user made has to reach the caller too.
    #[test]
    fn a_choice_that_cannot_be_printed_is_not_a_success() {
        let chosen = vec![b"/a/one.txt".to_vec()];
        let mut out = Refusing::new(b"one.txt");
        assert!(
            print_selection(&chosen, &mut out).is_err(),
            "the write fails"
        );
    }

    /// A writer that takes what it can and refuses the write that
    /// carries the needle.
    ///
    /// The broken pipe of a reader that went away: the writes before
    /// the needle go through, the one that carries it is refused, and
    /// the bytes that went through are what the caller would have
    /// read.
    struct Refusing {
        /// The bytes that make the write fail.
        needle: &'static [u8],
        /// The bytes that went through.
        taken: Vec<u8>,
    }

    impl Refusing {
        fn new(needle: &'static [u8]) -> Refusing {
            Refusing {
                needle,
                taken: Vec::new(),
            }
        }
    }

    impl Write for Refusing {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if buf
                .windows(self.needle.len())
                .any(|window| window == self.needle)
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "o leitor foi-se embora",
                ));
            }
            self.taken.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A path that carries a line break is not carried.
    ///
    /// The line is one path, and a path with a newline in it would be
    /// two: half of a path is a path that does not exist, and a reader
    /// that cannot tell the halves apart opens nothing.
    #[test]
    fn a_path_that_carries_a_line_break_is_not_carried() {
        let paths = vec![
            "/home/dana/notas\nde marco.txt".to_string(),
            "/home/dana/final\raso.txt".to_string(),
            "/home/dana/other.txt".to_string(),
        ];
        let mut out = Vec::new();
        write_paths(&paths, &mut out).expect("the write");
        let text = String::from_utf8(out).expect("the output is text");
        assert_eq!(
            text.lines().count(),
            1,
            "the paths that carry a line break are not lines: {text:?}"
        );
        assert!(
            text.contains("other.txt"),
            "the other path is there: {text:?}"
        );
    }

    /// A path with a percent sign is the path.
    ///
    /// The line format does not rewrite a path: a `%` is a `%`, and the
    /// path the reader gets is the path the daemon found.
    #[test]
    fn a_path_with_a_percent_sign_is_the_path() {
        let paths = vec!["/home/dana/100%done.txt".to_string()];
        let mut out = Vec::new();
        write_paths(&paths, &mut out).expect("the write");
        let text = String::from_utf8(out).expect("the output is text");
        assert_eq!(
            text.trim_end(),
            "/home/dana/100%done.txt",
            "the path is the path: {text:?}"
        );
    }

    /// A Windows path is not rewritten.
    ///
    /// The separator of a Windows path is a backslash, and a path that is
    /// rewritten is a path that does not exist.
    #[test]
    fn a_windows_path_is_not_rewritten() {
        let paths = vec![r"C:\Users\dana\notas.txt".to_string()];
        let mut out = Vec::new();
        write_paths(&paths, &mut out).expect("the write");
        let text = String::from_utf8(out).expect("the output is text");
        assert_eq!(
            text.trim_end(),
            r"C:\Users\dana\notas.txt",
            "the path is the path: {text:?}"
        );
    }
}
