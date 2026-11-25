// SPDX-License-Identifier: Apache-2.0

//! The machine behind `rgxy --tui`'s actions.
//!
//! The window driver reaches its platform through `platform::backend`, and
//! the terminal reaches the SAME actions through this file. It implements
//! [`rgy_search_ui::Runner`]: the terminal decides what the user asked for,
//! and this is what opens the file, opens its folder, takes or gives the
//! clipboard and reads or writes the settings file.
//!
//! Two rules keep it honest:
//!
//! - The X11 clipboard in `rgy-launcher-x11::actions` belongs to the
//!   window's display connection, which a terminal session does not have.
//!   On unix the terminal writes the clipboard with **OSC 52**, the escape
//!   sequence a terminal understands, straight to `/dev/tty` — no X
//!   connection, no external helper, no hang. A terminal that does not
//!   implement OSC 52 ignores the sequence, and the notice still names the
//!   path. Reading it back is the terminal's own bracketed-paste job, so
//!   `paste_text` answers `None` there rather than pretending.
//! - On a host with neither window backend (macOS) the actions fall back
//!   to `std::process::Command` and the platform's own opener. A refusal
//!   is a sentence the terminal can show, not a panic. FreeBSD has the
//!   X11 backend, so `open`/`open_folder` go through it there.

use rgy_launcher::Settings;
use rgy_launcher::text::Lang;
use rgy_search_ui::Runner;

use crate::settings_store;

/// The terminal's machine, in the language the launcher speaks.
pub struct TuiActions {
    lang: Lang,
}

impl TuiActions {
    /// A runner that says its refusals in `lang`.
    pub fn new(lang: Lang) -> TuiActions {
        TuiActions { lang }
    }
}

impl Runner for TuiActions {
    fn open(&self, path: &str) -> Result<(), String> {
        open(path, self.lang)
    }

    fn open_folder(&self, path: &str) -> Result<(), String> {
        reveal(path, self.lang)
    }

    fn copy_path(&self, path: &str) -> Result<(), String> {
        copy_path(path, self.lang)
    }

    fn paste_text(&self) -> Option<String> {
        paste_text()
    }

    fn settings(&self) -> Settings {
        settings_store::store_path()
            .and_then(|path| settings_store::load(&path).ok())
            .unwrap_or_default()
    }

    fn save_settings(&self, settings: &Settings) -> Result<(), String> {
        let path = settings_store::store_path()
            .ok_or_else(|| String::from("the settings file has no path"))?;
        settings_store::save(&path, settings).map_err(|err| err.to_string())
    }
}

// --- opening a path -----------------------------------------------------------

#[cfg(any(windows, target_os = "linux", target_os = "freebsd"))]
fn open(path: &str, lang: Lang) -> Result<(), String> {
    crate::platform::backend::actions::open(path, lang)
}

#[cfg(not(any(windows, target_os = "linux", target_os = "freebsd")))]
fn open(path: &str, lang: Lang) -> Result<(), String> {
    portable::spawn_detached(portable::opener(), &[path], lang)
}

#[cfg(any(windows, target_os = "linux", target_os = "freebsd"))]
fn reveal(path: &str, lang: Lang) -> Result<(), String> {
    crate::platform::backend::actions::reveal(path, lang)
}

#[cfg(not(any(windows, target_os = "linux", target_os = "freebsd")))]
fn reveal(path: &str, lang: Lang) -> Result<(), String> {
    portable::spawn_detached(portable::opener(), &[portable::parent_of(path)], lang)
}

// --- the clipboard ------------------------------------------------------------

#[cfg(windows)]
fn copy_path(path: &str, lang: Lang) -> Result<(), String> {
    // `OpenClipboard(null)` is the Win32 call that needs no window at all:
    // the console is not an X display that has to be opened first.
    crate::platform::backend::actions::copy_path(path, lang)
}

#[cfg(not(windows))]
fn copy_path(path: &str, _lang: Lang) -> Result<(), String> {
    portable::osc52(path)
}

#[cfg(windows)]
fn paste_text() -> Option<String> {
    match crate::platform::backend::actions::paste_text() {
        crate::platform::backend::actions::Clipboard::Text(text) if !text.is_empty() => Some(text),
        _ => None,
    }
}

#[cfg(not(windows))]
fn paste_text() -> Option<String> {
    // A terminal delivers a paste as bracketed-paste bytes, and `term.rs`
    // already decodes them into keys. There is no portable OSC 52 *read*
    // that a terminal answers without a query/response the decoder does
    // not own, so the honest answer is "the terminal will paste it".
    None
}

// --- the portable half --------------------------------------------------------

/// The OSC 52 writer for the terminal's clipboard on every unix host, and
/// the platform's opener on hosts that have no window backend.
#[cfg(not(windows))]
mod portable {
    use std::io::Write;

