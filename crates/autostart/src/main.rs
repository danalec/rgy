// SPDX-License-Identifier: Apache-2.0

//! `rgy-autostart` — put the rgy search daemon in place at login.
//!
//! The daemon is what makes a search find the files that were created
//! while the machine was off, and that only works if the daemon is running
//! when the user logs in. This tool registers it, unregisters it, and says
//! what is registered.
//!
//! It never prompts and never asks for elevation. Registering is an
//! explicit thing the user does — running this command *is* the consent —
//! and every failure is a sentence on stderr, never a dialog.
//!
//!     rgy-autostart install
//!     rgy-autostart status
//!     rgy-autostart doctor
//!     rgy-autostart uninstall
//!
//! `install` needs nothing: the daemon is the one beside this tool, and
//! the tree it indexes is the user's home directory. Both defaults exist
//! because the alternative is a user typing a path he has to guess: the
//! release installs the two binaries side by side, and a login entry
//! starts in `C:\Windows\System32` on Windows and in `/` on macOS — so a
//! daemon that inherits the platform's login directory walks the wrong
//! tree, and says nothing about it.
//!
//! What gets registered depends on the platform:
//!
//!   * Windows — a Task Scheduler task with a logon trigger, or the
//!     `HKCU\...\Run` value when the task tool is not there. The task's
//!     command line is printed, because the task scheduler has no file
//!     form and the user is meant to see what was registered.
//!   * Linux — a systemd **user** unit, with `Restart=` and
//!     `WantedBy=default.target`, written out as text under
//!     `~/.config/systemd/user/`.
//!   * macOS — a launchd agent under `~/Library/LaunchAgents/`, with
//!     `RunAtLoad` and `KeepAlive`, written out as text.
//!
//! Exit codes: 0 = done, 1 = nothing was done (the reason is on stderr),
//! 2 = the entry was written but the platform could not arm it (the reason
//! is on stdout, with the command that would arm it).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use rgy_autostart::linux;
use rgy_autostart::macos;
use rgy_autostart::plan::{
    Mechanism, Platform, Transport, choose_mechanism, default_name, detect_platform, listen_arg,
    transport,
};
use rgy_autostart::render::{reg_add_command_line, schtasks_command_line, windows_task_command};
use rgy_autostart::windows;
use rgy_autostart::{
    DAEMON_NAME, Removal, Report, cache_dir, display_path, effective_args, find_daemon,
    find_launcher, home_dir, launcher_default_name, listen_of, mechanism_label, pipe_name,
    resolve_binary, root_of, socket_path_in,
};

/// The console's output code page.
///
/// Windows decides how the bytes a program prints are decoded, and the
/// default is a legacy single-byte page: a path with an accent comes out
/// as `?`. The path of the daemon is exactly the kind of text that has an
/// accent in it, so the code page is switched to UTF-8 — but only when
/// there is a console at all, because a pipe wants the raw bytes. On
/// every other platform this does nothing.
mod console {
    #[cfg(windows)]
    pub fn init() {
        use std::ffi::c_void;
        use std::os::windows::io::AsRawHandle;

        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetConsoleMode(handle: *mut c_void, mode: *mut u32) -> i32;
            fn SetConsoleOutputCP(code_page: u32) -> i32;
        }

        // The UTF-8 code page.
        const CP_UTF8: u32 = 65001;

        let mut mode = 0u32;
        let is_console =
            unsafe { GetConsoleMode(std::io::stdout().as_raw_handle(), &mut mode) } != 0;
        if is_console {
            unsafe { SetConsoleOutputCP(CP_UTF8) };
        }
    }

    #[cfg(not(windows))]
    pub fn init() {}
}

/// The usage text.
fn usage() -> String {
    format!(
        "rgy-autostart {version} — register the rgy search daemon to start at login.\n\
         \n\
         usage:\n  \
           rgy-autostart install [--binary <path>] [--args \"<args>\"] [--name <name>] [--launcher]\n  \
           rgy-autostart uninstall [--name <name>] [--launcher]\n  \
           rgy-autostart status [--name <name>]\n  \
           rgy-autostart doctor [--name <name>]\n\
         \n\
         options:\n  \
           --binary <path>   the daemon to start (default: the {daemon} next to this tool)\n  \
           --args \"<args>\"    arguments handed to the daemon (default: --root <your home> --listen <transport>)\n  \
           --name <name>     the name of the entry (default: {default})\n  \
           --launcher        on install: register the launcher (rgxy) too, under its own name\n\
                            on uninstall: take the launcher's entry out alone\n\
           -h, --help        this text\n  \
           -V, --version     the version\n\
         \n\
         The daemon is registered into your own session: no administrator,\n\
         no prompt, no dialog. Uninstalling removes what was installed --\n\
         the daemon's entry and the launcher's, when both are there.\n\
         \n\
         `install` needs nothing: the daemon is the one beside this tool,\n\
         and the tree it indexes is your home directory. `doctor` says what\n\
         is registered, whether the daemon is up, and what to do about it.\n\
         \n\
         The daemon is registered listening on a transport that has no end:\n\
         a socket on Linux and macOS, a named pipe on Windows. The daemon's\n\
         own default reads from standard input, and a login entry has no\n\
         standard input to read from: the daemon would start, index the\n\
         tree, and exit -- registered, and gone.",
        version = env!("CARGO_PKG_VERSION"),
        default = default_name(detect_platform()),
        daemon = DAEMON_NAME,
    )
}

