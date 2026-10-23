// SPDX-License-Identifier: Apache-2.0

//! Console output encoding: the Windows code page (parity row 4.7).
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

use std::io::{self, Write};
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
struct State {
    stdout: Mode,
    stderr: Mode,
}

static STATE: OnceLock<State> = OnceLock::new();

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
pub struct Console<W> {
    inner: W,
    mode: Mode,
    /// The console handle. Only the wide path needs it, and the wide path
    /// only exists on Windows.
    #[cfg(windows)]
    handle: sys::Handle,
}

impl<W> Console<W> {
    fn new(inner: W, mode: Mode, handle: sys::Handle) -> Self {
        #[cfg(not(windows))]
        let _ = handle;
        Console {
            inner,
            mode,
            #[cfg(windows)]
            handle,
        }
    }
}

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

/// Print to standard output the way the console can render it.
///
/// The counterpart of `println!` for a program whose output may land on a
/// Windows console: the bytes are the same, and a console that refused
/// the UTF-8 code page gets the text as UTF-16 instead of mojibake.
///
/// The newline is part of the same write, the way `println!` does it: a
/// line that another thread could split in half is not a line.
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
#[cfg(not(windows))]
mod sys {
    /// No handle to speak of: the wide path does not exist here.
    pub type Handle = ();
}

/// Read the console's state and switch its code page when there is one.
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
        #[cfg(windows)]
        assert!(!sys::is_console(std::ptr::null_mut()));
    }
}
