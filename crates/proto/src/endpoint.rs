// SPDX-License-Identifier: Apache-2.0

//! Where a running daemon listens, and where a client goes to find it.
//!
//! Two rules, one per platform, and they are the same rules the login entry
//! is written with: a daemon registered to start at login has no standard
//! input, so it listens on a **named pipe** on Windows and on a **unix
//! socket** everywhere else.
//!
//! The rule lives here, and not in two places, because the registration and
//! the lookup have to agree on the same string: a client that looks in the
//! wrong place is a client that never answers, and it fails silently — the
//! daemon is up, the index is warm, and the launcher says "no results".
//!
//! `rgy-autostart` writes the registration and reads these functions, and so
//! does the client, so the two cannot drift.

use alloc::format;
use alloc::string::{String, ToString};

/// The daemon's socket file name, inside the cache directory.
pub const SOCKET_NAME: &str = "searchd.sock";

/// The name of the named pipe the registered daemon listens on, for a user.
///
/// The pipe namespace belongs to the machine and not to the user, so the
/// user's own name is part of the name: two people logged into the same
/// Windows have two daemons, and each one answers on its own pipe.
///
/// The name is spelled with the characters a pipe name can carry, and
/// anything else becomes an underscore — a name the platform will refuse
/// is a service that does not start.
pub fn pipe_name_for(user: &str) -> String {
    let clean: String = user
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if clean.is_empty() {
        "rgy-searchd".to_string()
    } else {
        format!("rgy-searchd-{clean}")
    }
}

/// The named pipe the registered daemon listens on.
///
/// The name of the current user, from the environment: `USERNAME` on
/// Windows, which is what the pipe namespace is per-user there.
#[cfg(feature = "std")]
pub fn pipe_name() -> String {
    let user = std::env::var("USERNAME").unwrap_or_default();
    if user.is_empty() {
        // A shell that does not carry the name still gets a daemon it can
        // reach: the unnamed form is the one the fallback registration uses.
        return "rgy-searchd".to_string();
    }
    pipe_name_for(&user)
}

/// The directory the platform keeps its cache in, given the environment.
///
/// The same shape the daemon's own index uses (`name-core`'s
/// `persist::cache_dir_from`): `XDG_CACHE_HOME` when it is set, else
/// `~/.cache` on Linux and `~/Library/Caches` on macOS; Windows keeps
/// `%LOCALAPPDATA%`, and `%USERPROFILE%\AppData\Local` when that is unset.
///
/// The socket goes in the same place, so the daemon's index and the socket
/// the clients reach it through are in one directory.
///
/// Split from the path logic so the test does not have to mutate the
/// process environment: a test that sets `HOME` races every other test in
/// the process.
#[cfg(feature = "std")]
pub fn cache_dir_from(
    xdg: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
    local: Option<&std::ffi::OsStr>,
) -> Option<std::path::PathBuf> {
    let some =
        |v: Option<&std::ffi::OsStr>| v.filter(|s| !s.is_empty()).map(std::path::PathBuf::from);
    if cfg!(windows) {
        return some(local).or_else(|| some(home).map(|h| h.join("AppData").join("Local")));
    }
    if cfg!(target_os = "macos") {
        return some(xdg).or_else(|| some(home).map(|h| h.join("Library").join("Caches")));
    }
    some(xdg).or_else(|| some(home).map(|h| h.join(".cache")))
}

/// The platform's cache directory.
///
/// `None` when the platform has no home to speak of: a machine with
/// neither `XDG_CACHE_HOME` nor `HOME` has nowhere to keep a socket, and
/// the install says so rather than inventing a path in a shared directory.
#[cfg(feature = "std")]
pub fn cache_dir() -> Option<std::path::PathBuf> {
    let home = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    cache_dir_from(
        std::env::var_os("XDG_CACHE_HOME").as_deref(),
        std::env::var_os(home).as_deref(),
        std::env::var_os("LOCALAPPDATA").as_deref(),
    )
}

