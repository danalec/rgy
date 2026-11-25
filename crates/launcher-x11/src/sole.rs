// SPDX-License-Identifier: Apache-2.0

//! One launcher per session.
//!
//! The launcher is resident, and everything it holds is a **global**: a
//! passive key grab that sees every modifier press on the machine, an
//! icon in the system tray, a topmost window, a clock. A second `rgxy`
//! is a second one of each, and the second one is exactly what a user
//! who forgot the hotkey gets when they start the launcher again.
//!
//! On Linux the claim is an **abstract Unix-domain socket**: the kernel
//! gives the name to exactly one live socket and takes it back when the
//! process dies, so a launcher that was killed is a launcher that can
//! be started again without a stale lock file. FreeBSD has **no abstract
//! sockets**, so there the claim is a real `/tmp` path: `take` probes
//! with `connect` first (so it never deletes a live launcher's file),
//! `unlink`s a stale file before `bind`, and `Sole`'s `Drop` removes it
//! on a clean exit. The second process finds the name taken, connects to
//! the first one's listening socket and the window pump reads that
//! connection as "show yourself".
//!
//! The name carries the **uid** and a hash of `DISPLAY`: two logged-in
//! users have two launchers, and a launcher on another display is not
//! this display's launcher. Both facts are in the name, and the name is
//! short enough to fit `sun_path` whole (108 bytes on Linux, 104 on
//! FreeBSD).
//!
//! # `no_std`
//!
//! `alloc`, `core` and the [`crate::x`] seam. [`display_hash`] and
//! [`socket_name`] are pure decisions about bytes, with tests that run
//! on the host that has no X server.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::{c_int, c_void};
use core::sync::atomic::{AtomicI32, Ordering};

use crate::{runtime, x};

/// The listening socket of this session's one launcher, or `-1`.
///
/// The window pump polls this fd alongside the X connection; a readable
/// listener is a second launcher asking to be shown.
static LISTEN: AtomicI32 = AtomicI32::new(-1);

/// What the launcher found when it asked for the session.
#[derive(Debug)]
pub enum Taken {
    /// This process is the session's launcher, and holds the socket.
    First(Sole),
    /// Another launcher holds the name: this process is the second.
    Other,
    /// The platform would not answer the question, with the reason to
    /// say. The launcher runs anyway: a broken socket must not be the
    /// one thing that can stop it.
    Unknown(String),
}

/// The claim a launcher takes on its session.
///
/// The fd is the claim: it is closed when this struct is dropped, and
/// the kernel gives the abstract name back with it. On FreeBSD the name
/// is a filesystem path, and `path` is the NUL-terminated C string that
/// `Drop` `unlink`s after closing the fd.
#[derive(Debug)]
pub struct Sole {
    fd: i32,
    /// The NUL-terminated socket path, removed on drop (FreeBSD only;
    /// Linux's abstract name leaves no file behind).
    #[cfg(target_os = "freebsd")]
    path: Vec<u8>,
}

/// FNV-1a over the bytes of `display`.
///
/// A small, stable hash is all the socket name needs: it turns a display
/// string such as `:0` or `wayland-0` into sixteen hex digits that differ
/// between displays and agree for the same one, with no dependency on a
/// hasher the `no_std` crate does not have. Empty input is fine — it
/// hashes to the FNV offset basis.
pub fn display_hash(s: &str) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for byte in s.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// The single-instance socket's name, as bytes: abstract on Linux, a
/// filesystem path on FreeBSD.
///
/// Linux: `b"\0rgy-rgxy-launcher-<uid>-<hash>"`. The leading NUL makes
/// it an **abstract** socket (no filesystem entry, no stale file to clean
/// up), and no trailing NUL is needed — the length passed to `bind` says
/// where the name ends. FreeBSD has **no abstract sockets**, so its name
/// is a real path `/tmp/rgy-rgxy-launcher-<uid>-<hash>.sock` with neither
/// a leading nor a trailing NUL (`address` adds the terminating NUL). A
/// killed launcher can leave that file behind; `take` handles it with a
/// connect-first probe and an `unlink` before `bind`. Sixteen hex digits
/// of a FNV-1a hash of `DISPLAY` keep two displays apart on either host.
pub fn socket_name(uid: u32, display: &str) -> Vec<u8> {
    let hash = display_hash(display);

    // Linux: an abstract name — a leading NUL, no filesystem entry. The
    // hash above is computed once for both hosts.
    #[cfg(target_os = "linux")]
    let name = format!("\0rgy-rgxy-launcher-{uid}-{hash:016x}");
    // FreeBSD has NO abstract sockets: a real /tmp path instead. `address`
    // adds the terminating NUL; `take` unlinks a stale file before bind.
    #[cfg(target_os = "freebsd")]
    let name = format!("/tmp/rgy-rgxy-launcher-{uid}-{hash:016x}.sock");

    name.into_bytes()
}