/// What the command line said.
#[derive(Debug, Default)]
struct Options {
    /// The daemon's path.
    binary: Option<PathBuf>,
    /// The daemon's arguments.
    args: Vec<String>,
    /// The name of the entry.
    name: Option<String>,
    /// Whether the launcher is registered too.
    launcher: bool,
}

fn main() -> ExitCode {
    console::init();

    let argv: Vec<String> = std::env::args().skip(1).collect();
    match run(&argv) {
        Ok(code) => code,
        Err(reason) => {
            eprintln!("rgy-autostart: {reason}");
            ExitCode::from(1)
        }
    }
}

/// Run the command. `Err` is a sentence for stderr and exit code 1.
fn run(argv: &[String]) -> Result<ExitCode, String> {
    let Some(command) = argv.first() else {
        return Err(usage());
    };

    match command.as_str() {
        "-h" | "--help" | "help" => {
            println!("{}", usage());
            return Ok(ExitCode::SUCCESS);
        }
        "-V" | "--version" => {
            println!("rgy-autostart {}", env!("CARGO_PKG_VERSION"));
            return Ok(ExitCode::SUCCESS);
        }
        _ => {}
    }

    let options = parse_options(&argv[1..])?;
    match command.as_str() {
        "install" => install(&options),
        "uninstall" => uninstall(&options),
        "status" => status(&options),
        "doctor" => doctor(&options),
        other => Err(format!("unknown command `{other}`\n\n{}", usage())),
    }
}

/// Read the options after the command.
fn parse_options(argv: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut index = 0;
    while index < argv.len() {
        match argv[index].as_str() {
            "--binary" => {
                index += 1;
                let value = argv.get(index).ok_or("--binary needs a path")?;
                options.binary = Some(PathBuf::from(value));
            }
            "--args" => {
                index += 1;
                let value = argv.get(index).ok_or("--args needs a value")?;
                options.args = rgy_autostart::split_args(value);
            }
            "--name" => {
                index += 1;
                let value = argv.get(index).ok_or("--name needs a name")?;
                options.name = Some(value.clone());
            }
            "--launcher" => options.launcher = true,
            other => return Err(format!("unknown option `{other}`\n\n{}", usage())),
        }
        index += 1;
    }
    Ok(options)
}

/// The platform's default name, or the one the user picked.
fn entry_name(options: &Options, platform: Platform) -> String {
    options
        .name
        .clone()
        .unwrap_or_else(|| default_name(platform).to_string())
}

