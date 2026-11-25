// SPDX-License-Identifier: Apache-2.0

//! A terminal probe for the keyboard paths, for a test no unit test can do.
//!
//! The read loop's rules are unit-tested with simulated bytes, but the
//! questions that decide which loop runs and what a paste delivers are only
//! answerable against a real terminal. This example is the instrument for
//! them: it drives the same seam the terminal UI drives — `enable_raw_mode`,
//! `Screen::enable_bracketed_paste`, `poll`, `read` — and writes every event
//! it sees to a log, so the answer is readable after the session is over.
//!
//! On Windows it also reports the console's window class and input mode
//! before raw mode, after it and after the console is given back: that pair
//! is the whole answer to "which read path will this console take". On a
//! unix terminal there is no such question — the bytes are the terminal's —
//! and the probe is driven by writing those bytes into a pseudo-terminal.
//!
//! ```text
//! cargo install --path bins/search-ui --example vtprobe --force
//! vtprobe <log> <seconds>                       # Windows Terminal tab
//! conhost.exe cmd /c vtprobe <log> <seconds>    # legacy host
//! ```
//!
//! On unix, the terminal emulator is replaced by a pty, and the burst is
//! written by the caller: `\x1b[200~PASTE1\rPASTE2\x1b[201~`. Pressing
//! `Ctrl+V` and `Escape` by hand works there too.

fn main() {
    probe::run();
}

mod probe {
    use std::io::Write;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    pub fn run() {
        let mut args = std::env::args().skip(1);
        let log_path = args
            .next()
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("vtprobe.log"));
        let seconds: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(60);

        let mut log = match std::fs::File::create(&log_path) {
            Ok(log) => log,
            Err(err) => {
                eprintln!("vtprobe: cannot write {}: {err}", log_path.display());
                return;
            }
        };
        // Every line goes to the log and to the terminal: the terminal is for
        // the person doing the test, the log is for the machine afterwards.
        macro_rules! say {
            ($($arg:tt)*) => {{
                let line = format!($($arg)*);
                let _ = writeln!(log, "{line}");
                let _ = log.flush();
                let mut out = std::io::stdout();
                let _ = writeln!(out, "{line}");
                let _ = out.flush();
            }};
        }

        say!("LOG {}", log_path.display());

        #[cfg(windows)]
        {
            say!("CONSOLE class={}", win::console_class());
            say!("BEFORE {}", win::describe_mode(win::input_mode()));
        }
        #[cfg(not(windows))]
        say!("CONSOLE a terminal: the bytes are the terminal's, there is no console mode");

        if let Err(err) = rgy_search_ui::term::enable_raw_mode() {
            say!("RAW_MODE_FAILED {err}");
            return;
        }
        #[cfg(windows)]
        say!("AFTER_RAW {}", win::describe_mode(win::input_mode()));

        // The window is opened by a launcher, and a key goes to the window
        // that has the keyboard. Asking for it is what makes the probe usable
        // when it is started in the background: the person doing the test
        // presses the keys, and they must land in this console.
        #[cfg(windows)]
        if let Some(focused) = win::claim_foreground() {
            say!("FOREGROUND claimed={focused}");
        }

        let mut screen = rgy_search_ui::term::Screen::new(std::io::stdout());
        if let Err(err) = screen.enable_bracketed_paste() {
            say!("BRACKETED_PASTE_FAILED {err}");
        }
        say!("READY press Ctrl+V to paste, Escape to stop");

        let started = Instant::now();
        let mut events = 0u32;
        while started.elapsed().as_secs() < seconds {
            match rgy_search_ui::term::poll(Duration::from_millis(100)) {
                Ok(true) => match rgy_search_ui::term::read() {
                    Ok(event) => {
                        events += 1;
                        say!(
                            "EVENT {events} at={}ms {event:?}",
                            started.elapsed().as_millis()
                        );
                        if let rgy_search_ui::term::Event::Key(key) = event
                            && key.code == rgy_search_ui::term::KeyCode::Esc
                        {
                            break;
                        }
                    }
                    Err(err) => {
                        say!("READ_ERROR {err}");
                        break;
                    }
                },
                Ok(false) => {}
                Err(err) => {
                    say!("POLL_ERROR {err}");
                    break;
                }
            }
        }

        say!("TOTAL {events}");
        let _ = screen.disable_bracketed_paste();
        let _ = rgy_search_ui::term::disable_raw_mode();
        #[cfg(windows)]
        say!("RESTORED {}", win::describe_mode(win::input_mode()));
        say!("END");
        println!("\nvtprobe: the log is {}", log_path.display());
    }

    /// The Windows console's own answers: its class, its mode, and the
    /// foreground claim that makes a background launch usable.
    #[cfg(windows)]
    mod win {
        type Handle = *mut core::ffi::c_void;

        const STD_INPUT_HANDLE: u32 = 0xFFFF_FFF6;
        const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;
        const ENABLE_WINDOW_INPUT: u32 = 0x0008;

        unsafe extern "system" {
            fn GetStdHandle(which: u32) -> Handle;
            fn GetConsoleMode(handle: Handle, mode: *mut u32) -> i32;
            fn GetConsoleWindow() -> Handle;
        }

        #[link(name = "user32")]
        unsafe extern "system" {
            fn GetClassNameW(window: Handle, buffer: *mut u16, max: i32) -> i32;
            fn SetForegroundWindow(window: Handle) -> i32;
        }

        /// The console's input mode, or `None` when the handle is not one.
        pub(super) fn input_mode() -> Option<u32> {
            let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
            let mut mode: u32 = 0;
            match unsafe { GetConsoleMode(handle, &mut mode) } {
                0 => None,
                _ => Some(mode),
            }
        }

        /// The mode, in the words the read path is chosen in.
        pub(super) fn describe_mode(mode: Option<u32>) -> String {
            match mode {
                None => "mode=unreadable".to_string(),
                Some(mode) => format!(
                    "mode=0x{mode:04x} vt_bit={} window_bit={}",
                    mode & ENABLE_VIRTUAL_TERMINAL_INPUT != 0,
                    mode & ENABLE_WINDOW_INPUT != 0
                ),
            }
        }

        /// The class of the window the console reports.
        pub(super) fn console_class() -> String {
            let window = unsafe { GetConsoleWindow() };
            if window.is_null() {
                return "<no window>".to_string();
            }
            let mut buffer = [0u16; 128];
            let count = unsafe { GetClassNameW(window, buffer.as_mut_ptr(), buffer.len() as i32) };
            if count <= 0 {
                return "<unnamed>".to_string();
            }
            String::from_utf16_lossy(&buffer[..count as usize])
        }

        /// Ask the console window for the keyboard. `None` when there is no
        /// window to ask.
        pub(super) fn claim_foreground() -> Option<bool> {
            let window = unsafe { GetConsoleWindow() };
            if window.is_null() {
                return None;
            }
            Some(unsafe { SetForegroundWindow(window) } != 0)
        }
    }
}