/// The `DISPLAY` environment variable, or `":0"` when it is absent or
/// empty.
///
/// `getenv` gives a pointer into libc's own storage; `strlen` says how
/// long it is and `from_raw_parts` reads exactly that many bytes. A
/// display is ASCII in practice, and `from_utf8_lossy` keeps the hash
/// defined even for a byte that is not.
fn display_env() -> String {
    let ptr = unsafe { x::getenv(c"DISPLAY".as_ptr()) };
    if ptr.is_null() {
        return String::from(":0");
    }
    let len = unsafe { x::strlen(ptr) };
    if len == 0 {
        return String::from(":0");
    }
    let bytes = unsafe { core::slice::from_raw_parts(ptr.cast::<u8>(), len) };
    String::from_utf8_lossy(bytes).into_owned()
}

/// Build the Linux `sockaddr_un` for the session's abstract name.
///
/// `family` is `AF_UNIX` and `path` starts with the abstract marker
/// (zero). The 108-byte buffer is filled with zeros first, so the bytes
/// after the name are the kernel's, not ours.
#[cfg(target_os = "linux")]
fn address(name: &[u8]) -> (x::SockaddrUn, u32) {
    let mut addr = x::SockaddrUn {
        family: x::AF_UNIX as u16,
        path: [0u8; 108],
    };
    let copied = name.len().min(addr.path.len());
    addr.path[..copied].copy_from_slice(&name[..copied]);
    // `offsetof(sockaddr_un, sun_path)` is 2 on this ABI; the length of
    // an abstract address is that offset plus the name's own bytes.
    (addr, (2 + name.len()) as u32)
}

/// Build the FreeBSD `sockaddr_un` for the session's filesystem path.
///
/// BSD `sockaddr` structs carry a leading `sun_len` byte, so `len` is set
/// to the whole address length (2 header bytes + the path + its NUL),
/// `family` is a one-byte `AF_UNIX`, and `path` holds the real `/tmp` name.
/// `sun_path` must be NUL-terminated on FreeBSD (no abstract sockets); the
/// `+1` in the length is that terminating NUL.
#[cfg(target_os = "freebsd")]
fn address(name: &[u8]) -> (x::SockaddrUn, u32) {
    let mut addr = x::SockaddrUn {
        len: (2 + name.len() + 1) as u8,
        family: x::AF_UNIX as u8,
        path: [0u8; 104],
    };
    addr.path[..name.len()].copy_from_slice(name);
    // The zero-filled buffer already holds the NUL; setting it by index
    // makes the contract explicit, and matches the `+1` in `len`.
    addr.path[name.len()] = 0;
    (addr, (2 + name.len() + 1) as u32)
}

/// A socket of the type the claim uses, or `-1`.
fn new_socket() -> c_int {
    unsafe { x::socket(x::AF_UNIX, x::SOCK_STREAM | x::SOCK_CLOEXEC, 0) }
}

/// The FreeBSD socket path as a NUL-terminated C string, for `unlink`.
///
/// `name` is a real `/tmp` path with no NUL of its own; libc's `unlink`
/// needs one. The same buffer is kept in `Sole.path` so `Drop` can remove
/// the file again on a clean exit. Linux's abstract name leaves no file
/// behind and needs no C string.
#[cfg(target_os = "freebsd")]
fn c_path(name: &[u8]) -> Vec<u8> {
    let mut path = Vec::with_capacity(name.len() + 1);
    path.extend_from_slice(name);
    path.push(0);
    path
}