/// Register the daemon.
fn install(options: &Options) -> Result<ExitCode, String> {
    let platform = detect_platform();
    if platform == Platform::Unsupported {
        return Err("autostart is not implemented on this operating system".to_string());
    }

    // The daemon is the one next to this tool, or the one on the PATH.
    // The release installs the two side by side, and a user who has to
    // type the path is a user who gets it wrong — or who types the path
    // of a daemon that is not there.
    let binary = match options.binary.as_ref() {
        Some(path) => resolve_binary(path)?,
        None => find_daemon()?,
    };
    let name = entry_name(options, platform);

    // The tree the daemon indexes is always named. The daemon's own
    // default is `.`, and the directory a login entry starts in is not
    // the user's home: it is `C:\Windows\System32` on Windows and `/` on
    // macOS. A daemon that inherits that walks the wrong tree.
    let root = display_path(&home_dir());

    // The transport is named too, and it has to be one with no end. The
    // daemon's own default is stdio, and a login entry has no standard
    // input to read from: the daemon reads one EOF and exits, and the
    // service is registered, started, and gone. `Restart=on-failure` does
    // not bring it back, because an exit with a zero status is not a
    // failure. A socket (unix) or a named pipe (Windows) waits for a
    // client and never sees an end of file.
    let transport = transport(platform)
        .ok_or("this operating system has no transport to register the daemon on")?;
    let endpoint = match transport {
        Transport::UnixSocket => {
            let cache = cache_dir().ok_or(
                "cannot work out where the platform keeps its cache directory: neither \
                 XDG_CACHE_HOME nor HOME is set",
            )?;
            let socket = socket_path_in(&cache);
            // The socket the daemon will bind. A unix socket path is
            // short by the platform's own rules -- `sun_path` is 104
            // bytes on macOS and 108 on Linux -- and a path that does
            // not fit is a daemon that cannot start at all. The rule is
            // the protocol layer's, and the daemon shortens the same
            // path the same way: a registration that named another socket
            // would be a daemon nobody reaches.
            let socket = rgy_proto::endpoint::socket_path(&socket).map_err(|e| e.to_string())?;
            // The directory has to be there before the daemon binds the
            // socket in it, and the daemon does not create it: a bind on a
            // missing directory is a daemon that exits and is restarted
            // for ever.
            if let Some(dir) = socket.parent() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
            }
            display_path(&socket)
        }
        Transport::NamedPipe => pipe_name(),
    };
    let listen = listen_arg(transport, &endpoint);

    let args = effective_args(&options.args, &root, &listen);

    let mut report = match platform {
        Platform::Windows => install_windows(&name, &binary, &args)?,
        Platform::Linux => install_linux(&name, &binary, &args)?,
        Platform::MacOs => install_macos(&name, &binary, &args)?,
        Platform::Unsupported => return Err("autostart is not implemented here".to_string()),
    };
    report.notes.push(match transport {
        Transport::UnixSocket => format!("the daemon listens on the socket {endpoint}"),
        Transport::NamedPipe => {
            format!("the daemon listens on the named pipe \\\\.\\pipe\\{endpoint}")
        }
    });

    print_report(&report);

    // The launcher is a program of its own, and it gets its own entry.
    //
    // It is **resident**: the window is there in a few milliseconds
    // because the daemon has the index warm, not because the launcher
    // built one. A launcher that started at login would pay for the
    // startup on every opening, and an opening that takes a second is
    // not a launcher.
    //
    // The endpoint is named explicitly, for the same reason the daemon's
    // root is: a login entry does not inherit the environment of the
    // session that installed it.
    let mut armed = report.armed;
    let mut launcher = rgy_autostart::Launcher::Missing;
    if options.launcher {
        match find_launcher() {
            Ok(launcher_binary) => {
                let launcher_name = launcher_default_name(platform);
                let launcher_args = vec!["--endpoint".to_string(), listen.clone()];
                let launcher_report = match platform {
                    Platform::Windows => {
                        install_windows(launcher_name, &launcher_binary, &launcher_args)?
                    }
                    Platform::Linux => {
                        install_linux(launcher_name, &launcher_binary, &launcher_args)?
                    }
                    Platform::MacOs => {
                        install_macos(launcher_name, &launcher_binary, &launcher_args)?
                    }
                    Platform::Unsupported => {
                        return Err("autostart is not implemented here".to_string());
                    }
                };
                launcher = if launcher_report.armed {
                    rgy_autostart::Launcher::Armed
                } else {
                    rgy_autostart::Launcher::NotArmed
                };
                armed = armed && launcher_report.armed;
                println!();
                println!("the launcher:");
                print_report(&launcher_report);
            }
            Err(reason) => {
                // The daemon is registered already, and it is armed. A
                // launcher that is not on the machine is not a failure of
                // the daemon -- the daemon works without it, and the
                // launcher is the optional half.
                //
                // The exit status has to say what happened: one that says
                // "nothing was done" when the daemon was done is an exit
                // status that lies, and a script that reads it would take
                // the daemon back out.
                eprintln!("rgy-autostart: {reason}");
                eprintln!("  the daemon was registered; the launcher was not");
            }
        }
    }

    // 2 says what happened without dressing it up: the entry is written,
    // the platform could not arm it, and the note says how.
    Ok(ExitCode::from(rgy_autostart::install_status(
        armed, launcher,
    )))
}

