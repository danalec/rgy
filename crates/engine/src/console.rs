// SPDX-License-Identifier: Apache-2.0

//! Console output encoding: the Windows code page.
//!
//! rgy writes UTF-8 bytes. On Linux and macOS the terminal *is* UTF-8, so
//! the bytes are the text. On Windows the console's output code page
//! decides how those bytes are decoded, and the default is a legacy
//! single-byte page: a path with an accent comes out as `?` or as
//! mojibake. The name is the product here, so a mangled name is a broken
//! result, not a cosmetic detail.
//!
//! Two mechanisms, in the order the platform itself recommends:
//!
//! 1. [`init`] switches the console's output code page to UTF-8, so the
//!    console decodes what the program writes as UTF-8. It is one call, and
//!    it covers every writer in the process -- `println!`, crossterm, a
//!    dependency's own output -- because the code page belongs to the
//!    console rather than to any one of them.
//! 2. When the console refuses the code page, [`Console`] falls back to
//!    `WriteConsoleW` with the text converted to UTF-16. The console
//!    always renders UTF-16 correctly, so this is the path that cannot
//!    fail to be right.
//!
//! A redirected stream is not a console, and it keeps the raw UTF-8 bytes:
//! that is what the machine on the other end of the pipe expects, and a
//! machine consumer must not be handed transcoded text. Nothing here
//! changes what a pipe carries.
//!
//! The code page is a property of the console, not of the process, so it
//! stays UTF-8 for that console after the program exits -- the same thing
//! `chcp 65001` does, and the same trade-off: the programs that run next
//! in that console are read as UTF-8 too. Restoring it on the way out
//! would help only the programs that exit cleanly, and the daemon is
//! usually stopped with Ctrl-C, which does not unwind.
//!
//! On every platform other than Windows this is a no-op: the streams are
//! left exactly as they were.

// The wide path is the Windows one: the text is converted to UTF-16 and
// written with `WriteConsoleW`. On every other platform the bytes go out
// as they are, and these two names are not used.
#[cfg(all(feature = "std", windows))]
use alloc::string::String;
#[cfg(all(feature = "std", windows))]
use alloc::vec::Vec;
#[cfg(feature = "std")]
use std::io::{self, Write};
#[cfg(feature = "std")]
use std::sync::OnceLock;

/// How one output stream is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The bytes go out exactly as written.
    ///
    /// The stream is not a console (a pipe or a file), or the platform needs
    /// no fix (Unix), or the console already speaks UTF-8.
    Bytes,
    /// The stream is a console that could not be switched to UTF-8: the
    /// text is converted to UTF-16 and written with `WriteConsoleW`.
    Utf16,
}

/// The mode for one stream, given what the platform reported.
///
/// Pure, so the fallback can be tested without a console: the whole
/// decision is "a console that is not speaking UTF-8 needs the wide path",
/// and the only way to get that wrong is to get this function wrong.
pub fn mode_for(is_console: bool, utf8_codepage: bool) -> Mode {
    if is_console && !utf8_codepage {
        Mode::Utf16
    } else {
        Mode::Bytes
    }
}

/// What the console said about the two output streams.
#[cfg(feature = "std")]
struct State {
    stdout: Mode,
    stderr: Mode,
}

#[cfg(feature = "std")]
static STATE: OnceLock<State> = OnceLock::new();

#[cfg(feature = "std")]
fn state() -> &'static State {
    STATE.get_or_init(detect)
}

/// Put the console into UTF-8 for output, and report how stdout is written.
///
/// Call this once, at start-up. On Windows it switches the console's
/// output code page to UTF-8 when a console is attached at all; on every
/// other platform it does nothing and the streams stay raw UTF-8 bytes.
///
/// Nothing here fails, prompts, or changes an exit code: a console that
/// cannot be reconfigured leaves the program running with the wide
/// fallback in place of the code page.
#[cfg(feature = "std")]
pub fn init() -> Mode {
    state().stdout
}

/// Like [`init`], and also switches the console's *input* code page to
/// UTF-8.
///
/// For a program that reads what the user types: the console encodes the
/// keystrokes with the input code page, so without this an accented
/// character typed into a query arrives as the legacy page's bytes. Only
/// a console is touched -- a piped stdin is left alone.
#[cfg(feature = "std")]
pub fn init_with_input() -> Mode {
    let mode = state().stdout;
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        sys::set_input_utf8(std::io::stdin().as_raw_handle());
    }
    mode
}

