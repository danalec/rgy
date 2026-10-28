// SPDX-License-Identifier: Apache-2.0

//! The programs the launcher can call, and what it does when they are
//! there and when they are not.
//!
//! The launcher is a **client**: the index lives in the daemon, and the
//! daemon is what answers. But a client that only works when another
//! program is running is a client that works on one machine, so the
//! launcher knows one fallback — the `rgx` binary, which carries the same
//! adapters the daemon carries — and it uses it when the daemon is not
//! there.
//!
//! The decision is a pure function on purpose: "which backend answers the
//! content layer" is a question about the machine, and the part of it that
//! is not about the machine can be tested from anywhere.

use alloc::string::String;

/// The external programs the launcher looks for.
pub const KNOWN_TOOLS: &[&str] = &["rgx"];

/// The words the launcher uses when the daemon is not there.
///
/// It is a constant because it is a contract with two other things: the
/// window's status line, and the one-shot query, which says it on stderr
/// so that "no results" and "the daemon is not there" are not the same
/// answer wearing the same exit status.
pub const NO_DAEMON: &str = "sem ligação ao daemon";

/// Where the content layer's answer comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentBackend {
    /// The daemon, which carries the rgx adapters and, when it was started
    /// with a pack store, the pack store itself.
    Daemon,
    /// The `rgx` binary, run by the launcher.
    Rgx,
    /// Nothing on this machine can answer it.
    None,
}

impl ContentBackend {
    /// The word the status line shows.
    pub fn label(&self) -> &'static str {
        match self {
            ContentBackend::Daemon => "adaptadores",
            ContentBackend::Rgx => "rgx",
            ContentBackend::None => "sem conteúdo",
        }
    }

    /// Whether this backend can answer at all.
    pub fn is_available(&self) -> bool {
        !matches!(self, ContentBackend::None)
    }
}

/// The backend that answers the content layer.
///
/// The daemon first, always: its index is warm in another process and the
/// answer is a re-rank instead of a walk. The `rgx` binary is the
/// fallback for the case the daemon is not there, and it is a fallback
/// only when it is on the `PATH` — a launcher that promised a content
/// search it cannot run is worse than one that says it cannot.
pub fn content_backend(daemon_up: bool, rgx_present: bool) -> ContentBackend {
    if daemon_up {
        ContentBackend::Daemon
    } else if rgx_present {
        ContentBackend::Rgx
    } else {
        ContentBackend::None
    }
}

/// The sentence the launcher shows when the content layer cannot answer.
///
/// It names what is missing, because the fix is not the same in each case:
/// one is a daemon that is not running, the other is a program that is
/// not installed.
pub fn content_unavailable(daemon_up: bool, rgx_present: bool) -> String {
    if daemon_up {
        String::new()
    } else if rgx_present {
        String::from(
            "o daemon não está a correr; a camada de conteúdo usa o rgx, sobre o escopo da última busca",
        )
    } else {
        String::from(
            "a camada de conteúdo precisa do daemon (searchd) ou do rgx no PATH",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn the_daemon_is_the_first_backend() {
        assert_eq!(content_backend(true, true), ContentBackend::Daemon);
        assert_eq!(content_backend(true, false), ContentBackend::Daemon);
    }

    #[test]
    fn the_rgx_binary_is_the_fallback() {
        assert_eq!(content_backend(false, true), ContentBackend::Rgx);
    }

    #[test]
    fn without_a_daemon_and_without_rgx_there_is_no_content_layer() {
        assert_eq!(content_backend(false, false), ContentBackend::None);
        assert!(!ContentBackend::None.is_available());
        assert!(ContentBackend::Daemon.is_available());
        assert!(ContentBackend::Rgx.is_available());
    }

    #[test]
    fn the_sentence_names_what_is_missing() {
        assert_eq!(content_unavailable(true, false), "".to_string());
        let without_daemon = content_unavailable(false, false);
        assert!(without_daemon.contains("searchd"), "{without_daemon}");
        assert!(without_daemon.contains("rgx"), "{without_daemon}");
        let with_rgx = content_unavailable(false, true);
        assert!(with_rgx.contains("rgx"), "{with_rgx}");
        assert!(with_rgx.contains("escopo"), "{with_rgx}");
    }

    #[test]
    fn the_tool_the_launcher_looks_for_is_rgx() {
        assert!(KNOWN_TOOLS.contains(&"rgx"));
    }
}
