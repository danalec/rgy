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
//!     rgy-autostart install --binary /usr/local/bin/searchd
//!     rgy-autostart status
//!     rgy-autostart uninstall
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
use rgy_autostart::plan::{Mechanism, Platform, choose_mechanism, default_name, detect_platform};
use rgy_autostart::render::{reg_add_command_line, schtasks_command_line, windows_task_command};
use rgy_autostart::windows;
use rgy_autostart::{Removal, Report, mechanism_label, resolve_binary};

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
           rgy-autostart install --binary <path> [--args \"<args>\"] [--name <name>]\n  \
           rgy-autostart uninstall [--name <name>]\n  \
           rgy-autostart status [--name <name>]\n\
         \n\
         options:\n  \
           --binary <path>   the daemon to start (required to install)\n  \
           --args \"<args>\"    arguments handed to the daemon\n  \
           --name <name>     the name of the entry (default: {default})\n  \
           -h, --help        this text\n  \
           -V, --version     the version\n\
         \n\
         The daemon is registered into your own session: no administrator,\n\
         no prompt, no dialog. Uninstalling removes what was installed.",
        version = env!("CARGO_PKG_VERSION"),
        default = default_name(detect_platform()),
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

    let Some(binary) = options.binary.as_ref() else {
        return Err(format!("install needs the daemon's path:\n\n{}", usage()));
    };
    let binary = resolve_binary(binary)?;
    let name = entry_name(options, platform);
    let args = options.args.clone();

    let report = match platform {
        Platform::Windows => install_windows(&name, &binary, &args)?,
        Platform::Linux => install_linux(&name, &binary, &args)?,
        Platform::MacOs => install_macos(&name, &binary, &args)?,
        Platform::Unsupported => return Err("autostart is not implemented here".to_string()),
    };

    print_report(&report);
    // 2 says what happened without dressing it up: the entry is written,
    // the platform could not arm it, and the note says how.
    Ok(if report.armed {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
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
    let name = entry_name(options, platform);

    let removal = match platform {
        Platform::Windows => uninstall_windows(&name)?,
        Platform::Linux => uninstall_linux(&name)?,
        Platform::MacOs => uninstall_macos(&name)?,
        Platform::Unsupported => return Err("autostart is not implemented here".to_string()),
    };

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
    Ok(ExitCode::SUCCESS)
}

/// The Windows side: the task, and the registry value that may stand in
/// for it.
fn uninstall_windows(name: &str) -> Result<Removal, String> {
    let mut notes = Vec::new();
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
        Err(reason) => notes.push(reason),
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
        Err(reason) => notes.push(reason),
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

    match linux::disable(&unit_name) {
        Ok(_) => notes.push(format!(
            "the unit was stopped: systemctl --user disable --now {unit_name}"
        )),
        Err(reason) => notes.push(format!(
            "the unit could not be stopped through systemd ({reason}); the file is removed anyway"
        )),
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

    match macos::unload(&path, name) {
        Ok(true) => notes.push("the agent was unloaded".to_string()),
        Ok(false) => notes.push("the agent was not loaded".to_string()),
        Err(reason) => notes.push(reason),
    }

    let removed = macos::remove_plist(&home, name)
        .map_err(|e| format!("cannot remove {}: {e}", path.display()))?;

    notes.push(if removed {
        format!("the plist is gone: {}", path.display())
    } else {
        format!("there was no plist at {}", path.display())
    });

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
    Ok(ExitCode::SUCCESS)
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
    println!("  plist:   {}", macos::plist_path_in(&home, name).display());
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

/// Print what an install did.
fn print_report(report: &Report) {
    println!("installed: {}", mechanism_label(report.mechanism));
    if let Some(artifact) = &report.artifact {
        println!("  file:    {}", artifact.display());
    }
    println!("  command: {}", report.command);
    println!("  armed:   {}", if report.armed { "yes" } else { "no" });
    for note in &report.notes {
        println!("  {note}");
    }
}