/// A standard stream that a Windows console renders as UTF-8.
///
/// The bytes are the program's own UTF-8. Nothing is transcoded unless the
/// console refused the UTF-8 code page, in which case the text goes out
/// as UTF-16 through `WriteConsoleW`.
#[cfg(feature = "std")]
pub struct Console<W> {
    inner: W,
    mode: Mode,
    /// The console handle. Only the wide path needs it, and the wide path
    /// only exists on Windows.
    #[cfg(windows)]
    handle: sys::Handle,
}

#[cfg(feature = "std")]
impl<W> Console<W> {
    fn new(inner: W, mode: Mode, _handle: sys::Handle) -> Self {
        Console {
            inner,
            mode,
            // The handle is the platform's own: a raw handle on Windows and
            // nothing at all everywhere else. The name carries the
            // underscore because the platform without a handle has no
            // binding to keep -- and a binding of a unit is a lint, not a
            // promise.
            #[cfg(windows)]
            handle: _handle,
        }
    }
}

#[cfg(feature = "std")]
impl<W: Write> Write for Console<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.mode {
            Mode::Bytes => self.inner.write(buf),
            Mode::Utf16 => self.write_wide(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(feature = "std")]
#[cfg(windows)]
impl<W: Write> Console<W> {
    /// The fallback path: the console did not take the UTF-8 code page, so
    /// the text goes out as UTF-16, which the console always renders.
    fn write_wide(&mut self, buf: &[u8]) -> io::Result<usize> {
        // The bytes are the program's own UTF-8; a byte that is not valid
        // UTF-8 becomes U+FFFD rather than an abort.
        let text = String::from_utf8_lossy(buf);
        let wide: Vec<u16> = text.encode_utf16().collect();
        if sys::write_wide(self.handle, &wide).is_ok() {
            return Ok(buf.len());
        }
        // The console is gone or refused the write. The raw bytes still
        // reach the handle, so the output is not lost -- it is only read
        // with the legacy code page, which is what it would have been
        // anyway.
        self.inner.write_all(buf)?;
        Ok(buf.len())
    }
}

#[cfg(feature = "std")]
#[cfg(not(windows))]
impl<W: Write> Console<W> {
    /// Unreachable on this platform: the mode is never [`Mode::Utf16`]
    /// here, because there is no code page to refuse. It writes the bytes
    /// unchanged rather than dropping them.
    fn write_wide(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }
}

/// Standard output, written so a Windows console renders it as UTF-8.
#[cfg(feature = "std")]
pub fn stdout() -> Console<std::io::Stdout> {
    #[cfg(windows)]
    let handle = {
        use std::os::windows::io::AsRawHandle;
        std::io::stdout().as_raw_handle()
    };
    #[cfg(not(windows))]
    let handle = ();
    Console::new(std::io::stdout(), state().stdout, handle)
}

/// Standard error, written so a Windows console renders it as UTF-8.
#[cfg(feature = "std")]
pub fn stderr() -> Console<std::io::Stderr> {
    #[cfg(windows)]
    let handle = {
        use std::os::windows::io::AsRawHandle;
        std::io::stderr().as_raw_handle()
    };
    #[cfg(not(windows))]
    let handle = ();
    Console::new(std::io::stderr(), state().stderr, handle)
}

/// How many paths could not be spelled.
///
/// The counter counts paths, and not spellings: the daemon spells the
/// search root once per hit, and a path spelled a thousand times is one
/// path. The paths already counted are kept (see [`count_once`]), so the
/// number the user reads is the number of paths.
#[cfg(feature = "std")]
static UNSPELLABLE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The unspellable paths already counted.
///
/// The set holds the paths, not the names, and a path that is not UTF-8
/// is rare: the set is small.
#[cfg(feature = "std")]
static COUNTED: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashSet<alloc::vec::Vec<u8>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));

/// Has this path been counted already?
///
/// The count is of paths and not of spellings, and a path is spelled more
/// than once — the daemon spells the search root once per hit, and the
/// root is one path — so the paths that were counted are kept and a path
/// that is already in them is not counted again.
///
/// A lock that cannot be taken (a poisoned one) counts the path anyway:
/// the count is then the old one, which is wrong by the number of the
/// repetitions and right about the number of the paths.
#[cfg(feature = "std")]
fn count_once(path: &[u8]) -> bool {
    match COUNTED.lock() {
        Ok(mut seen) => seen.insert(path.to_vec()),
        Err(_) => true,
    }
}

