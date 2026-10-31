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
//!
//! A third rule is here for the same reason: a unix socket path has to fit
//! the platform's `sun_path`, and a path that does not fit is a daemon that
//! cannot start at all. The daemon that binds and the client that connects
//! shorten it the same way, and the shortening is here, so the two cannot
//! drift either.

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

/// The limit of the platform's `sun_path`, in bytes.
///
/// The header is `struct sockaddr_un { sa_family_t sun_family; char
/// sun_path[104]; }` on macOS and on the BSDs, and `char sun_path[108]` on
/// Linux, and the path has to be **shorter** than the array: the platform
/// refuses a longer one with EINVAL and a message that says "path must be
/// shorter than SUN_LEN".
///
/// Windows has no unix socket and does not use this number; it is here so
/// that the rule can be read — and tested — on every platform.
pub const SUN_PATH_MAX: usize = if cfg!(target_os = "linux") { 108 } else { 104 };

/// Why a socket path could not be worked out.
#[cfg(feature = "std")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SocketPathError {
    /// The path is longer than the platform's `sun_path`, and the machine
    /// has no home directory to put a shorter one in.
    ///
    /// There is nowhere private to put the socket: a socket in a folder
    /// another local user can write to is a socket another local user can
    /// take, and the daemon would then answer to whoever made it first.
    NoHome {
        /// The path the caller asked for.
        path: std::path::PathBuf,
        /// The platform's limit, in bytes.
        limit: usize,
    },
    /// The path is longer than the platform's `sun_path`, and the home
    /// directory is so long that the short path does not fit either.
    HomeTooLong {
        /// The path the caller asked for.
        path: std::path::PathBuf,
        /// The platform's limit, in bytes.
        limit: usize,
    },
}

#[cfg(feature = "std")]
impl core::fmt::Display for SocketPathError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SocketPathError::NoHome { path, limit } => write!(
                f,
                "the socket path is {} bytes and a unix socket holds fewer than {limit} bytes, \
                 and this machine has no home directory to put a shorter one in: {}",
                path.as_os_str().as_encoded_bytes().len(),
                path.display()
            ),
            SocketPathError::HomeTooLong { path, limit } => write!(
                f,
                "the socket path is {} bytes and a unix socket holds fewer than {limit} bytes, \
                 and the home directory is too long for a short path either: {}",
                path.as_os_str().as_encoded_bytes().len(),
                path.display()
            ),
        }
    }
}

/// The directory the short socket path lives in: the user's own cache,
/// under the home directory.
///
/// `XDG_CACHE_HOME` is ignored on purpose: the short path has to be
/// short, and `XDG_CACHE_HOME` is a path the user chose and can be as
/// long as they like. The home's own cache is `~/.cache` on Linux and
/// `~/Library/Caches` on macOS, and it belongs to the user alone — a
/// socket in a folder another local user can write to is a socket another
/// local user can take.
#[cfg(feature = "std")]
pub fn private_cache_dir_from(home: Option<&std::ffi::OsStr>) -> Option<std::path::PathBuf> {
    let home = home
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)?;
    if cfg!(target_os = "macos") {
        Some(home.join("Library").join("Caches"))
    } else {
        Some(home.join(".cache"))
    }
}

/// The user's own cache directory, where the short socket path lives.
///
/// `None` when the machine has no home to speak of.
#[cfg(feature = "std")]
pub fn private_cache_dir() -> Option<std::path::PathBuf> {
    private_cache_dir_from(std::env::var_os("HOME").as_deref())
}

/// The path a unix socket is at, given the path the caller asked for.
///
/// The rule is here and not in the daemon and the client, because the two
/// have to arrive at the same string: a daemon that bound one path and a
/// client that looked for another is a client that never answers, and it
/// fails silently — the daemon is up, the index is warm, and the launcher
/// says "no results".
///
/// * A path that fits the platform's `sun_path` ([`SUN_PATH_MAX`]) is used
///   as it is.
/// * A path that does not fit is replaced by a short path in the user's
///   own cache directory, named after a hash of the requested path's bytes:
///   the same request gives the same path in every process, and two
///   different requests give two different paths.
/// * A machine with no home has nowhere private to put the short path, and
///   the answer is [`SocketPathError::NoHome`]: a socket in a shared
///   folder is a socket another local user can take.
///
/// The rule is idempotent: a path that is already short is handed back
/// unchanged, so a client that was handed a short path does not shorten it
/// again.
#[cfg(feature = "std")]
pub fn socket_path(requested: &std::path::Path) -> Result<std::path::PathBuf, SocketPathError> {
    let private = private_cache_dir();
    socket_path_within(requested, private.as_deref())
}