/// The socket the registered daemon listens on, under a cache directory.
///
/// It lives in the `rgy` directory the daemon already keeps its index in:
/// one directory for this root's files, in the place the platform owns.
#[cfg(feature = "std")]
pub fn socket_path_in(cache_dir: &std::path::Path) -> std::path::PathBuf {
    cache_dir.join("rgy").join(SOCKET_NAME)
}

/// Where a client connects to talk to a daemon.
///
/// The three shapes the protocol travels over, and they are the same three
/// the daemon serves: standard input/output, a unix socket, and a Windows
/// named pipe.
#[cfg(feature = "std")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// The daemon's own standard input and output. A client that spawns the
    /// daemon and speaks to it over the pipes it gave it.
    Stdio,
    /// A unix domain socket.
    Socket(std::path::PathBuf),
    /// A Windows named pipe, by name — the `\\.\pipe\` prefix is the
    /// platform's, not the caller's.
    Pipe(String),
}

#[cfg(feature = "std")]
impl Endpoint {
    /// The endpoint the registered daemon listens on, on this platform.
    ///
    /// The answer is the environment's: the registration wrote it there, so
    /// this is where the client looks. When nothing is registered there is
    /// nothing to connect to, and the caller is told so rather than being
    /// sent to a path that does not exist.
    pub fn registered() -> Option<Endpoint> {
        if cfg!(windows) {
            Some(Endpoint::Pipe(pipe_name()))
        } else {
            let cache = cache_dir()?;
            Some(Endpoint::Socket(socket_path_in(&cache)))
        }
    }

    /// A human sentence for this endpoint, for a message a person reads.
    pub fn describe(&self) -> String {
        match self {
            Endpoint::Stdio => "stdio".to_string(),
            Endpoint::Socket(p) => format!("uds:{}", p.display()),
            Endpoint::Pipe(n) => format!("pipe:{n}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pipe_name_carries_the_user() {
        assert_eq!(pipe_name_for("dana"), "rgy-searchd-dana");
    }

    #[test]
    fn a_pipe_name_drops_what_a_pipe_cannot_carry() {
        // A space, a dot and a slash are all legal in a user name and none
        // of them is legal in a pipe name.
        assert_eq!(pipe_name_for("a b.c/d"), "rgy-searchd-a_b_c_d");
    }

    #[test]
    fn an_empty_user_gets_the_unnamed_form() {
        assert_eq!(pipe_name_for(""), "rgy-searchd");
    }

    #[test]
    fn the_pipe_and_the_socket_are_named_once() {
        // The registration writes these; a client that looked for a
        // different name would never find the daemon.
        assert_eq!(SOCKET_NAME, "searchd.sock");
    }

    #[cfg(feature = "std")]
    #[test]
    fn the_cache_directory_is_the_one_the_index_uses() {
        use std::ffi::OsStr;
        let home = Some(OsStr::new("/home/dana"));
        let xdg = Some(OsStr::new("/xdg"));
        if cfg!(windows) {
            assert_eq!(
                cache_dir_from(xdg, home, Some(OsStr::new("C:/Local"))),
                Some(std::path::PathBuf::from("C:/Local"))
            );
            assert_eq!(
                cache_dir_from(xdg, home, None),
                Some(std::path::PathBuf::from("/home/dana").join("AppData").join("Local"))
            );
        } else if cfg!(target_os = "macos") {
            assert_eq!(
                cache_dir_from(xdg, home, None),
                Some(std::path::PathBuf::from("/home/dana").join("Library").join("Caches"))
            );
        } else {
            assert_eq!(
                cache_dir_from(xdg, home, None),
                Some(std::path::PathBuf::from("/xdg"))
            );
            assert_eq!(
                cache_dir_from(None, home, None),
                Some(std::path::PathBuf::from("/home/dana").join(".cache"))
            );
        }
    }

    #[cfg(feature = "std")]
    #[test]
    fn the_socket_sits_beside_the_index() {
        let base = std::path::Path::new("/cache");
        assert_eq!(
            socket_path_in(base),
            base.join("rgy").join("searchd.sock")
        );
    }
}
