// SPDX-License-Identifier: Apache-2.0

//! A console probe for the keyboard paths, for a test no unit test can do.
//!
//! The read loop's rules are unit-tested with simulated bytes, but the two
//! questions that decide which loop runs are only answerable in a real
//! console: does `SetConsoleMode` accept `ENABLE_VIRTUAL_TERMINAL_INPUT`,
//! and does a real paste arrive as a bracketed burst whose `\r` is a
//! character? This example is the instrument for them.
//!
//! It drives the same seam the terminal UI drives — `enable_raw_mode`,
//! `Screen::enable_bracketed_paste`, `poll`, `read` — and writes every event
//! it sees to a log in the temp folder, so the result can be read after the
//! window is closed. Press `Ctrl+V` to paste and `Escape` to stop.
//!
//! ```text
//! cargo build --example vtprobe -p rgy-search-ui
//! start target\debug\examples\vtprobe.exe 60
//! ```
//!
//! The console's class is logged with the mode: `PseudoConsoleWindow` is a
//! terminal that implements bracketed paste (Windows Terminal today) and
//! takes the VT byte path; `ConsoleWindowClass` is the legacy host, which
//! keeps the key-record path and its own paste shortcuts.

#[cfg(windows)]
fn main() {
    probe::run();
}

#[cfg(not(windows))]
fn main() {
    eprintln!("vtprobe is a Windows console probe: there is no console mode to read here");
}

#[cfg(windows)]
mod probe {
    use std::io::Write;
    use std::time::{Duration, Instant};

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

    /// The console's input mode, or `None` when the handle is not a console.
    fn input_mode() -> Option<u32> {
        let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
        let mut mode: u32 = 0;
        match unsafe { GetConsoleMode(handle, &mut mode) } {
            0 => None,
            _ => Some(mode),
        }
    }

    /// The mode, in the words the decision is made in.
    fn describe_mode(mode: Option<u32>) -> String {
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
    fn console_class() -> String {
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

    pub fn run() {
        let mut args = std::env::args().skip(1);
        let log_path = args
            .next()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("vtprobe.log"));
        let seconds: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(60);

        let mut log = match std::fs::File::create(&log_path) {
            Ok(log) => log,
            Err(err) => {
                eprintln!("vtprobe: cannot write {}: {err}", log_path.display());
                return;
            }
        };
        // Every line goes to the log and to the window: the window is for the
        // person doing the test, the log is for the machine afterwards.
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
        say!("CONSOLE class={}", console_class());
        say!("BEFORE {}", describe_mode(input_mode()));

        if let Err(err) = rgy_search_ui::term::enable_raw_mode() {
            say!("RAW_MODE_FAILED {err}");
            return;
        }
        say!("AFTER_RAW {}", describe_mode(input_mode()));

        // The window is opened by a launcher, and a key goes to the window
        // that has the keyboard. Asking for it here is what makes the probe
        // usable when it is started in the background: the person doing the
        // test presses Ctrl+V and the keys must land in this console.
        let window = unsafe { GetConsoleWindow() };
        if !window.is_null() {
            let focused = unsafe { SetForegroundWindow(window) };
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
        say!("RESTORED {}", describe_mode(input_mode()));
        say!("END");
        println!("\nvtprobe: the log is {}", log_path.display());
    }
}