/// Take the session's name, and learn what this process is.
///
/// The `connect` probe comes FIRST, before `bind` and before any `unlink`:
/// a peer that answers means a live launcher owns the name, so this is the
/// second launcher (`Other`) and the running one's claim is left alone.
/// With no `errno` accessor in the seam, a failed connect followed by a
/// failed `bind` is the honest `Unknown` — the platform would not let us
/// take a name nobody is listening on. That order matters on FreeBSD,
/// where the name is a filesystem path: a stale file must be removed
/// before `bind`, and only the failed connect proves no live launcher
/// still owns it.
pub fn take() -> Taken {
    let uid = unsafe { x::getuid() };
    let name = socket_name(uid, &display_env());
    let (addr, len) = address(&name);

    let fd = new_socket();
    if fd < 0 {
        return Taken::Unknown("the single-instance socket could not be opened".to_string());
    }

    // FIRST: is a launcher already listening on this name? A connect that
    // succeeds means a live peer, so close both fds and step aside. On
    // FreeBSD this is what keeps us from unlinking a running launcher's
    // socket file; only a failed connect proves the file is safe to remove.
    let peer = new_socket();
    let alive = peer >= 0 && unsafe { x::connect(peer, &addr, len) } == 0;
    if peer >= 0 {
        unsafe {
            x::close(peer);
        }
    }
    if alive {
        unsafe {
            x::close(fd);
        }
        return Taken::Other;
    }

    // FreeBSD: the name is a real /tmp path. A launcher that was killed
    // left its socket file behind; the failed connect above proved nobody
    // is listening, so it is safe to remove before bind.
    #[cfg(target_os = "freebsd")]
    let cpath = c_path(&name);
    #[cfg(target_os = "freebsd")]
    unsafe {
        x::unlink(cpath.as_ptr().cast());
    }

    let bound = unsafe { x::bind(fd, &addr, len) };
    if bound != 0 {
        // A bind that lost the race to another launcher is not a platform
        // refusal: probe once more, and step aside if a peer now answers.
        // The first probe above was before either process could bind.
        let other = new_socket();
        let alive = other >= 0 && unsafe { x::connect(other, &addr, len) } == 0;
        if other >= 0 {
            unsafe {
                x::close(other);
            }
        }
        unsafe {
            x::close(fd);
        }
        if alive {
            return Taken::Other;
        }
        return Taken::Unknown(format!(
            "the single-instance socket could not be bound ({bound})"
        ));
    }

    unsafe {
        x::listen(fd, 8);
    }
    LISTEN.store(fd, Ordering::Release);
    runtime::set_sole_listen(fd);

    #[cfg(target_os = "linux")]
    let sole = Sole { fd };
    #[cfg(target_os = "freebsd")]
    let sole = Sole { fd, path: cpath };
    Taken::First(sole)
}

/// Ask the launcher that is already running to show itself.
///
/// One byte (`'S'`) on a connected socket is the whole request; the
/// running launcher's pump sees its listener become readable and calls
/// [`accept_show`]. True when the connection was made, false when there
/// was nobody to connect to.
pub fn ask_the_running_one_to_show() -> bool {
    let uid = unsafe { x::getuid() };
    let name = socket_name(uid, &display_env());
    let (addr, len) = address(&name);
    let fd = new_socket();
    if fd < 0 {
        return false;
    }
    let connected = unsafe { x::connect(fd, &addr, len) } == 0;
    if connected {
        let byte = b'S';
        unsafe {
            x::send(fd, (&raw const byte).cast::<c_void>(), 1, 0);
        }
    }
    unsafe {
        x::close(fd);
    }
    connected
}

/// The listening fd the window pump polls, or `-1`.
pub(crate) fn listen_fd() -> i32 {
    LISTEN.load(Ordering::Acquire)
}

/// Accept one connection on the listening fd and say it was a "show".
///
/// `accept4` with `SOCK_CLOEXEC | SOCK_NONBLOCK` never blocks on a
/// listener the pump already knows is readable, and the accepted fd is
/// non-blocking too, so the `read` of up to eight bytes cannot wait.
/// A successful accept **is** the request — a second launcher connected
/// to ask to show — so bytes or no bytes, the answer is true. A bare
/// connection with no payload counts, and an `EAGAIN` read does not
/// turn it into a false one.
pub(crate) fn accept_show() -> bool {
    let fd = LISTEN.load(Ordering::Acquire);
    if fd < 0 {
        return false;
    }
    let accepted = unsafe {
        x::accept4(
            fd,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            x::SOCK_CLOEXEC | x::SOCK_NONBLOCK,
        )
    };
    if accepted < 0 {
        return false;
    }
    let mut bytes = [0u8; 8];
    let read = unsafe { x::read(accepted, bytes.as_mut_ptr().cast::<c_void>(), bytes.len()) };
    unsafe {
        x::close(accepted);
    }
    // `read` can answer 0 (peer closed) or -1 (EAGAIN on a non-blocking
    // fd); neither changes the fact that a peer connected to ask. Keep
    // the value read so the call is not silently dropped, and answer.
    let _ = read;
    true
}