/// The path both sides use, given the requested path and the private
/// directory to shorten it into.
///
/// Pure: the same two inputs always give the same path, in every process,
/// and a path that already fits is handed back unchanged.
#[cfg(feature = "std")]
fn socket_path_within(
    requested: &std::path::Path,
    private: Option<&std::path::Path>,
) -> Result<std::path::PathBuf, SocketPathError> {
    if fits_sun_path(requested) {
        return Ok(requested.to_path_buf());
    }
    let Some(private) = private else {
        return Err(SocketPathError::NoHome {
            path: requested.to_path_buf(),
            limit: SUN_PATH_MAX,
        });
    };
    // The name is the hash of the requested path's bytes: the same request
    // gives the same name in every process — the daemon and the client do
    // not share a memory — and two different requests give two different
    // names.
    let hash = fnv1a(requested.as_os_str().as_encoded_bytes());
    let short = private
        .join("rgy")
        .join(format!("searchd-{hash:016x}.sock"));
    if fits_sun_path(&short) {
        Ok(short)
    } else {
        Err(SocketPathError::HomeTooLong {
            path: requested.to_path_buf(),
            limit: SUN_PATH_MAX,
        })
    }
}

/// Whether the path fits the platform's `sun_path`.
#[cfg(feature = "std")]
fn fits_sun_path(path: &std::path::Path) -> bool {
    path.as_os_str().as_encoded_bytes().len() < SUN_PATH_MAX
}