/// The Windows side: the task scheduler, or the registry as a fallback.
fn install_windows(name: &str, binary: &Path, args: &[String]) -> Result<Report, String> {
    let tool = windows::startup_task_tool_available();
    let command = windows_task_command(binary, args);
    let mut notes = Vec::new();

    // The decision is the pure one: the task scheduler first, and the
    // registry only when it is not there.
    match choose_mechanism(Platform::Windows, tool) {
        Some(Mechanism::StartupTask) => match windows::install_task(name, binary, args) {
            Ok(()) => {
                notes.push(format!(
                    "the task's command line:\n    {}",
                    schtasks_command_line(binary, args, name)
                ));
                notes.push(
                    "the task runs at logon, with your own rights and no elevation".to_string(),
                );
                Ok(Report {
                    mechanism: Mechanism::StartupTask,
                    artifact: None,
                    command,
                    armed: true,
                    notes,
                })
            }
            Err(reason) => {
                // The task scheduler said no. The registry value is the
                // weaker mechanism, and the tool says so rather than
                // pretending the two are the same.
                windows::install_run_value(name, binary, args).map_err(|fallback| {
                    format!("{reason}; the fallback to the HKCU Run value failed too: {fallback}")
                })?;
                notes.push(format!(
                    "the startup task could not be created ({reason}), so the weaker HKCU Run \
                     value was written instead: no Task Scheduler entry, and no restart policy"
                ));
                notes.push(format!(
                    "the value's command line:\n    {}",
                    reg_add_command_line(binary, args, name)
                ));
                Ok(Report {
                    mechanism: Mechanism::RegistryRun,
                    artifact: None,
                    command,
                    armed: true,
                    notes,
                })
            }
        },
        Some(Mechanism::RegistryRun) => {
            windows::install_run_value(name, binary, args)?;
            notes.push(
                "the task scheduler's command line is not there, so the weaker HKCU Run value was \
                 written: no Task Scheduler entry, and no restart policy"
                    .to_string(),
            );
            notes.push(format!(
                "the value's command line:\n    {}",
                reg_add_command_line(binary, args, name)
            ));
            Ok(Report {
                mechanism: Mechanism::RegistryRun,
                artifact: None,
                command,
                armed: true,
                notes,
            })
        }
        other => Err(format!("there is no Windows mechanism to use ({other:?})")),
    }
}

/// The Linux side: a systemd user unit.
fn install_linux(name: &str, binary: &Path, args: &[String]) -> Result<Report, String> {
    let unit_name = unit_name(name);
    let home = linux::config_home();
    let path = linux::unit_path_in(&home, &unit_name);

    let changed = linux::write_unit(&home, &unit_name, binary, args)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;

    let mut notes = vec![if changed {
        "the unit file was written".to_string()
    } else {
        "the unit file was already there and says the same thing".to_string()
    }];

    let armed = match linux::enable(&unit_name) {
        Ok(_) => {
            notes.push(format!(
                "armed with: systemctl --user enable --now {unit_name}"
            ));
            true
        }
        Err(reason) => {
            notes.push(format!(
                "the unit is written but could not be armed: {reason}"
            ));
            if reason.contains("does not exist") {
                // The manager's unit path is fixed when it starts, so a
                // config directory it was not started with is invisible to
                // it: the file is there, and the manager never looks
                // there.
                notes.push(
                    "the user's systemd manager may not be looking at this directory: it \
                     searches the config home it was started with"
                        .to_string(),
                );
            }
            notes.push(format!(
                "arm it by hand with: systemctl --user enable --now {unit_name}"
            ));
            false
        }
    };

    let command = linux::registered_command(&home, &unit_name)
        .unwrap_or_else(|| binary.display().to_string());

    Ok(Report {
        mechanism: Mechanism::SystemdUserUnit,
        artifact: Some(path),
        command,
        armed,
        notes,
    })
}

/// The macOS side: a launchd agent.
fn install_macos(name: &str, binary: &Path, args: &[String]) -> Result<Report, String> {
    let home = rgy_autostart::home_dir();
    let path = macos::plist_path_in(&home, name);

    let changed = macos::write_plist(&home, name, binary, args)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;

    let mut notes = vec![if changed {
        "the plist was written".to_string()
    } else {
        "the plist was already there and says the same thing".to_string()
    }];

    let armed = match macos::load(&path) {
        Ok(_) => {
            notes.push("loaded into your session with launchctl".to_string());
            true
        }
        Err(reason) => {
            notes.push(format!(
                "the plist is written but could not be loaded: {reason}"
            ));
            notes.push(format!(
                "load it by hand with: launchctl bootstrap gui/$(id -u) {}",
                path.display()
            ));
            false
        }
    };

    let command =
        macos::registered_command(&home, name).unwrap_or_else(|| binary.display().to_string());

    Ok(Report {
        mechanism: Mechanism::LaunchdAgent,
        artifact: Some(path),
        command,
        armed,
        notes,
    })
}

/// The systemd unit's name: the file name ends in `.service`.
fn unit_name(name: &str) -> String {
    if name.ends_with(".service") {
        name.to_string()
    } else {
        format!("{name}.service")
    }
}

