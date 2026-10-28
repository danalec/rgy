// SPDX-License-Identifier: Apache-2.0

//! Which platform, which mechanism — decided before anything is touched.
//!
//! The decision is a pure function on purpose. The choice between the task
//! scheduler and the registry is the one place where this tool can quietly
//! do the weaker thing, and a pure function is testable from any operating
//! system: the rule is written down once and every platform obeys it.

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
}