/// The FNV-1a hash of the bytes, 64 bits.
///
/// The same hash `name-core`'s `persist::fnv1a` computes, spelled out here
/// because that one is private to its crate: the two have to be the same
/// function, and the tree has no shared crate for two hash functions.
#[cfg(feature = "std")]
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
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
    ///
    /// The path is the one the caller asked for; the path the socket is
    /// really at is [`socket_path`] of that path. The shortening happens
    /// where the socket is used — the daemon's bind and the client's
    /// connect — so no path reaches `bind` or `connect` unshortened, and
    /// the two sides cannot disagree about where the daemon is.
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

    /// The cache directory is the one the index uses.
    ///
    /// The rule is the index's own — `name-core`'s `persist::cache_dir_from`
    /// keeps a private copy of it, and the two have to agree: the socket
    /// and the index live in one directory, and a daemon that wrote its
    /// index somewhere else would be a daemon nobody finds.
    ///
    /// The rule is the same on every platform: `XDG_CACHE_HOME` wins when
    /// it is set, and only the fallback differs (`~/.cache` on Linux,
    /// `~/Library/Caches` on macOS, `%LOCALAPPDATA%` on Windows).
    #[cfg(feature = "std")]
    #[test]
    fn the_cache_directory_is_the_one_the_index_uses() {
        use std::ffi::OsStr;
        // A home that belongs to nobody: the rule is about the platform,
        // not about a person.
        let home = Some(OsStr::new("/home/u"));
        let xdg = Some(OsStr::new("/xdg"));
        if cfg!(windows) {
            // Windows keeps `%LOCALAPPDATA%`, and `%USERPROFILE%\AppData\Local`
            // when that is unset.
            assert_eq!(
                cache_dir_from(xdg, home, Some(OsStr::new("C:/Local"))),
                Some(std::path::PathBuf::from("C:/Local"))
            );
            assert_eq!(
                cache_dir_from(xdg, home, None),
                Some(
                    std::path::PathBuf::from("/home/u")
                        .join("AppData")
                        .join("Local")
                )
            );
        } else {
            // `XDG_CACHE_HOME` wins when it is set, on every platform: the
            // macOS branch of the rule is the fallback, not a way to keep
            // the home's cache when the environment names another one.
            assert_eq!(
                cache_dir_from(xdg, home, None),
                Some(std::path::PathBuf::from("/xdg")),
                "XDG_CACHE_HOME wins when it is set"
            );
            // Without it, the home's own cache is the platform's.
            let fallback = if cfg!(target_os = "macos") {
                std::path::PathBuf::from("/home/u")
                    .join("Library")
                    .join("Caches")
            } else {
                std::path::PathBuf::from("/home/u").join(".cache")
            };
            assert_eq!(
                cache_dir_from(None, home, None),
                Some(fallback),
                "the home's own cache is the fallback"
            );
        }
    }

    /// A path longer than every platform's limit.
    ///
    /// The limit is 104 bytes on macOS and 108 on Linux, and this is longer
    /// than both — so the test says the same thing on every platform, and
    /// it says it about a path that does not exist.
    #[cfg(feature = "std")]
    fn long_path(tag: &str) -> std::path::PathBuf {
        let mut path = std::path::PathBuf::from("/var/folders/");
        path.push("x".repeat(200));
        path.push(tag);
        path
    }

    /// A path that fits is the path both sides use.
    #[cfg(feature = "std")]
    #[test]
    fn a_path_that_fits_is_used_as_it_is() {
        let path = std::path::Path::new("/tmp/rgy/searchd.sock");
        assert_eq!(
            socket_path_within(path, None).expect("the path fits"),
            path,
            "a path the platform can carry is not touched"
        );
    }

    /// A path that does not fit is shortened into the private directory.
    #[cfg(feature = "std")]
    #[test]
    fn a_path_that_does_not_fit_is_shortened() {
        let long = long_path("one");
        let private = std::path::Path::new("/home/u/.cache");
        let short = socket_path_within(&long, Some(private)).expect("the short path fits");
        assert!(
            short.as_os_str().as_encoded_bytes().len() < SUN_PATH_MAX,
            "the short path fits the platform: {short:?}"
        );
        assert!(
            short.starts_with(private),
            "the short path is in the user's own directory: {short:?}"
        );
    }

    /// The short path is the same every time.
    ///
    /// The daemon and the client do not share a memory, and the daemon is
    /// a different process: a name that changed between them would be a
    /// client that never finds the daemon.
    #[cfg(feature = "std")]
    #[test]
    fn the_short_path_is_the_same_every_time() {
        let long = long_path("one");
        let private = std::path::Path::new("/home/u/.cache");
        let first = socket_path_within(&long, Some(private)).expect("the short path fits");
        let second = socket_path_within(&long, Some(private)).expect("the short path fits");
        assert_eq!(first, second, "the same request gives the same path");
    }

    /// Two paths do not shorten into one.
    #[cfg(feature = "std")]
    #[test]
    fn two_paths_do_not_shorten_into_one() {
        let private = std::path::Path::new("/home/u/.cache");
        let one =
            socket_path_within(&long_path("one"), Some(private)).expect("the short path fits");
        let two =
            socket_path_within(&long_path("two"), Some(private)).expect("the short path fits");
        assert_ne!(one, two, "two requests give two paths");
    }

    /// Shortening a short path changes nothing.
    #[cfg(feature = "std")]
    #[test]
    fn shortening_a_short_path_changes_nothing() {
        let private = std::path::Path::new("/home/u/.cache");
        let short =
            socket_path_within(&long_path("one"), Some(private)).expect("the short path fits");
        let again = socket_path_within(&short, Some(private)).expect("the short path fits");
        assert_eq!(again, short, "the short path is already short");
    }

    /// A machine with no home has nowhere private to put the socket.
    #[cfg(feature = "std")]
    #[test]
    fn a_machine_with_no_home_cannot_shorten_the_path() {
        let long = long_path("one");
        let err = socket_path_within(&long, None).expect_err("there is nowhere to put it");
        match err {
            SocketPathError::NoHome { path, limit } => {
                assert_eq!(path, long, "the error names the path that does not fit");
                assert_eq!(limit, SUN_PATH_MAX, "the error names the platform's limit");
            }
            other => panic!("the error is about the missing home: {other:?}"),
        }
    }

    #[cfg(feature = "std")]
    #[test]
    fn the_socket_sits_beside_the_index() {
        let base = std::path::Path::new("/cache");
        assert_eq!(socket_path_in(base), base.join("rgy").join("searchd.sock"));
    }
}
