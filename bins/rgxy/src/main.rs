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
#[cfg(windows)]
mod preview;
mod rgx;
mod tools;

#[cfg(windows)]
mod gui;

use std::io::Write;
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
        }
    }
}

/// The usage text.
fn usage() -> String {
    format!(
        "rgxy {version} — a janela de procura do daemon rgy.\n\
         \n\
         usage:\n  \
           rgxy [--tui] [--query <texto>] [--limit <n>] [--endpoint <e>] [--no-hook] [--show]\n\
         \n\
         options:\n  \
           --tui              a interface de terminal, em vez da janela\n  \
           --query <texto>    uma busca só: imprime os caminhos e sai\n  \
           --limit <n>        quantos resultados (default: 50)\n  \
           --endpoint <e>     pipe:<nome> | uds:<caminho> | stdio\n  \
           --no-hook          não instala o atalho global\n  \
           --show             abre a janela logo de início\n  \
           -h, --help         este texto\n  \
           -V, --version      a versão\n\
         \n\
         A janela é um cliente: o índice está no daemon (searchd), e uma\n\
         tecla é uma pergunta enviada por um pipe.\n\
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
    let endpoint = match &options.endpoint {
        Some(text) => endpoint_of(text)?,
        None => Endpoint::registered()
            .ok_or("não se sabe onde o daemon escuta: nem XDG_CACHE_HOME nem HOME estão definidos")?,
    };
    let source = DaemonSource::new(endpoint, tools::rgx());

    if let Some(query) = &options.query {
        return one_shot(source, query, options.limit);
    }
    if options.tui {
        return run_tui(source);
    }
    run_window(source, &options)
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
fn run_tui(mut source: DaemonSource) -> Result<ExitCode, String> {
    // The daemon is asked who it is: the answer says whether there is a
    // pack store, and the interface shows it.
    source.connect();
    let root = home_dir();
    rgy_search_ui::run_with_source(Box::new(source), root).map(|_| ExitCode::SUCCESS)
}

/// The window, where there is a desktop to put it on.
#[cfg(windows)]
fn run_window(source: DaemonSource, options: &Options) -> Result<ExitCode, String> {
    if !rgy_launcher_windows::session_available() {
        // No desktop: the terminal is the fallback, and it says so.
        eprintln!("rgxy: sem ecrã gráfico; a interface de terminal é a alternativa");
        return run_tui(source);
    }
    gui::run(source, options).map(|_| ExitCode::SUCCESS)
}

/// The window, where there is a desktop to put it on.
///
/// The floating window is a Windows thing for now: the double-Control
/// hotkey is a low-level keyboard hook, and the hook this repository has
/// is the Windows one. Everywhere else the terminal interface is the
/// launcher, and it is the same interface over the same source.
#[cfg(not(windows))]
fn run_window(source: DaemonSource, _options: &Options) -> Result<ExitCode, String> {
    eprintln!(
        "rgxy: a janela flutuante é só no Windows por agora; a interface de terminal é a alternativa"
    );
    run_tui(source)
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
        assert!(matches!(endpoint_of("uds:/tmp/x.sock"), Ok(Endpoint::Socket(_))));
        assert!(endpoint_of("http://x").is_err());
    }

    #[test]
    fn the_options_are_read_the_way_they_are_written() {
        let argv: Vec<String> = [
            "--tui",
            "--limit",
            "10",
            "--query",
            "invoice",
            "--no-hook",
        ]
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
}