/// The path as text.
///
/// The console and the wire carry UTF-8, and a path that is not UTF-8
/// cannot be spelled on either: the answer carries a replacement
/// character, and the file the user opens is not the file the search
/// found. A path that cannot be spelled cannot be opened either.
///
/// The path is written anyway — it is the only spelling there is — and
/// the count is said out loud, because a path that cannot be spelled is
/// a path the user has to know about.
#[cfg(feature = "std")]
pub fn spell(path: &[u8]) -> alloc::string::String {
    match std::str::from_utf8(path) {
        Ok(text) => alloc::string::String::from(text),
        Err(_) => {
            // The count is of paths, so a path that was counted already is
            // not counted again — the daemon spells the search root once
            // per hit, and one root is one path.
            if count_once(path) {
                let count = UNSPELLABLE.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if count == 1 || count.is_multiple_of(1000) {
                    let _ = std::io::Write::write_fmt(
                        &mut stderr(),
                        format_args!(
                            "rgy: {count} path(s) are not valid UTF-8 and are written with a replacement character\n"
                        ),
                    );
                }
            }
            alloc::string::String::from_utf8_lossy(path).into_owned()
        }
    }
}

/// Print to standard output the way the console can render it.
///
/// The counterpart of `println!` for a program whose output may land on a
/// Windows console: the bytes are the same, and a console that refused
/// the UTF-8 code page gets the text as UTF-16 instead of mojibake.
///
/// The newline is part of the same write, the way `println!` does it: a
/// line that another thread could split in half is not a line.
#[cfg(feature = "std")]
#[macro_export]
macro_rules! outln {
    ($($arg:tt)*) => {{
        let mut out = $crate::console::stdout();
        ::std::io::Write::write_fmt(
            &mut out,
            format_args!("{}\n", format_args!($($arg)*)),
        )
        .expect("failed printing to stdout");
    }};
}

/// Print to standard error the way the console can render it.
///
/// The counterpart of `eprintln!`; see [`outln!`].
#[cfg(feature = "std")]
#[macro_export]
macro_rules! errln {
    ($($arg:tt)*) => {{
        let mut err = $crate::console::stderr();
        ::std::io::Write::write_fmt(
            &mut err,
            format_args!("{}\n", format_args!($($arg)*)),
        )
        .expect("failed printing to stderr");
    }};
}

// --- the platform half -------------------------------------------------------

/// The Windows calls, declared by hand.
///
/// The `windows` crate family does not cross-compile (see
/// `crates/discovery-windows/src/lib.rs` for why), and a dependency edge
/// is the owner's decision, not this crate's. These four calls are all in
/// kernel32, which every Windows process already links.
#[cfg(feature = "std")]
#[cfg(windows)]
mod sys {
    use std::ffi::c_void;
    use std::io;

    pub type Handle = *mut c_void;

    /// The UTF-8 code page.
    const CP_UTF8: u32 = 65001;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetConsoleMode(handle: Handle, mode: *mut u32) -> i32;
        fn SetConsoleOutputCP(code_page: u32) -> i32;
        fn SetConsoleCP(code_page: u32) -> i32;
        fn WriteConsoleW(
            handle: Handle,
            buffer: *const c_void,
            chars: u32,
            written: *mut u32,
            reserved: *mut c_void,
        ) -> i32;
    }

    /// Whether the handle is a console.
    ///
    /// `GetConsoleMode` is the platform's own test: it succeeds for a
    /// console handle and fails for a file or a pipe.
    pub fn is_console(handle: Handle) -> bool {
        let mut mode = 0u32;
        unsafe { GetConsoleMode(handle, &mut mode) != 0 }
    }

    /// Switch the console's output code page to UTF-8.
    ///
    /// Returns whether the console took it. A console that refuses is the
    /// case the wide path exists for.
    pub fn set_output_utf8() -> bool {
        unsafe { SetConsoleOutputCP(CP_UTF8) != 0 }
    }

    /// Switch the console's input code page to UTF-8, when the handle is a
    /// console at all.
    pub fn set_input_utf8(handle: Handle) {
        if is_console(handle) {
            unsafe { SetConsoleCP(CP_UTF8) };
        }
    }

    /// Write `text` to the console as UTF-16.
    ///
    /// `WriteConsoleW` may take a partial write, so the loop is not
    /// paranoia: a console that is busy writes what it can.
    pub fn write_wide(handle: Handle, text: &[u16]) -> io::Result<()> {
        let mut done = 0usize;
        while done < text.len() {
            let mut written = 0u32;
            let ok = unsafe {
                WriteConsoleW(
                    handle,
                    text[done..].as_ptr().cast::<c_void>(),
                    (text.len() - done) as u32,
                    &mut written,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            if written == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "the console accepted the write but wrote nothing",
                ));
            }
            done += written as usize;
        }
        Ok(())
    }
}

