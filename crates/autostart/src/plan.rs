// SPDX-License-Identifier: Apache-2.0

//! Which platform, which mechanism — decided before anything is touched.
//!
//! The decision is a pure function on purpose. The choice between the task
//! scheduler and the registry is the one place where this tool can quietly
//! do the weaker thing, and a pure function is testable from any operating
//! system: the rule is written down once and every platform obeys it.

use alloc::format;
use alloc::string::String;

/// The operating systems this tool knows how to arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// Windows: the task scheduler, or the registry as a fallback.
    Windows,
    /// Linux: a systemd user unit.
    Linux,
    /// macOS: a launchd agent.
    MacOs,
    /// Anything else: the tool says so and does nothing.
    Unsupported,
}

impl Platform {
    /// The name the code uses.
    pub fn name(self) -> &'static str {
        match self {
            Platform::Windows => "windows",
            Platform::Linux => "linux",
            Platform::MacOs => "macos",
            Platform::Unsupported => "unsupported",
        }
    }
}

/// The mechanism that puts the daemon in place at login.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mechanism {
    /// A Task Scheduler task with a logon trigger (Windows).
    StartupTask,
    /// The `HKCU\...\Run` value (Windows, when the task tool is not there).
    RegistryRun,
    /// A systemd **user** unit (Linux).
    SystemdUserUnit,
    /// A launchd agent (macOS).
    LaunchdAgent,
}

impl Mechanism {
    /// The name, in the user's words.
    pub fn name(self) -> &'static str {
        match self {
            Mechanism::StartupTask => "startup task",
            Mechanism::RegistryRun => "HKCU Run value",
            Mechanism::SystemdUserUnit => "systemd user unit",
            Mechanism::LaunchdAgent => "launchd agent",
        }
    }

    /// Whether this is the fallback, and therefore the weaker mechanism.
    ///
    /// The registry value is the exception: it is what is left when the
    /// task scheduler is not there, and it is worth less — no Task
    /// Scheduler entry to look at, no restart policy, and nothing the user
    /// can see without opening regedit.
    pub fn is_fallback(self) -> bool {
        matches!(self, Mechanism::RegistryRun)
    }
}

/// The platform this build runs on.
pub fn detect_platform() -> Platform {
    #[cfg(target_os = "windows")]
    {
        Platform::Windows
    }
    #[cfg(target_os = "linux")]
    {
        Platform::Linux
    }
    #[cfg(target_os = "macos")]
    {
        Platform::MacOs
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        Platform::Unsupported
    }
}

/// The name the autostart entry gets on this platform, unless the user
/// picks another one.
///
/// The launchd label is a reverse-DNS name, because that is what launchd
/// expects; the other two are plain names.
pub fn default_name(platform: Platform) -> &'static str {
    match platform {
        Platform::MacOs => "com.rgy.searchd",
        _ => "rgy-searchd",
    }
}

/// The mechanism to use on `platform`.
///
/// `startup_task_tool` says whether the task scheduler's command line is
/// there to be used; without it the only mechanism left on Windows is the
/// registry value. `None` means the platform has no mechanism at all.
pub fn choose_mechanism(platform: Platform, startup_task_tool: bool) -> Option<Mechanism> {
    match platform {
        Platform::Windows => Some(if startup_task_tool {
            Mechanism::StartupTask
        } else {
            Mechanism::RegistryRun
        }),
        Platform::Linux => Some(Mechanism::SystemdUserUnit),
        Platform::MacOs => Some(Mechanism::LaunchdAgent),
        Platform::Unsupported => None,
    }
}

/// The transport the registered daemon listens on.
///
/// The daemon's own default is stdio, and stdio has an end: a daemon
/// started at login has its standard input closed, so it reads one EOF and
/// exits. The entry is registered, the daemon starts, it walks the tree,
/// and then it is gone — and `Restart=on-failure` does not bring it back,
/// because an exit with a zero status is not a failure. The service looks
/// armed, and it is dead.
///
/// A registered daemon listens on a transport that has no end instead: a
/// unix socket on the unix platforms, a named pipe on Windows. Both wait
/// for a client, and neither one ever sees an end of file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// A unix socket, at a path.
    UnixSocket,
    /// A Windows named pipe — `\\.\pipe\<name>`.
    NamedPipe,
}

