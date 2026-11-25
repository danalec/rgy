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

use crate::text::{Lang, Text};

/// The external programs the launcher looks for.
pub const KNOWN_TOOLS: &[&str] = &["rgx"];

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
    /// The word the status line shows, in the language the user reads.
    pub fn label(&self, lang: Lang) -> &'static str {
        let text = Text::for_lang(lang);
        match self {
            ContentBackend::Daemon => text.adapters,
            ContentBackend::Rgx => text.rgx,
            ContentBackend::None => text.no_content,
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn the_tool_the_launcher_looks_for_is_rgx() {
        assert!(KNOWN_TOOLS.contains(&"rgx"));
    }

    /// The backend's word is said in the language the user reads.
    #[test]
    fn the_backend_label_is_in_the_users_language() {
        assert_eq!(ContentBackend::Daemon.label(Lang::English), "adapters");
        assert_eq!(
            ContentBackend::Daemon.label(Lang::Portuguese),
            "adaptadores"
        );
        assert_eq!(ContentBackend::None.label(Lang::English), "no content");
        assert_eq!(ContentBackend::None.label(Lang::Portuguese), "sem conteúdo");
    }
}