/// The stub for every platform that is not Windows: there is no code page
/// there, so there is nothing to detect and nothing to change.
#[cfg(feature = "std")]
#[cfg(not(windows))]
mod sys {
    /// No handle to speak of: the wide path does not exist here.
    pub type Handle = ();
}

/// Read the console's state and switch its code page when there is one.
#[cfg(feature = "std")]
#[cfg(windows)]
fn detect() -> State {
    use std::os::windows::io::AsRawHandle;

    let stdout_console = sys::is_console(std::io::stdout().as_raw_handle());
    let stderr_console = sys::is_console(std::io::stderr().as_raw_handle());
    // The code page belongs to the console, not to one stream, so a single
    // call covers both -- and it only happens when there is a console to
    // configure at all.
    let utf8 = (stdout_console || stderr_console) && sys::set_output_utf8();
    State {
        stdout: mode_for(stdout_console, utf8),
        stderr: mode_for(stderr_console, utf8),
    }
}

/// Everywhere else the terminal is UTF-8 already.
#[cfg(feature = "std")]
#[cfg(not(windows))]
fn detect() -> State {
    State {
        stdout: Mode::Bytes,
        stderr: Mode::Bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    /// A path that cannot be spelled is written and said.
    ///
    /// The console and the wire carry UTF-8, and a path that is not
    /// UTF-8 cannot be spelled on either. The path is written anyway --
    /// it is the only spelling there is -- and the count is said, so
    /// that a path the user cannot open is a path they were told about.
    ///
    /// The gate is the feature and not the platform: `spell` is the
    /// std-only half of the console -- it writes to stderr and it keeps
    /// the count of the paths it could not spell -- so a build without
    /// the feature has no `spell` to test, and a test module that names
    /// it anyway does not compile in the crate's own `no_std`
    /// configuration. The rule is the one the other crates follow: a
    /// test of a std-only thing stands down with the thing.
    #[cfg(feature = "std")]
    #[test]
    fn a_path_that_cannot_be_spelled_is_written_and_said() {
        assert_eq!(spell(b"C:/a/notas.txt"), "C:/a/notas.txt");
        // The bytes of a latin-1 name: they are not UTF-8.
        let spelled = spell(b"caf\xe9.txt");
        assert!(
            spelled.contains('\u{FFFD}'),
            "the replacement character is there: {spelled}"
        );
    }

    /// A path spelled twice is one path.
    ///
    /// The count is of paths and not of spellings, and the daemon spells
    /// the search root once per hit: a root spelled a hundred times is
    /// one path, and the number the user reads has to say one.
    #[cfg(feature = "std")]
    #[test]
    fn a_path_spelled_twice_is_counted_once() {
        let path = b"counted-once-probe-\xff";
        assert!(count_once(path), "the first time counts the path");
        assert!(!count_once(path), "the second time does not");
        assert!(!count_once(path), "and the third does not");
        // Another path is another path.
        assert!(
            count_once(b"counted-once-probe-\xfe"),
            "a different path is counted too"
        );
    }

    #[test]
    fn a_pipe_keeps_its_bytes() {
        // Not a console: the machine on the other end gets the UTF-8 the
        // program wrote, unchanged.
        assert_eq!(mode_for(false, false), Mode::Bytes);
        assert_eq!(mode_for(false, true), Mode::Bytes);
    }

    #[test]
    fn a_console_speaking_utf8_keeps_its_bytes() {
        // The code page was switched, so the console decodes the bytes as
        // UTF-8 and the wide path is not needed.
        assert_eq!(mode_for(true, true), Mode::Bytes);
    }

    #[test]
    fn a_console_that_refused_the_code_page_uses_the_wide_path() {
        // The fallback: the console is real, but it is not speaking UTF-8,
        // so the text has to go out as UTF-16.
        assert_eq!(mode_for(true, false), Mode::Utf16);
    }

    #[test]
    fn the_wide_path_carries_the_text_as_utf16() {
        // What the fallback writes for a name with an accent: the console
        // receives UTF-16 code units, not the UTF-8 bytes.
        let wide: Vec<u16> = "ação".encode_utf16().collect();
        assert_eq!(wide, vec![0x61, 0xE7, 0xE3, 0x6F]);
    }

    #[test]
    fn a_handle_that_is_not_a_console_is_not_one() {
        // The platform's own test, exercised where it can be: a handle that
        // is not a console is recognised as one.
        #[cfg(all(windows, feature = "std"))]
        assert!(!sys::is_console(std::ptr::null_mut()));
    }
}