/// The transport this platform's registered daemon listens on.
///
/// `None` for a platform this tool does not arm at all.
pub fn transport(platform: Platform) -> Option<Transport> {
    match platform {
        Platform::Windows => Some(Transport::NamedPipe),
        Platform::Linux | Platform::MacOs => Some(Transport::UnixSocket),
        Platform::Unsupported => None,
    }
}

/// The `--listen` argument that names the transport.
///
/// The daemon's own spelling: `uds:<path>` for a unix socket, and
/// `pipe:<name>` for a named pipe — the daemon puts the `\\.\pipe\`
/// prefix on the name itself, so the argument carries the name alone.
pub fn listen_arg(transport: Transport, endpoint: &str) -> String {
    match transport {
        Transport::UnixSocket => format!("uds:{endpoint}"),
        Transport::NamedPipe => format!("pipe:{endpoint}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_gets_a_systemd_user_unit() {
        assert_eq!(
            choose_mechanism(Platform::Linux, true),
            Some(Mechanism::SystemdUserUnit)
        );
        assert_eq!(
            choose_mechanism(Platform::Linux, false),
            Some(Mechanism::SystemdUserUnit)
        );
    }

    #[test]
    fn macos_gets_a_launchd_agent() {
        assert_eq!(
            choose_mechanism(Platform::MacOs, true),
            Some(Mechanism::LaunchdAgent)
        );
    }

    #[test]
    fn windows_prefers_the_task_and_falls_back_to_the_registry() {
        assert_eq!(
            choose_mechanism(Platform::Windows, true),
            Some(Mechanism::StartupTask)
        );
        assert_eq!(
            choose_mechanism(Platform::Windows, false),
            Some(Mechanism::RegistryRun)
        );
        assert!(Mechanism::RegistryRun.is_fallback());
        assert!(!Mechanism::StartupTask.is_fallback());
    }

    #[test]
    fn an_unknown_platform_has_no_mechanism() {
        assert_eq!(choose_mechanism(Platform::Unsupported, true), None);
    }

    #[test]
    fn the_platform_agrees_with_the_build() {
        let expected = if cfg!(target_os = "windows") {
            Platform::Windows
        } else if cfg!(target_os = "linux") {
            Platform::Linux
        } else if cfg!(target_os = "macos") {
            Platform::MacOs
        } else {
            Platform::Unsupported
        };
        assert_eq!(detect_platform(), expected);
    }

    #[test]
    fn the_launchd_label_is_a_reverse_dns_name() {
        assert_eq!(default_name(Platform::MacOs), "com.rgy.searchd");
        assert_eq!(default_name(Platform::Linux), "rgy-searchd");
        assert_eq!(default_name(Platform::Windows), "rgy-searchd");
    }

    #[test]
    fn the_unix_platforms_listen_on_a_socket_and_windows_on_a_pipe() {
        assert_eq!(
            transport(Platform::Linux),
            Some(Transport::UnixSocket),
            "a unix socket has no end of file"
        );
        assert_eq!(transport(Platform::MacOs), Some(Transport::UnixSocket));
        assert_eq!(transport(Platform::Windows), Some(Transport::NamedPipe));
        assert_eq!(transport(Platform::Unsupported), None);
    }

    #[test]
    fn the_listen_argument_is_the_daemons_own_spelling() {
        assert_eq!(
            listen_arg(Transport::UnixSocket, "/home/u/.cache/rgy/searchd.sock"),
            "uds:/home/u/.cache/rgy/searchd.sock"
        );
        assert_eq!(
            listen_arg(Transport::NamedPipe, "rgy-searchd-ana"),
            "pipe:rgy-searchd-ana"
        );
    }
}
