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
// The memory of what the user used. The window is the only thing that
// records a use, and the window is Windows-only for now: elsewhere the
// module is compiled — and tested — with nothing calling it yet.
#[cfg_attr(not(windows), allow(dead_code))]
mod mru;
#[cfg(windows)]
mod preview;
mod rgx;
mod tools;

#[cfg(windows)]
mod gui;

use std::io::{IsTerminal, Write};
use std::process::ExitCode;
use std::time::{Duration, Instant};

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
fn usage() -> String {
    format!(
        "rgxy {version} — a janela de procura do daemon rgy.\n\
         \n\
         usage:\n  \
           rgxy [--tui] [--query <texto>] [--limit <n>] [--endpoint <e>] [--no-hook] [--show]\n  \
                [--pick] [--fzf-binary <p>] [--fzf-arg <arg>]\n\
         \n\
         options:\n  \
           --tui              a interface de terminal, em vez da janela\n  \
           --query <texto>    uma busca só: imprime os caminhos e sai\n  \
           --limit <n>        quantos resultados (default: 50)\n  \
           --endpoint <e>     pipe:<nome> | uds:<caminho> | stdio\n  \
           --no-hook          não instala o atalho global\n  \
           --show             abre a janela logo de início\n  \
           --pick             entrega os resultados a um picker e imprime a escolha\n  \
           --fzf-binary <p>   o picker a rodar (default: fzf ou sk no PATH)\n  \
           --fzf-arg <arg>    um argumento para o picker, repetível\n  \
           -h, --help         este texto\n  \
           -V, --version      a versão\n\
         \n\
         A janela é um cliente: o índice está no daemon (searchd), e uma\n\
         tecla é uma pergunta enviada por um pipe.\n\
         \n\
         Com --pick, os resultados vão para um picker e a escolha sai na\n\
         stdout, um caminho por linha — o mesmo contrato de --query.\n\
         \n\
         O picker por omissão é o fzf ou o sk: só eles leem a lista\n\
         separada por NUL. Um picker nomeado que não a leia — o peco, o\n\
         fz — recebe a lista com um caminho por linha.\n\
         \n\
         Na barra: file: folder: doc: pic: video: audio: — filtros;\n\
         >open >folder >copy >preview >menu — comandos; grep: — o conteúdo\n\
         dos ficheiros que a busca de nomes escolheu.",
        version = env!("CARGO_PKG_VERSION"),
    )
}

fn main() -> ExitCode {
    // The console decides how the bytes are decoded, and a path with an
    // accent has to reach the screen as itself.
    rgy_engine::console::init();

    let argv: Vec<String> = std::env::args().skip(1).collect();
    match run(&argv) {
        Ok(code) => code,
        Err(reason) => {
            eprintln!("rgxy: {reason}");
            ExitCode::from(1)
        }
    }
}

/// Run the command. `Err` is a sentence for stderr and exit code 1.
fn run(argv: &[String]) -> Result<ExitCode, String> {
    let Some(command) = argv.first() else {
        println!("{}", usage());
        return Ok(ExitCode::SUCCESS);
    };
    match command.as_str() {
        "-h" | "--help" | "help" => {
            println!("{}", usage());
            return Ok(ExitCode::SUCCESS);
        }
        "-V" | "--version" => {
            println!("rgxy {}", env!("CARGO_PKG_VERSION"));
            return Ok(ExitCode::SUCCESS);
        }
        _ => {}
    }

    let options = parse(argv)?;
    if options.pick && options.query.is_none() {
        // A picker filters the results of a search: without a question
        // there is no list to filter.
        return Err(
            "--pick precisa de --query: um picker filtra os resultados de uma busca".to_string(),
        );
    }
    let endpoint = match &options.endpoint {
        Some(text) => endpoint_of(text)?,
        None => Endpoint::registered().ok_or(
            "não se sabe onde o daemon escuta: nem XDG_CACHE_HOME nem HOME estão definidos",
        )?,
    };
    let rgx = tools::rgx();

    if let Some(query) = &options.query {
        // The command line waits as long as the client always did.
        let source = command_source(endpoint, rgx);
        if options.pick {
            return pick(source, query, options.limit, &options);
        }
        return one_shot(source, query, options.limit);
    }
    if options.tui {
        return run_tui(DaemonSource::new(endpoint, rgx), None);
    }
    run_window(endpoint, rgx, &options)
}