    /// Put `text` on the terminal's clipboard with OSC 52.
    ///
    /// The sequence goes to `/dev/tty` and not to stdout: stdout is the
    /// alternate screen the frames are painted on, and a control sequence
    /// written between two rows would be a row the painter did not draw.
    /// A terminal that does not implement OSC 52 ignores the bytes; the
    /// caller still shows the path, so nothing is lost.
    pub fn osc52(text: &str) -> Result<(), String> {
        let mut tty = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/tty")
            .map_err(|err| format!("the terminal's clipboard could not be reached ({err})"))?;
        let mut body = String::from("\x1b]52;c;");
        base64(text.as_bytes(), &mut body);
        body.push('\x07');
        tty.write_all(body.as_bytes())
            .map_err(|err| format!("the clipboard sequence could not be written ({err})"))?;
        tty.flush()
            .map_err(|err| format!("the clipboard sequence could not be flushed ({err})"))
    }

    /// Base 64, written by hand: the terminal's clipboard is a byte string,
    /// and a crate for six lines of encoding is a crate the binary does not
    /// need.
    fn base64(bytes: &[u8], out: &mut String) {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut at = 0;
        while at + 3 <= bytes.len() {
            let n = (u32::from(bytes[at]) << 16)
                | (u32::from(bytes[at + 1]) << 8)
                | u32::from(bytes[at + 2]);
            out.push(TABLE[(n >> 18) as usize & 63] as char);
            out.push(TABLE[(n >> 12) as usize & 63] as char);
            out.push(TABLE[(n >> 6) as usize & 63] as char);
            out.push(TABLE[n as usize & 63] as char);
            at += 3;
        }
        let left = bytes.len() - at;
        if left > 0 {
            let n = (u32::from(bytes[at]) << 16)
                | (if left > 1 {
                    u32::from(bytes[at + 1]) << 8
                } else {
                    0
                });
            out.push(TABLE[(n >> 18) as usize & 63] as char);
            out.push(TABLE[(n >> 12) as usize & 63] as char);
            out.push(if left > 1 {
                TABLE[(n >> 6) as usize & 63] as char
            } else {
                '='
            });
            out.push('=');
        }
    }

    /// The program that opens a path with the platform's own association.
    #[cfg(not(any(windows, target_os = "linux", target_os = "freebsd")))]
    pub fn opener() -> &'static str {
        if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        }
    }

    /// Everything before the last separator — the folder that holds a path.
    #[cfg(not(any(windows, target_os = "linux", target_os = "freebsd")))]
    pub fn parent_of(path: &str) -> &str {
        match path.rfind(['/', '\\']) {
            Some(0) => &path[..1],
            Some(at) => &path[..at],
            None => ".",
        }
    }

    /// Open a path without waiting for a program that may outlive the TUI.
    ///
    /// The child is reaped on a thread of its own: dropping a `Child`
    /// without waiting leaves a zombie behind a resident launcher. All
    /// three streams go to the null device, so a GUI opener does not
    /// inherit the terminal's raw mode or write over the alternate screen.
    #[cfg(not(any(windows, target_os = "linux", target_os = "freebsd")))]
    pub fn spawn_detached(
        program: &str,
        args: &[&str],
        lang: rgy_launcher::text::Lang,
    ) -> Result<(), String> {
        use rgy_launcher::text::{Text, fill};
        use std::process::{Command, Stdio};

        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|err| {
                fill(
                    Text::for_lang(lang).could_not_open,
                    &[args.first().copied().unwrap_or(program), &err.to_string()],
                )
            })?;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// The encoder is the standard one, including the two paddings.
        #[test]
        fn the_osc52_body_is_base64() {
            let mut out = String::new();
            base64(b"abc", &mut out);
            assert_eq!(out, "YWJj");
            out.clear();
            base64(b"a", &mut out);
            assert_eq!(out, "YQ==");
            out.clear();
            base64(b"ab", &mut out);
            assert_eq!(out, "YWI=");
            out.clear();
            base64(b"", &mut out);
            assert!(out.is_empty());
        }

        /// A path with a separator keeps its folder, and a bare name is in
        /// the current one. This half exists only on a host with no window
        /// backend; the Linux backend opens the folder itself.
        #[cfg(not(any(windows, target_os = "linux", target_os = "freebsd")))]
        #[test]
        fn the_folder_is_the_part_before_the_last_separator() {
            assert_eq!(parent_of("/a/b/report.txt"), "/a/b");
            assert_eq!(parent_of("/report.txt"), "/");
            assert_eq!(parent_of("report.txt"), ".");
            assert_eq!(parent_of(r"C:\a\b.txt"), r"C:\a");
        }
    }
}