/// Unregister the daemon.
fn uninstall(options: &Options) -> Result<ExitCode, String> {
    let platform = detect_platform();
    if platform == Platform::Unsupported {
        return Err("autostart is not implemented on this operating system".to_string());
    }

    // What goes: the entry the caller named, the launcher's entry, or
    // both of the entries an install writes.
    //
    // An install that registered the launcher registered two things, and
    // an uninstall that removes one of them leaves the other starting at
    // every login -- which is the way a user finds out that a tool they
    // removed is still running.
    let names =
        rgy_autostart::uninstall_targets(options.name.as_deref(), options.launcher, platform);

    for (index, name) in names.iter().enumerate() {
        let removal = match platform {
            Platform::Windows => uninstall_windows(name)?,
            Platform::Linux => uninstall_linux(name)?,
            Platform::MacOs => uninstall_macos(name)?,
            Platform::Unsupported => return Err("autostart is not implemented here".to_string()),
        };

        if index > 0 {
            println!();
        }
        println!(
            "removed: {}",
            if removal.removed {
                removal.mechanism.name()
            } else {
                "nothing"
            }
        );
        for note in &removal.notes {
            println!("  {note}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// The Windows side: the task, and the registry value that may stand in
/// for it.
fn uninstall_windows(name: &str) -> Result<Removal, String> {
    let mut notes = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    let mut removed = false;
    let mut mechanism = Mechanism::StartupTask;

    match windows::remove_task(name) {
        Ok(there) => {
            removed |= there;
            notes.push(if there {
                format!("the startup task `{name}` is gone")
            } else {
                format!("there was no startup task named `{name}`")
            });
        }
        Err(reason) => {
            // The task is still there: the uninstall did not do what it
            // was asked to do.
            failures.push(reason.clone());
            notes.push(reason);
        }
    }

    match windows::remove_run_value(name) {
        Ok(there) => {
            if there {
                // The value is the only thing that was there, so it is the
                // mechanism that was removed.
                if !removed {
                    mechanism = Mechanism::RegistryRun;
                }
                removed = true;
                notes.push(format!("the HKCU Run value `{name}` is gone"));
            } else {
                notes.push(format!("there was no HKCU Run value named `{name}`"));
            }
        }
        Err(reason) => {
            failures.push(reason.clone());
            notes.push(reason);
        }
    }

    if !failures.is_empty() {
        // The uninstall did not unregister: it says so, and it says so with
        // the exit code, because a script that reads the exit code is the
        // thing that notices.
        return Err(format!(
            "the entry `{name}` is still registered: {}",
            failures.join("; ")
        ));
    }

    Ok(Removal {
        mechanism,
        removed,
        notes,
    })
}

/// The Linux side: stop the unit, and take the file away.
fn uninstall_linux(name: &str) -> Result<Removal, String> {
    let unit_name = unit_name(name);
    let home = linux::config_home();
    let mut notes = Vec::new();
    let mut failures: Vec<String> = Vec::new();

    match linux::disable(&unit_name) {
        Ok(_) => notes.push(format!(
            "the unit was stopped: systemctl --user disable --now {unit_name}"
        )),
        Err(reason) => {
            // The file goes away, but a unit that is still loaded still
            // runs: the uninstall did not unregister it.
            failures.push(reason.clone());
            notes.push(format!(
                "the unit could not be stopped through systemd ({reason}); the file is removed anyway"
            ));
        }
    }

    let removed = linux::remove_unit(&home, &unit_name).map_err(|e| {
        format!(
            "cannot remove {}: {e}",
            linux::unit_path_in(&home, &unit_name).display()
        )
    })?;

    notes.push(if removed {
        format!(
            "the unit file is gone: {}",
            linux::unit_path_in(&home, &unit_name).display()
        )
    } else {
        format!(
            "there was no unit file at {}",
            linux::unit_path_in(&home, &unit_name).display()
        )
    });

    if !failures.is_empty() {
        return Err(format!(
            "the unit `{unit_name}` is still loaded: {}",
            failures.join("; ")
        ));
    }

    Ok(Removal {
        mechanism: Mechanism::SystemdUserUnit,
        removed,
        notes,
    })
}

/// The macOS side: unload the agent, and take the plist away.
fn uninstall_macos(name: &str) -> Result<Removal, String> {
    let home = rgy_autostart::home_dir();
    let path = macos::plist_path_in(&home, name);
    let mut notes = Vec::new();

    let mut failures: Vec<String> = Vec::new();
    match macos::unload(&path, name) {
        Ok(true) => notes.push("the agent was unloaded".to_string()),
        Ok(false) => notes.push("the agent was not loaded".to_string()),
        Err(reason) => {
            // The plist goes away, but an agent that is still loaded still
            // starts with the session.
            failures.push(reason.clone());
            notes.push(reason);
        }
    }

    let removed = macos::remove_plist(&home, name)
        .map_err(|e| format!("cannot remove {}: {e}", path.display()))?;

    notes.push(if removed {
        format!("the plist is gone: {}", path.display())
    } else {
        format!("there was no plist at {}", path.display())
    });

    if !failures.is_empty() {
        return Err(format!(
            "the agent `{name}` is still loaded: {}",
            failures.join("; ")
        ));
    }

    Ok(Removal {
        mechanism: Mechanism::LaunchdAgent,
        removed,
        notes,
    })
}

/// Say what is registered.
fn status(options: &Options) -> Result<ExitCode, String> {
    let platform = detect_platform();
    if platform == Platform::Unsupported {
        return Err("autostart is not implemented on this operating system".to_string());
    }
    let name = entry_name(options, platform);

    println!("platform:  {}", platform.name());
    match platform {
        Platform::Windows => status_windows(&name),
        Platform::Linux => status_linux(&name),
        Platform::MacOs => status_macos(&name),
        Platform::Unsupported => return Err("autostart is not implemented here".to_string()),
    }

    // The launcher is a second entry, and "is the window going to open
    // when I log in" is a question about it. The report answers both, or
    // the user is left with a window that never opens and a status that
    // says everything is fine.
    if options.name.is_none() {
        let launcher = launcher_default_name(platform);
        println!();
        println!("launcher:  {launcher}");
        match platform {
            Platform::Windows => status_windows(launcher),
            Platform::Linux => status_linux(launcher),
            Platform::MacOs => status_macos(launcher),
            Platform::Unsupported => return Err("autostart is not implemented here".to_string()),
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Whether the launcher's entry is registered, on this platform.
fn launcher_registered(platform: Platform, name: &str) -> bool {
    match platform {
        Platform::Windows => windows::status(name, name).registered(),
        Platform::Linux => linux::status(&linux::config_home(), &unit_name(name)).file,
        Platform::MacOs => macos::status(&rgy_autostart::home_dir(), name).file,
        Platform::Unsupported => false,
    }
}

/// The Windows side.
fn status_windows(name: &str) {
    let state = windows::status(name, name);
    if let Some(command) = &state.task {
        println!("mechanism: startup task");
        println!("  task:    {name}");
        println!("  command: {command}");
        println!("  state:   registered");
        println!("  the task's command line:");
        println!("    schtasks /Query /TN \"{name}\" /FO LIST /V");
        return;
    }
    if let Some(data) = &state.run_value {
        println!("mechanism: {}", mechanism_label(Mechanism::RegistryRun));
        println!("  value:   {name}");
        println!("  command: {data}");
        println!("  state:   registered");
        return;
    }
    println!("mechanism: startup task");
    println!("  state:   not registered");
}

/// The Linux side.
fn status_linux(name: &str) {
    let unit_name = unit_name(name);
    let home = linux::config_home();
    let state = linux::status(&home, &unit_name);

    println!("mechanism: systemd user unit");
    if !state.file {
        println!("  state:   not registered");
        return;
    }
    println!(
        "  unit:    {}",
        linux::unit_path_in(&home, &unit_name).display()
    );
    if let Some(command) = linux::registered_command(&home, &unit_name) {
        println!("  command: {command}");
    }
    println!(
        "  enabled: {}",
        state.enabled.as_deref().unwrap_or("unknown")
    );
    println!(
        "  active:  {}",
        state.active.as_deref().unwrap_or("unknown")
    );
}

/// The macOS side.
fn status_macos(name: &str) {
    let home = rgy_autostart::home_dir();
    let state = macos::status(&home, name);

    println!("mechanism: launchd agent");
    if !state.file {
        println!("  state:   not registered");
        return;
    }
    println!("  plist:   {}", macos::plist_path(name).display());
    if let Some(command) = macos::registered_command(&home, name) {
        println!("  command: {command}");
    }
    println!(
        "  loaded:  {}",
        match state.loaded {
            Some(true) => "yes",
            Some(false) => "no",
            None => "unknown",
        }
    );
}

/// What the doctor found, in the shape every platform answers in.
///
/// The shape is the same on the three operating systems on purpose: a
/// user who learns one of them has learned the other two, and a report
/// that changes its lines per platform is a report nobody can read at a
/// glance.
struct Diagnosis {
    /// The daemon, and where it was found.
    daemon: Option<String>,
    /// Where the daemon was found.
    daemon_where: &'static str,
    /// Whether the entry is registered.
    registered: bool,
    /// The mechanism that holds it.
    mechanism: Mechanism,
    /// The entry's own name or path.
    entry: Option<String>,
    /// Whether the daemon is running now.
    running: Option<bool>,
    /// The tree the registered command indexes.
    root: Option<String>,
    /// The transport the registered command listens on.
    listen: Option<String>,
    /// The directory this tool is in.
    tool_dir: Option<String>,
    /// Whether that directory is on the PATH.
    on_path: Option<bool>,
    /// The command that starts the daemon, when it is registered and idle.
    fix_run: Option<String>,
}

/// Say what is where: the daemon, the entry, whether the daemon is up,
/// the tree it indexes, and the PATH.
///
/// The doctor registers nothing and changes nothing. It is the command a
/// user runs when something is not working, and a diagnostic that
/// changes the machine is not a diagnostic.
fn doctor(options: &Options) -> Result<ExitCode, String> {
    let platform = detect_platform();
    if platform == Platform::Unsupported {
        return Err("autostart is not implemented on this operating system".to_string());
    }
    let name = entry_name(options, platform);

    let diagnosis = match platform {
        Platform::Windows => diagnose_windows(&name),
        Platform::Linux => diagnose_linux(&name),
        Platform::MacOs => diagnose_macos(&name),
        Platform::Unsupported => return Err("autostart is not implemented here".to_string()),
    };
    print_diagnosis(&diagnosis);

    // The launcher is a second entry, and the question it answers is
    // "why did the window not open at login". The doctor answers it.
    if options.name.is_none() {
        let launcher = launcher_default_name(platform);
        let registered = launcher_registered(platform, launcher);
        println!();
        println!("launcher:  {launcher}");
        if registered {
            println!("  state:   registered");
        } else {
            println!("  state:   not registered");
            println!("  to register it: rgy-autostart install --launcher");
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Whether the daemon is up, as the daemon itself says.
///
/// "Registered" and "running" are two different questions. The entry
/// answers the first -- the registry value, the unit file, the plist --
/// and the daemon answers the second: it serves the endpoint the clients
/// look for, and something serving that endpoint is a daemon that is up.
///
/// The connection is the question, and not the daemon's own answer: a
/// daemon that is busy indexing the tree does not answer, and a daemon
/// that is busy is up. The entry's own state is what is left when there
/// is no endpoint to ask -- a machine with nowhere to put a socket has no
/// daemon to ask, and the scheduled task still knows whether it runs.
///
/// The doctor asks a question and changes nothing: the connection is
/// opened and closed, and no request is sent.
///
/// The endpoint is the **client's** endpoint, and it is asked for with the
/// client's own two calls: `Endpoint::registered` names the place, and
/// `Client::connect` opens it. The rule that names the place lives in
/// `rgy-proto` (`rgy_proto::endpoint`), and the doctor reads it from
/// there instead of spelling it out here -- a doctor that looked under a
/// path of its own would say "no" on a machine where the daemon is up, and
/// a diagnostic that lies about the one thing it exists for is worse than
/// no diagnostic at all.
fn daemon_is_up() -> Option<bool> {
    let endpoint = rgy_proto::endpoint::Endpoint::registered()?;
    Some(rgy_proto::Client::connect(&endpoint).is_ok())
}

/// The Windows side.
fn diagnose_windows(name: &str) -> Diagnosis {
    let state = windows::status(name, name);
    let command = state.task.clone().or_else(|| state.run_value.clone());
    // The registry value has no state to ask about: there is no "is it
    // running" in a string, and the tool says "unknown" rather than
    // guessing. The daemon can be asked, and a daemon that serves the
    // registered endpoint is one that is up.
    let running = daemon_is_up().or_else(|| {
        state
            .task
            .as_ref()
            .and_then(|_| windows::task_status(name))
            .map(|status| windows::status_is_running(&status))
    });
    let (daemon, daemon_where) = daemon_and_where();
    let dir = tool_dir();
    Diagnosis {
        daemon,
        daemon_where,
        registered: state.registered(),
        mechanism: if state.task.is_some() {
            Mechanism::StartupTask
        } else {
            Mechanism::RegistryRun
        },
        entry: Some(name.to_string()),
        running,
        root: command.as_deref().and_then(root_of),
        listen: command.as_deref().and_then(listen_of),
        on_path: dir.as_deref().and_then(on_path),
        tool_dir: dir,
        fix_run: Some(format!("schtasks /Run /TN \"{name}\"")),
    }
}

/// The Linux side.
fn diagnose_linux(name: &str) -> Diagnosis {
    let unit = unit_name(name);
    let home = linux::config_home();
    let state = linux::status(&home, &unit);
    let command = linux::registered_command(&home, &unit);
    let (daemon, daemon_where) = daemon_and_where();
    let dir = tool_dir();
    Diagnosis {
        daemon,
        daemon_where,
        registered: state.file,
        mechanism: Mechanism::SystemdUserUnit,
        entry: Some(linux::unit_path_in(&home, &unit).display().to_string()),
        running: daemon_is_up()
            .or_else(|| state.active.as_deref().map(|active| active == "active")),
        root: command.as_deref().and_then(root_of),
        listen: command.as_deref().and_then(listen_of),
        on_path: dir.as_deref().and_then(on_path),
        tool_dir: dir,
        fix_run: Some(format!("systemctl --user enable --now {unit}")),
    }
}

/// The macOS side.
fn diagnose_macos(name: &str) -> Diagnosis {
    let home = home_dir();
    let state = macos::status(&home, name);
    let command = macos::registered_command(&home, name);
    let (daemon, daemon_where) = daemon_and_where();
    let dir = tool_dir();
    Diagnosis {
        daemon,
        daemon_where,
        registered: state.file,
        mechanism: Mechanism::LaunchdAgent,
        entry: Some(macos::plist_path(name).display().to_string()),
        running: daemon_is_up().or_else(|| {
            macos::agent_state(name)
                .as_deref()
                .map(macos::state_is_running)
        }),
        root: command.as_deref().and_then(root_of),
        listen: command.as_deref().and_then(listen_of),
        on_path: dir.as_deref().and_then(on_path),
        tool_dir: dir,
        fix_run: Some(format!(
            "launchctl bootstrap gui/$(id -u) {}",
            macos::plist_path(name).display()
        )),
    }
}

/// Print what the doctor found.
fn print_diagnosis(d: &Diagnosis) {
    println!("platform:   {}", detect_platform().name());
    match &d.daemon {
        Some(path) => println!("daemon:     {path} ({})", d.daemon_where),
        None => println!("daemon:     not found ({})", d.daemon_where),
    }
    if d.registered {
        println!("registered: yes ({})", d.mechanism.name());
        if let Some(entry) = &d.entry {
            println!("entry:      {entry}");
        }
    } else {
        println!("registered: no");
    }
    println!(
        "running:    {}",
        match d.running {
            Some(true) => "yes",
            Some(false) => "no",
            None => "unknown",
        }
    );
    match &d.root {
        Some(root) => println!("root:       {root}"),
        None => println!("root:       unknown"),
    }
    match &d.listen {
        Some(listen) => println!("listens:    {listen}"),
        None => println!("listens:    unknown"),
    }
    match (&d.tool_dir, d.on_path) {
        (Some(dir), Some(true)) => println!("path:       {dir} is on the PATH"),
        (Some(dir), Some(false)) => println!("path:       {dir} is not on the PATH"),
        (Some(dir), None) => println!("path:       {dir} (the PATH could not be read)"),
        (None, _) => println!("path:       unknown"),
    }

    if !d.registered {
        println!("fix:        rgy-autostart install");
    } else if d.running == Some(false)
        && let Some(command) = &d.fix_run
    {
        println!("fix:        {command}");
    }
    if d.on_path == Some(false) {
        println!(
            "fix:        open a new terminal: the PATH of this session is older than the install"
        );
    }
}

/// The daemon, and where it was found.
///
/// The two places are looked at in the order the install looks at them,
/// so the answer says the same thing the install would do.
fn daemon_and_where() -> (Option<String>, &'static str) {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join(DAEMON_NAME);
        if candidate.is_file() && rgy_autostart::is_executable(&candidate) {
            return (Some(display_path(&candidate)), "next to this tool");
        }
    }
    match find_daemon() {
        Ok(path) => (Some(display_path(&path)), "on the PATH"),
        Err(_) => (None, "neither next to this tool nor on the PATH"),
    }
}

/// The directory this tool is in.
fn tool_dir() -> Option<String> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(display_path))
}

/// Whether the directory is on the PATH of this session.
fn on_path(dir: &str) -> Option<bool> {
    let path = std::env::var_os("PATH")?;
    Some(std::env::split_paths(&path).any(|entry| same_path(&display_path(&entry), dir)))
}

/// Whether two paths are the same one.
fn same_path(a: &str, b: &str) -> bool {
    #[cfg(windows)]
    {
        a.eq_ignore_ascii_case(b)
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

/// Print what an install did.
fn print_report(report: &Report) {
    println!("installed: {}", mechanism_label(report.mechanism));
    if let Some(artifact) = &report.artifact {
        println!("  file:    {}", artifact.display());
    }
    println!("  command: {}", report.command);
    // The tree the daemon indexes, read out of the command that was
    // registered: one source of truth, so the two cannot disagree.
    if let Some(root) = root_of(&report.command) {
        println!("  root:    {root}");
    }
    println!("  armed:   {}", if report.armed { "yes" } else { "no" });
    for note in &report.notes {
        println!("  {note}");
    }
}