/// The source for the command line.
///
/// The window is the launcher's and the command line is the client's:
/// a search box that stops answering the keyboard is broken, while a
/// command line waiting for a cold daemon may wait as long as the
/// client always did.
fn command_source(endpoint: Endpoint, rgx: Option<String>) -> DaemonSource {
    DaemonSource::with_window(endpoint, rgx, rgy_proto::client::read_timeout())
}

/// Read the options.
fn parse(argv: &[String]) -> Result<Options, String> {
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
                options.fzf_binary = Some(
                    argv.get(index)
                        .ok_or("--fzf-binary precisa de um caminho")?
                        .clone(),
                );
            }
            "--fzf-arg" => {
                index += 1;
                options.fzf_arg.push(
                    argv.get(index)
                        .ok_or("--fzf-arg precisa de um argumento")?
                        .clone(),
                );
            }
            "--query" => {
                index += 1;
                options.query = Some(
                    argv.get(index)
                        .ok_or("--query precisa de um texto")?
                        .clone(),
                );
            }
            "--limit" => {
                index += 1;
                let value = argv.get(index).ok_or("--limit precisa de um número")?;
                options.limit = value
                    .parse::<usize>()
                    .map_err(|_| format!("--limit precisa de um número: {value}"))?;
            }
            "--endpoint" => {
                index += 1;
                options.endpoint = Some(
                    argv.get(index)
                        .ok_or("--endpoint precisa de um endereço")?
                        .clone(),
                );
            }
            other => return Err(format!("opção desconhecida `{other}`\n\n{}", usage())),
        }
        index += 1;
    }
    Ok(options)
}

/// The endpoint the command line named.
fn endpoint_of(text: &str) -> Result<Endpoint, String> {
    if let Some(path) = text.strip_prefix("uds:") {
        return Ok(Endpoint::Socket(std::path::PathBuf::from(path)));
    }
    if let Some(name) = text.strip_prefix("pipe:") {
        return Ok(Endpoint::Pipe(name.to_string()));
    }
    if text == "stdio" {
        return Ok(Endpoint::Stdio);
    }
    Err(format!(
        "endpoint desconhecido: {text} (use pipe:<nome>, uds:<caminho> ou stdio)"
    ))
}