impl Sole {
    /// Take the session's name, and learn what this process is.
    ///
    /// The driver says `backend::Sole::take()`; the free [`take`] is the
    /// same question for a caller that does not name the type first.
    pub fn take() -> Taken {
        crate::sole::take()
    }

    /// Give the name back, when the launcher is done before the process is.
    pub fn release(self) {
        drop(self);
    }
}

impl Drop for Sole {
    fn drop(&mut self) {
        if self.fd >= 0 {
            unsafe {
                x::close(self.fd);
            }
            // FreeBSD: the name is a real file, so a clean exit removes it.
            // A killed process cannot run `Drop` and leaves one behind;
            // `take`'s connect-first probe and unlink-before-bind reclaim
            // that stale file at the next start. Linux's abstract name
            // leaves nothing on the filesystem and takes no action here.
            #[cfg(target_os = "freebsd")]
            unsafe {
                x::unlink(self.path.as_ptr().cast());
            }
            LISTEN.store(-1, Ordering::Release);
            runtime::set_sole_listen(-1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hash is stable for one display and differs for two of them.
    #[test]
    fn the_hash_is_stable_and_tells_displays_apart() {
        assert_eq!(display_hash(":0"), display_hash(":0"));
        assert_ne!(display_hash(":0"), display_hash(":1"));
        assert_ne!(display_hash("wayland-0"), display_hash(":0"));
        // An empty display is a valid input, not a panic.
        assert_eq!(display_hash(""), display_hash(""));
    }

    /// The name is abstract, carries the uid, and fits `sun_path` (Linux).
    #[cfg(target_os = "linux")]
    #[test]
    fn the_name_is_abstract_carries_the_uid_and_fits() {
        let name = socket_name(1000, ":0");
        assert_eq!(name[0], 0, "the leading NUL makes it abstract");
        let text = core::str::from_utf8(&name[1..]).unwrap_or("");
        assert!(text.contains("1000"), "the uid is in the name: {text}");
        assert!(name.len() < 108, "the name fits the 108-byte path");
        assert!(text.contains("rgy-rgxy-launcher"));
    }

    /// On FreeBSD the name is a `/tmp` path, carries the uid, and fits the
    /// 104-byte `sun_path` (FreeBSD has no abstract sockets).
    #[cfg(target_os = "freebsd")]
    #[test]
    fn the_name_is_abstract_carries_the_uid_and_fits() {
        let name = socket_name(1000, ":0");
        assert_eq!(name[0], b'/', "the leading / makes it a filesystem path");
        let text = core::str::from_utf8(&name).unwrap_or("");
        assert!(text.contains("1000"), "the uid is in the name: {text}");
        assert!(name.len() < 104, "the name fits the 104-byte sun_path");
        assert!(text.contains("rgy-rgxy-launcher"));
    }

    /// Two uids are two launchers, even on the same display.
    #[test]
    fn two_uids_get_two_names() {
        assert_ne!(socket_name(1000, ":0"), socket_name(1001, ":0"));
        assert_eq!(socket_name(1000, ":0"), socket_name(1000, ":0"));
        assert_ne!(socket_name(1000, ":0"), socket_name(1000, ":1"));
    }

    /// The FreeBSD C path is the socket name plus exactly one terminating
    /// NUL, so `unlink` receives a C string.
    #[cfg(target_os = "freebsd")]
    #[test]
    fn c_path_terminates_the_socket_name() {
        let name = socket_name(1000, ":0");
        let path = c_path(&name);
        assert_eq!(path.len(), name.len() + 1);
        assert_eq!(path.last(), Some(&0), "the C string ends in NUL");
        assert_eq!(&path[..name.len()], name.as_slice(), "the name is kept");
        assert_eq!(path[0], b'/', "the path is absolute");
    }
}