/// One search, printed, and out.
///
/// The exit status is the search-shaped one: 0 for a match, 1 for none.
/// The *reason* a search has no answer goes to stderr, because "no
/// results" and "the daemon is not there" are different answers, and an
/// exit status that cannot tell them apart makes the second one look like
/// the first.
fn one_shot(mut source: DaemonSource, query: &str, limit: usize) -> Result<ExitCode, String> {
    let answer = wait_for_daemon(&mut source, query, limit);
    let mut out = std::io::stdout();
    for path in &answer.paths {
        let _ = writeln!(out, "{path}");
    }
    let _ = out.flush();
    for warning in source.status().borrow().warnings.clone() {
        eprintln!("rgxy: {warning}");
    }
    Ok(if answer.is_empty() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
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
/// run — which is not a search with no matches, and says so.
fn pick(
    mut source: DaemonSource,
    query: &str,
    limit: usize,
    options: &Options,
) -> Result<ExitCode, String> {
    if !std::io::stdout().is_terminal() {
        // A picker is interactive: without a terminal there is nothing
        // for the user to choose with, and pretending otherwise would
        // print a path nobody chose.
        return Err("um picker precisa de um terminal".to_string());
    }
    let answer = wait_for_daemon(&mut source, query, limit);
    for warning in source.status().borrow().warnings.clone() {
        eprintln!("rgxy: {warning}");
    }
    if answer.is_empty() {
        // There is nothing to pick, and the picker is not asked.
        eprintln!("rgxy: nenhum resultado");
        return Ok(ExitCode::from(1));
    }

    let req = rgy_pick::Request {
        args: &options.fzf_arg,
        binary: options.fzf_binary.as_deref(),
        separator: rgy_pick::Separator::Nul,
        order: rgy_pick::Order::Keep,
    };
    let buffer = records(&answer.paths);
    let search = Box::new(
        move |out: &mut (dyn Write + Send + 'static)| -> Result<bool, String> {
            out.write_all(&buffer)
                .map_err(|e| format!("os resultados não chegaram ao picker: {e}"))?;
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
    print_selection(&chosen, &mut out);
    Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)))
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
fn print_selection(chosen: &[Vec<u8>], out: &mut impl Write) {
    for record in chosen {
        let _ = out.write_all(record);
        let _ = out.write_all(b"\n");
    }
    let _ = out.flush();
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
    loop {
        let answer = source.search(query, limit);
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
fn run_tui(mut source: DaemonSource, reason: Option<&str>) -> Result<ExitCode, String> {
    let terminal = std::io::stdout().is_terminal();
    for notice in notices(&mut source, reason, terminal) {
        eprintln!("rgxy: {notice}");
    }
    if !terminal {
        return Ok(ExitCode::SUCCESS);
    }
    let root = home_dir();
    rgy_search_ui::run_with_source(Box::new(source), root).map(|_| ExitCode::SUCCESS)
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
#[cfg(windows)]
fn run_window(
    endpoint: Endpoint,
    rgx: Option<String>,
    options: &Options,
) -> Result<ExitCode, String> {
    if !rgy_launcher_windows::session_available() {
        // No desktop: the terminal is the fallback, and it says so.
        return run_tui(
            DaemonSource::new(endpoint, rgx),
            Some("sem ecrã gráfico; a interface de terminal é a alternativa"),
        );
    }
    // The launcher's own window: the window lives on the thread that
    // draws it, and it cannot wait the client's thirty seconds.
    gui::run(DaemonSource::new(endpoint, rgx), options).map(|_| ExitCode::SUCCESS)
}

/// The window, where there is a desktop to put it on.
///
/// The floating window is a Windows thing for now: the double-Control
/// hotkey is a low-level keyboard hook, and the hook this repository has
/// is the Windows one. Everywhere else the terminal interface is the
/// launcher, and it is the same interface over the same source.
#[cfg(not(windows))]
fn run_window(
    endpoint: Endpoint,
    rgx: Option<String>,
    _options: &Options,
) -> Result<ExitCode, String> {
    run_tui(
        DaemonSource::new(endpoint, rgx),
        Some(
            "a janela flutuante é só no Windows por agora; a interface de terminal é a alternativa",
        ),
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
        assert!(matches!(endpoint_of("stdio"), Ok(Endpoint::Stdio)));
        assert!(matches!(endpoint_of("pipe:abc"), Ok(Endpoint::Pipe(_))));
        assert!(matches!(
            endpoint_of("uds:/tmp/x.sock"),
            Ok(Endpoint::Socket(_))
        ));
        assert!(endpoint_of("http://x").is_err());
    }

    #[test]
    fn the_options_are_read_the_way_they_are_written() {
        let argv: Vec<String> = ["--tui", "--limit", "10", "--query", "invoice", "--no-hook"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let options = parse(&argv).expect("the options read");
        assert!(options.tui);
        assert!(!options.hook);
        assert_eq!(options.limit, 10);
        assert_eq!(options.query.as_deref(), Some("invoice"));
    }

    #[test]
    fn an_unknown_option_is_refused() {
        let argv = vec!["--nope".to_string()];
        assert!(parse(&argv).is_err());
    }

    #[test]
    fn a_limit_that_is_not_a_number_is_refused() {
        let argv = vec!["--limit".to_string(), "many".to_string()];
        assert!(parse(&argv).is_err());
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
        let options = parse(&argv).expect("the options read");
        assert!(options.pick);
        assert_eq!(options.fzf_binary.as_deref(), Some("C:/bin/fzf.exe"));
        // Each `--fzf-arg` is one argument of the picker, and the order
        // is the order they were written in.
        assert_eq!(options.fzf_arg, vec!["--height", "--border"]);
    }

    #[test]
    fn a_picker_without_a_search_is_refused() {
        let argv = vec!["--pick".to_string()];
        let reason = run(&argv).expect_err("a picker has nothing to filter");
        assert!(reason.contains("--query"), "{reason}");
    }

    #[test]
    fn the_help_names_the_picker_flags() {
        let text = usage();
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
        );
        let notices = notices(&mut source, None, true);
        assert_eq!(notices.len(), 1, "the daemon is not there: {notices:?}");
        assert_eq!(notices[0], source.no_daemon_message());
        assert!(notices[0].contains("sem daemon"), "{:?}", notices[0]);
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
        );
        let notices = notices(&mut source, Some("sem ecrã gráfico"), false);
        assert!(
            notices.is_empty(),
            "there is nowhere to say it: {notices:?}"
        );
    }

    #[test]
    fn the_help_offers_only_the_pickers_that_can_read_the_list() {
        let text = usage();
        // The default is the picker that reads the NUL-separated list:
        // `peco` and `fz` do not, and offering them is offering a picker
        // that never sees the results.
        assert!(text.contains("default: fzf ou sk no PATH"), "{text}");
        assert!(!text.contains("peco, fz no PATH"), "{text}");
    }
}
