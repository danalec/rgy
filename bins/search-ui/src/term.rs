// SPDX-License-Identifier: Apache-2.0

//! The tree's own terminal: the bytes on the screen, the keys, and the mode.
//!
//! The interface used to speak to the terminal through `crossterm`. The
//! crate is still in the tree — as a dev-dependency, the oracle — but no
//! binary carries it: the calls the terminal needs are declared here, by
//! hand, the way `crates/name-core/src/clock.rs` declares the clock and the
//! way `crates/discovery/src/file.rs` declares the filesystem. There is no
//! `libc` here either, and no `windows-sys`: the seam is a crate of bytes.
//!
//! Three things, and only three:
//!
//!   * the **escape sequences** — the cursor, the screen, the colours and
//!     the text — written as the bytes a terminal reads;
//!   * the **keys** — one event carrying the code and the modifiers,
//!     decoded from what the platform gives;
//!   * the **mode** — the raw mode that puts the keyboard in the program's
//!     hands, and the way back out of it.
//!
//! The bytes are the oracle's: the differential test holds every sequence
//! written here to the bytes `crossterm` wrote for the same command, and
//! the key corpus holds the decoding to the keys `crossterm` called the
//! same bytes. The oracle is a dev-dependency — it is in the tree, and it
//! is not in the binary.
//!
//! # The two halves
//!
//! The unix half is a terminal: the attributes of the line discipline, and
//! the bytes the keyboard sends. The Windows half is a console: the mode of
//! the input buffer, the records the console writes, and the screen buffer
//! the size comes from. They answer the same questions with the platform's
//! own words, and the interface above them does not know which one it is
//! talking to.

use std::io::Write;

// --- the colours ------------------------------------------------------------

/// The escape that puts the colours and the attributes back the way the
/// terminal had them.
///
/// A styled span that is cut short has to say where it stops, or the rest of
/// the line is painted in the span's colour.
pub const RESET: &str = "\x1b[0m";

/// A colour the terminal can paint with.
///
/// The sixteen are the ANSI palette, and `Rgb` is the 24-bit form. The
/// names are the ones the palette has always used: `DarkGrey` is the dim
/// grey of the recording, `White` the bright one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    /// The terminal's own colour again.
    Reset,
    Black,
    DarkGrey,
    Red,
    DarkRed,
    Green,
    DarkGreen,
    Yellow,
    DarkYellow,
    Blue,
    DarkBlue,
    Magenta,
    DarkMagenta,
    Cyan,
    DarkCyan,
    White,
    Grey,
    /// The 24-bit colour.
    Rgb {
        r: u8,
        g: u8,
        b: u8,
    },
}

/// Whether the terminal is to be told about colours at all.
///
/// `NO_COLOR` is the convention: a terminal that is not to be painted says
/// so in the environment, and the sequences are written without the colour
/// in them. The answer is read once — the environment does not change under
/// a program — and it is the rule the oracle follows too.
fn colors_disabled() -> bool {
    static DISABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DISABLED.get_or_init(|| !std::env::var("NO_COLOR").unwrap_or_default().is_empty())
}

/// The escape that sets a colour.
///
/// The channel is the prefix (`38;` for the foreground, `48;` for the
/// background) and the parameters are the colour itself: the ANSI palette
/// is an index, the 24-bit form is three numbers.
fn colour_escape(foreground: bool, colour: Color) -> String {
    let mut out = String::from("\x1b[");
    if !colors_disabled() {
        match colour {
            // The reset is the channel's own number, so it is written
            // without the prefix.
            Color::Reset => out.push_str(if foreground { "39" } else { "49" }),
            _ => {
                out.push_str(if foreground { "38;" } else { "48;" });
                out.push_str(&parameters(colour));
            }
        }
    }
    out.push('m');
    out
}

/// The colour's own parameters: the palette is an index, and the 24-bit
/// form is three numbers.
fn parameters(colour: Color) -> String {
    match colour {
        Color::Black => "5;0".into(),
        Color::DarkGrey => "5;8".into(),
        Color::Red => "5;9".into(),
        Color::DarkRed => "5;1".into(),
        Color::Green => "5;10".into(),
        Color::DarkGreen => "5;2".into(),
        Color::Yellow => "5;11".into(),
        Color::DarkYellow => "5;3".into(),
        Color::Blue => "5;12".into(),
        Color::DarkBlue => "5;4".into(),
        Color::Magenta => "5;13".into(),
        Color::DarkMagenta => "5;5".into(),
        Color::Cyan => "5;14".into(),
        Color::DarkCyan => "5;6".into(),
        Color::White => "5;15".into(),
        Color::Grey => "5;7".into(),
        Color::Rgb { r, g, b } => format!("2;{r};{g};{b}"),
        Color::Reset => String::new(),
    }
}

// --- the style --------------------------------------------------------------

/// The style a piece of text is painted in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Style {
    foreground: Option<Color>,
    background: Option<Color>,
    bold: bool,
    dim: bool,
    reverse: bool,
}

/// A piece of text with the style it is painted in.
///
/// The bytes are written when the text is: the background, the foreground,
/// the attributes, the text, and the reset that puts the terminal back the
/// way it was.
pub struct Styled {
    style: Style,
    content: String,
}

impl std::fmt::Display for Styled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The order is the terminal's: the background, the foreground, the
        // attributes, the text, and then the reset.
        let mut reset_background = false;
        let mut reset_foreground = false;
        if let Some(background) = self.style.background {
            f.write_str(&colour_escape(false, background))?;
            reset_background = true;
        }
        if let Some(foreground) = self.style.foreground {
            f.write_str(&colour_escape(true, foreground))?;
            reset_foreground = true;
        }
        // The attributes come in the SGR's own order — bold is 1, dim is 2,
        // reversed is 7 — and each is its own escape.
        let attributes = self.style.bold || self.style.dim || self.style.reverse;
        if self.style.bold {
            f.write_str("\x1b[1m")?;
        }
        if self.style.dim {
            f.write_str("\x1b[2m")?;
        }
        if self.style.reverse {
            f.write_str("\x1b[7m")?;
        }
        f.write_str(&self.content)?;
        if attributes {
            // An attribute is undone by the full reset, and the full reset
            // undoes the colours with it.
            f.write_str("\x1b[0m")
        } else {
            if reset_background {
                f.write_str("\x1b[49m")?;
            }
            if reset_foreground {
                f.write_str("\x1b[39m")?;
            }
            Ok(())
        }
    }
}

/// The way a piece of text is painted.
///
/// The names are the ones the interface already used; what changed is who
/// writes the bytes.
pub trait Paint {
    /// The text with a colour in front of it.
    fn with(self, colour: Color) -> Styled;
    /// The text in bold.
    fn bold(self) -> Styled;
    /// The text, dimmed: the words around the answer, not the answer.
    fn dim(self) -> Styled;
    /// The text with the colours swapped.
    fn reverse(self) -> Styled;
}

impl Paint for &str {
    fn with(self, colour: Color) -> Styled {
        Styled {
            style: Style {
                foreground: Some(colour),
                ..Style::default()
            },
            content: self.to_string(),
        }
    }

    fn bold(self) -> Styled {
        Styled {
            style: Style {
                bold: true,
                ..Style::default()
            },
            content: self.to_string(),
        }
    }

    fn dim(self) -> Styled {
        Styled {
            style: Style {
                dim: true,
                ..Style::default()
            },
            content: self.to_string(),
        }
    }

    fn reverse(self) -> Styled {
        Styled {
            style: Style {
                reverse: true,
                ..Style::default()
            },
            content: self.to_string(),
        }
    }
}

impl Paint for String {
    fn with(self, colour: Color) -> Styled {
        Styled {
            style: Style {
                foreground: Some(colour),
                ..Style::default()
            },
            content: self,
        }
    }

    fn bold(self) -> Styled {
        Styled {
            style: Style {
                bold: true,
                ..Style::default()
            },
            content: self,
        }
    }

    fn dim(self) -> Styled {
        Styled {
            style: Style {
                dim: true,
                ..Style::default()
            },
            content: self,
        }
    }

    fn reverse(self) -> Styled {
        Styled {
            style: Style {
                reverse: true,
                ..Style::default()
            },
            content: self,
        }
    }
}

impl Paint for char {
    fn with(self, colour: Color) -> Styled {
        self.to_string().with(colour)
    }

    fn bold(self) -> Styled {
        self.to_string().bold()
    }

    fn dim(self) -> Styled {
        self.to_string().dim()
    }

    fn reverse(self) -> Styled {
        self.to_string().reverse()
    }
}

impl Paint for Styled {
    fn with(mut self, colour: Color) -> Styled {
        self.style.foreground = Some(colour);
        self
    }

    fn bold(mut self) -> Styled {
        self.style.bold = true;
        self
    }

    fn dim(mut self) -> Styled {
        self.style.dim = true;
        self
    }

    fn reverse(mut self) -> Styled {
        self.style.reverse = true;
        self
    }
}

// --- the screen -------------------------------------------------------------

/// The screen the interface paints on.
///
/// The commands are the ones a terminal reads: the cursor moves, the screen
/// is cleared, the colours are set, the text is written. Each command writes
/// its bytes and nothing else — what they mean is the terminal's business,
/// not this module's.
pub struct Screen<W: Write> {
    out: W,
}

impl<W: Write> Screen<W> {
    /// The screen, over the writer the bytes go to.
    pub fn new(out: W) -> Self {
        Screen { out }
    }

    /// Move the cursor to a cell, counted from zero.
    pub fn move_to(&mut self, x: u16, y: u16) -> std::io::Result<()> {
        write!(self.out, "\x1b[{};{}H", y + 1, x + 1)?;
        Ok(())
    }

    /// Hide the cursor.
    pub fn hide(&mut self) -> std::io::Result<()> {
        self.out.write_all(b"\x1b[?25l")?;
        Ok(())
    }

    /// Show the cursor.
    pub fn show(&mut self) -> std::io::Result<()> {
        self.out.write_all(b"\x1b[?25h")?;
        Ok(())
    }

    /// Clear the whole screen.
    pub fn clear_all(&mut self) -> std::io::Result<()> {
        self.out.write_all(b"\x1b[2J")?;
        Ok(())
    }

    /// Clear the line the cursor is on.
    pub fn clear_line(&mut self) -> std::io::Result<()> {
        self.out.write_all(b"\x1b[2K")?;
        Ok(())
    }

    /// Switch to the alternate screen, the one the terminal keeps for the
    /// program and gives back when it leaves.
    pub fn enter_alternate_screen(&mut self) -> std::io::Result<()> {
        self.out.write_all(b"\x1b[?1049h")?;
        Ok(())
    }

    /// Switch back to the screen the user was on.
    pub fn leave_alternate_screen(&mut self) -> std::io::Result<()> {
        self.out.write_all(b"\x1b[?1049l")?;
        Ok(())
    }

    /// Set the background colour.
    pub fn background(&mut self, colour: Color) -> std::io::Result<()> {
        self.out
            .write_all(colour_escape(false, colour).as_bytes())?;
        Ok(())
    }

    /// Set the foreground colour.
    pub fn foreground(&mut self, colour: Color) -> std::io::Result<()> {
        self.out.write_all(colour_escape(true, colour).as_bytes())?;
        Ok(())
    }

    /// Put the colours and the attributes back the way the terminal had
    /// them.
    pub fn reset_colour(&mut self) -> std::io::Result<()> {
        self.out.write_all(b"\x1b[0m")?;
        Ok(())
    }

    /// Write the text.
    pub fn print(&mut self, text: &str) -> std::io::Result<()> {
        self.out.write_all(text.as_bytes())?;
        Ok(())
    }

    /// Send what was written.
    pub fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}

// --- the keys ---------------------------------------------------------------

/// The key a user pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCode {
    Backspace,
    Enter,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Delete,
    Esc,
    Tab,
    Char(char),
}

/// The modifiers held down with a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyModifiers(u8);

impl KeyModifiers {
    /// No modifier at all.
    pub const NONE: KeyModifiers = KeyModifiers(0);
    /// The shift key.
    pub const SHIFT: KeyModifiers = KeyModifiers(0b0000_0001);
    /// The control key.
    pub const CONTROL: KeyModifiers = KeyModifiers(0b0000_0010);
    /// The alt key.
    pub const ALT: KeyModifiers = KeyModifiers(0b0000_0100);

    /// Whether every modifier in `other` is held.
    pub fn contains(&self, other: KeyModifiers) -> bool {
        self.0 & other.0 == other.0
    }

    /// The modifiers of both, together.
    pub fn union(self, other: KeyModifiers) -> KeyModifiers {
        KeyModifiers(self.0 | other.0)
    }
}

/// Whether the key is going down, repeating, or coming up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEventKind {
    Press,
    Repeat,
    Release,
}

/// What the terminal said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// A key.
    Key(KeyEvent),
    /// The terminal changed size, and the two numbers are the new size in
    /// cells.
    Resize(u16, u16),
}

/// One key, with the modifiers held with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    /// The key.
    pub code: KeyCode,
    /// The modifiers held with it.
    pub modifiers: KeyModifiers,
    /// Whether the key is going down, repeating or coming up.
    pub kind: KeyEventKind,
}

impl KeyEvent {
    /// A key going down.
    pub fn new(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
        }
    }
}

/// What a run of bytes says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decoded {
    /// A key, and the number of bytes it took.
    Key(KeyEvent, usize),
    /// A sequence this program does not know, and the number of bytes it
    /// took: the run is over and there was no key in it.
    Unknown(usize),
    /// The run is the beginning of a sequence that is not whole yet.
    Incomplete,
}

/// The key in a run of bytes, and what the run says.
///
/// A terminal sends a character on its own, or an escape sequence for a key
/// that has no character of its own. A run that ends in the middle of a
/// sequence is not a key yet: the answer is [`Decoded::Incomplete`] and the
/// caller reads more.
///
/// `more_may_come` says whether the terminal may still have bytes to send.
/// A lone escape is the escape key only when nothing else is coming, since
/// a terminal that sends a sequence sends it in one piece.
pub fn decode(bytes: &[u8], more_may_come: bool) -> Decoded {
    let Some(first) = bytes.first().copied() else {
        return Decoded::Incomplete;
    };
    match first {
        b'\x1b' => decode_escape(bytes, more_may_come),
        b'\r' => Decoded::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 1),
        // A line feed is not the enter key here: the terminal is in the
        // raw mode, where the enter key sends a carriage return and a line
        // feed is the control key with the letter J.
        b'\t' => Decoded::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), 1),
        0x7f => Decoded::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE), 1),
        c @ 0x01..=0x1a => Decoded::Key(
            KeyEvent::new(
                KeyCode::Char((c - 0x01 + b'a') as char),
                KeyModifiers::CONTROL,
            ),
            1,
        ),
        c @ 0x1c..=0x1f => Decoded::Key(
            KeyEvent::new(
                KeyCode::Char((c - 0x1c + b'4') as char),
                KeyModifiers::CONTROL,
            ),
            1,
        ),
        0x00 => Decoded::Key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL), 1),
        _ => decode_char(bytes),
    }
}

/// The character at the start of the run.
///
/// A character that is not whole yet is not a character: the answer is
/// [`Decoded::Incomplete`] and the caller reads more.
fn decode_char(bytes: &[u8]) -> Decoded {
    let width = utf8_width(bytes[0]);
    if bytes.len() < width {
        return Decoded::Incomplete;
    }
    let Ok(text) = std::str::from_utf8(&bytes[..width]) else {
        return Decoded::Unknown(width);
    };
    let Some(c) = text.chars().next() else {
        return Decoded::Unknown(width);
    };
    // The shift key is in the character itself: an upper-case letter is a
    // shifted one.
    let modifiers = if c.is_uppercase() {
        KeyModifiers::SHIFT
    } else {
        KeyModifiers::NONE
    };
    Decoded::Key(KeyEvent::new(KeyCode::Char(c), modifiers), width)
}

/// How many bytes the character at the start of the run takes.
fn utf8_width(first: u8) -> usize {
    match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

/// The key in an escape sequence.
fn decode_escape(bytes: &[u8], more_may_come: bool) -> Decoded {
    match bytes.get(1) {
        None => {
            if more_may_come {
                // The sequence may still be coming: the escape key and the
                // beginning of a sequence look the same.
                Decoded::Incomplete
            } else {
                Decoded::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), 1)
            }
        }
        Some(b'O') => decode_ss3(bytes),
        Some(b'[') => decode_csi(bytes),
        Some(b'\x1b') => Decoded::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), 1),
        Some(_) => {
            // The escape in front of a key is the alt key held with it.
            match decode(&bytes[1..], more_may_come) {
                Decoded::Key(mut event, len) => {
                    event.modifiers = event.modifiers.union(KeyModifiers::ALT);
                    Decoded::Key(event, len + 1)
                }
                other => other,
            }
        }
    }
}

/// The key in an `ESC O` sequence, the one the cursor keys send in the
/// application mode.
fn decode_ss3(bytes: &[u8]) -> Decoded {
    let Some(final_byte) = bytes.get(2).copied() else {
        return Decoded::Incomplete;
    };
    let code = match final_byte {
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'H' => KeyCode::Home,
        b'F' => KeyCode::End,
        _ => return Decoded::Unknown(3),
    };
    Decoded::Key(KeyEvent::new(code, KeyModifiers::NONE), 3)
}

/// The key in a control sequence: `ESC [` and what follows.
fn decode_csi(bytes: &[u8]) -> Decoded {
    // The parameters are the numbers and the `;` between them; the letter
    // that ends the sequence names the key.
    let mut end = 2;
    while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b';') {
        end += 1;
    }
    if end == bytes.len() {
        return Decoded::Incomplete;
    }
    let final_byte = bytes[end];
    let length = end + 1;

    let mut parts = bytes[2..end].split(|b| *b == b';');
    let first = parts.next().unwrap_or(b"");
    let modifiers = parts
        .next()
        .and_then(|m| std::str::from_utf8(m).ok())
        .and_then(|m| m.parse::<u8>().ok())
        .map(modifiers_of)
        .unwrap_or(KeyModifiers::NONE);

    let code = match final_byte {
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'H' => KeyCode::Home,
        b'F' => KeyCode::End,
        b'~' => match first {
            b"1" | b"7" => KeyCode::Home,
            b"3" => KeyCode::Delete,
            b"4" | b"8" => KeyCode::End,
            b"5" => KeyCode::PageUp,
            b"6" => KeyCode::PageDown,
            _ => return Decoded::Unknown(length),
        },
        _ => return Decoded::Unknown(length),
    };
    Decoded::Key(KeyEvent::new(code, modifiers), length)
}

/// The modifiers a control sequence names.
///
/// The number the sequence carries is one more than the bit mask, so the
/// plain key is one and the control key is five.
fn modifiers_of(mask: u8) -> KeyModifiers {
    let bits = mask.saturating_sub(1);
    let mut modifiers = KeyModifiers::NONE;
    if bits & 1 != 0 {
        modifiers = modifiers.union(KeyModifiers::SHIFT);
    }
    if bits & 2 != 0 {
        modifiers = modifiers.union(KeyModifiers::ALT);
    }
    if bits & 4 != 0 {
        modifiers = modifiers.union(KeyModifiers::CONTROL);
    }
    modifiers
}

// --- the terminal -----------------------------------------------------------

/// Put the terminal in the mode the program needs: the keys come to the
/// program, one at a time, and the terminal does not echo them.
pub fn enable_raw_mode() -> std::io::Result<()> {
    imp::enable_raw_mode()
}

/// Put the terminal back the way it was.
///
/// The program leaves the terminal the way it found it: a terminal left in
/// the raw mode is a terminal the user cannot type into.
pub fn disable_raw_mode() -> std::io::Result<()> {
    imp::disable_raw_mode()
}

/// The size of the terminal, in cells.
pub fn size() -> std::io::Result<(u16, u16)> {
    imp::size()
}

/// Whether a key is waiting, without waiting for it.
pub fn poll(timeout: std::time::Duration) -> std::io::Result<bool> {
    imp::poll(timeout)
}

/// The next thing the terminal said.
pub fn read() -> std::io::Result<Event> {
    imp::read()
}

// --- the unix half ----------------------------------------------------------

/// The unix half: the terminal, the line discipline and the bytes.
#[cfg(unix)]
mod imp {
    use std::io;
    use std::os::fd::{AsRawFd, RawFd};
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    use super::{Decoded, Event, decode};

    /// The terminal's own attributes.
    ///
    /// The platform's struct, whole: the fields are the platform's, in the
    /// platform's order. Linux and the BSDs differ in one place — Linux
    /// carries the line discipline's own byte and has more control
    /// characters — and the difference is spelled here rather than guessed
    /// at.
    #[cfg(target_os = "linux")]
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Termios {
        c_iflag: u32,
        c_oflag: u32,
        c_cflag: u32,
        c_lflag: u32,
        c_line: u8,
        c_cc: [u8; 32],
        c_ispeed: u32,
        c_ospeed: u32,
    }

    /// The BSD shape of the same struct: no line byte, and twenty control
    /// characters.
    #[cfg(not(target_os = "linux"))]
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Termios {
        c_iflag: u32,
        c_oflag: u32,
        c_cflag: u32,
        c_lflag: u32,
        c_cc: [u8; 20],
        c_ispeed: u32,
        c_ospeed: u32,
    }

    impl Termios {
        /// The attributes with every flag off.
        fn cleared() -> Termios {
            Termios {
                c_iflag: 0,
                c_oflag: 0,
                c_cflag: 0,
                c_lflag: 0,
                #[cfg(target_os = "linux")]
                c_line: 0,
                c_cc: [0; Self::NCCS],
                c_ispeed: 0,
                c_ospeed: 0,
            }
        }

        /// How many control characters the platform has.
        #[cfg(target_os = "linux")]
        const NCCS: usize = 32;
        #[cfg(not(target_os = "linux"))]
        const NCCS: usize = 20;
    }

    // The platform's structs are the platform's sizes: a struct that is
    // short of them is a struct the platform writes past.
    #[cfg(target_os = "linux")]
    const _: () = assert!(std::mem::size_of::<Termios>() == 60);
    #[cfg(not(target_os = "linux"))]
    const _: () = assert!(std::mem::size_of::<Termios>() == 44);
    const _: () = assert!(std::mem::size_of::<PollFd>() == 8);
    const _: () = assert!(std::mem::size_of::<Winsize>() == 8);

    /// The index of the "a character is enough" control character.
    #[cfg(target_os = "linux")]
    const VMIN: usize = 6;
    #[cfg(not(target_os = "linux"))]
    const VMIN: usize = 16;

    /// The index of the "no timer" control character.
    #[cfg(target_os = "linux")]
    const VTIME: usize = 5;
    #[cfg(not(target_os = "linux"))]
    const VTIME: usize = 17;

    // The input flags, in the platform's own spelling.
    const IGNBRK: u32 = 0o000001;
    const BRKINT: u32 = 0o000002;
    const PARMRK: u32 = 0o000010;
    const ISTRIP: u32 = 0o000040;
    const INLCR: u32 = 0o000100;
    const IGNCR: u32 = 0o000200;
    const ICRNL: u32 = 0o000400;
    const IXON: u32 = 0o002000;

    /// The output flag: the terminal does not touch what is written.
    const OPOST: u32 = 0o000001;

    // The local flags.
    const ISIG: u32 = 0o000001;
    const ICANON: u32 = 0o000002;
    const ECHO: u32 = 0o000010;
    const ECHONL: u32 = 0o000100;
    const IEXTEN: u32 = 0o100000;

    // The control flags.
    const CSIZE: u32 = 0o000060;
    const PARENB: u32 = 0o000400;
    const CS8: u32 = 0o000060;

    /// The action that says "now".
    const TCSANOW: i32 = 0;

    /// The request that asks the terminal how big it is.
    ///
    /// The number is the platform's own: Linux spells it as a plain number
    /// and the BSDs as an encoded request.
    #[cfg(target_os = "linux")]
    const TIOCGWINSZ: u64 = 0x5413;
    #[cfg(not(target_os = "linux"))]
    const TIOCGWINSZ: u64 = 0x4008_7468;

    /// The event that says "there is something to read".
    const POLLIN: i16 = 0x001;

    /// How many descriptors `poll` watches.
    ///
    /// The count is the platform's own type: Linux counts in `unsigned
    /// long` and the BSDs in `unsigned int`.
    #[cfg(target_os = "linux")]
    type Count = u64;
    #[cfg(not(target_os = "linux"))]
    type Count = u32;

    unsafe extern "C" {
        fn tcgetattr(fd: i32, attributes: *mut Termios) -> i32;
        fn tcsetattr(fd: i32, actions: i32, attributes: *const Termios) -> i32;
        fn ioctl(fd: i32, request: u64, ...) -> i32;
        #[link_name = "poll"]
        fn poll_fds(fds: *mut PollFd, count: Count, timeout: i32) -> i32;
        #[link_name = "read"]
        fn read_bytes(fd: i32, buffer: *mut core::ffi::c_void, count: usize) -> isize;
    }

    /// The descriptor the terminal watches for input.
    #[repr(C)]
    struct PollFd {
        fd: i32,
        events: i16,
        revents: i16,
    }

    /// The size of the terminal, as the platform spells it.
    #[repr(C)]
    struct Winsize {
        ws_row: u16,
        ws_col: u16,
        ws_xpixel: u16,
        ws_ypixel: u16,
    }

    /// The terminal the keys come from.
    enum Tty {
        /// The standard input, which the program does not own.
        Standard,
        /// A file the program opened, and owns.
        Opened(std::fs::File),
    }

    impl Tty {
        /// The descriptor of the terminal.
        fn fd(&self) -> RawFd {
            match self {
                Tty::Standard => 0,
                Tty::Opened(file) => file.as_raw_fd(),
            }
        }
    }

    /// The terminal, opened once.
    ///
    /// The keys come from the terminal and not from the standard input: a
    /// program whose input is a pipe still has a terminal behind it, and
    /// that is where the user types.
    fn tty() -> io::Result<&'static Tty> {
        static TTY: OnceLock<io::Result<Tty>> = OnceLock::new();
        TTY.get_or_init(|| {
            // The standard input, when it is a terminal: `tcgetattr` is the
            // platform's own test, and it fails for a pipe or a file.
            let mut attributes = Termios::cleared();
            if unsafe { tcgetattr(0, &mut attributes) } == 0 {
                return Ok(Tty::Standard);
            }
            // The terminal behind the program, which a piped input does
            // not name.
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/tty")?;
            Ok(Tty::Opened(file))
        })
        .as_ref()
        .map_err(|e| io::Error::other(e.to_string()))
    }

    /// The attributes the terminal had before the program touched them.
    static ORIGINAL: Mutex<Option<Termios>> = Mutex::new(None);

    /// The bytes that have been read and not yet turned into keys.
    ///
    /// A terminal hands over a run of bytes that may hold several keys, and
    /// a sequence may be split across two reads; the bytes that are left
    /// over are kept here.
    static PENDING: Mutex<Vec<u8>> = Mutex::new(Vec::new());

    /// The attributes of a terminal that gives the program the keyboard.
    fn raw(attributes: &mut Termios) {
        // The input flags: no break signalling, no parity, no stripping and
        // no translation — the bytes the user types arrive as they were
        // typed.
        attributes.c_iflag &= !(IGNBRK | BRKINT | PARMRK | ISTRIP | INLCR | IGNCR | ICRNL | IXON);
        // The output flag: what the program writes is what the terminal
        // shows.
        attributes.c_oflag &= !OPOST;
        // The local flags: no echo, no line editing, no signals and no
        // extensions — the program reads the keys itself.
        attributes.c_lflag &= !(ECHO | ECHONL | ICANON | ISIG | IEXTEN);
        // The control flags: eight bits per character, no parity.
        attributes.c_cflag &= !(CSIZE | PARENB);
        attributes.c_cflag |= CS8;
        // The read returns as soon as there is a character.
        attributes.c_cc[VMIN] = 1;
        attributes.c_cc[VTIME] = 0;
    }

    pub fn enable_raw_mode() -> io::Result<()> {
        let fd = tty()?.fd();
        let mut original = ORIGINAL.lock().unwrap_or_else(|e| e.into_inner());
        if original.is_some() {
            return Ok(());
        }
        let mut attributes = Termios::cleared();
        if unsafe { tcgetattr(fd, &mut attributes) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut raw_attributes = attributes;
        raw(&mut raw_attributes);
        if unsafe { tcsetattr(fd, TCSANOW, &raw_attributes) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // The original is kept only once the raw mode is on: a terminal
        // that was never changed has nothing to put back.
        *original = Some(attributes);
        Ok(())
    }

    pub fn disable_raw_mode() -> io::Result<()> {
        let fd = tty()?.fd();
        let mut original = ORIGINAL.lock().unwrap_or_else(|e| e.into_inner());
        let Some(attributes) = original.take() else {
            return Ok(());
        };
        if unsafe { tcsetattr(fd, TCSANOW, &attributes) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn size() -> io::Result<(u16, u16)> {
        let fd = tty()?.fd();
        let mut size = Winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        if unsafe { ioctl(fd, TIOCGWINSZ, &mut size) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((size.ws_col, size.ws_row))
    }

    /// Whether the terminal has something to say right now.
    fn ready(fd: RawFd) -> io::Result<bool> {
        let mut fds = PollFd {
            fd,
            events: POLLIN,
            revents: 0,
        };
        let ready = unsafe { poll_fds(&mut fds, 1, 0) };
        if ready < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(ready > 0)
    }

    pub fn poll(timeout: Duration) -> io::Result<bool> {
        let fd = tty()?.fd();
        let mut fds = PollFd {
            fd,
            events: POLLIN,
            revents: 0,
        };
        let millis = timeout.as_millis().min(i32::MAX as u128) as i32;
        let ready = unsafe { poll_fds(&mut fds, 1, millis) };
        if ready < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(ready > 0)
    }

    pub fn read() -> io::Result<Event> {
        let fd = tty()?.fd();
        let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        let mut chunk = [0u8; 64];
        loop {
            // A key already in the buffer is a key the last read brought.
            if !pending.is_empty() {
                match decode(&pending, ready(fd)?) {
                    Decoded::Key(event, len) => {
                        pending.drain(..len);
                        return Ok(Event::Key(event));
                    }
                    Decoded::Unknown(len) => {
                        // A sequence this program does not know: it is
                        // dropped, and the next key is read — which may
                        // already be in the buffer.
                        pending.drain(..len);
                        continue;
                    }
                    Decoded::Incomplete => {}
                }
            }
            let count = unsafe { read_bytes(fd, chunk.as_mut_ptr().cast(), chunk.len()) };
            if count < 0 {
                return Err(io::Error::last_os_error());
            }
            if count == 0 {
                // The terminal is gone: there is no key to come.
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the terminal closed",
                ));
            }
            pending.extend_from_slice(&chunk[..count as usize]);
        }
    }
}

// --- the Windows half -------------------------------------------------------

/// The Windows half: the console, its mode, its records and its buffer.
#[cfg(windows)]
mod imp {
    use std::io;
    use std::time::Duration;

    use super::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    /// A handle the platform gave.
    pub type Handle = *mut core::ffi::c_void;

    /// The handle a failed call leaves behind.
    fn invalid() -> Handle {
        usize::MAX as Handle
    }

    /// The standard input.
    const STD_INPUT_HANDLE: u32 = 0xFFFF_FFF6;
    /// The standard output.
    const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5;

    /// The console mode bits that are not the raw mode.
    const ENABLE_PROCESSED_INPUT: u32 = 0x0001;
    const ENABLE_LINE_INPUT: u32 = 0x0002;
    const ENABLE_ECHO_INPUT: u32 = 0x0004;

    /// The console mode bit that makes the console read escape sequences.
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;

    /// The control key state bits.
    const RIGHT_ALT_PRESSED: u32 = 0x0001;
    const LEFT_ALT_PRESSED: u32 = 0x0002;
    const RIGHT_CTRL_PRESSED: u32 = 0x0004;
    const LEFT_CTRL_PRESSED: u32 = 0x0008;
    const SHIFT_PRESSED: u32 = 0x0010;
    const CAPSLOCK_ON: u32 = 0x0080;

    /// The virtual keys the interface names.
    const VK_BACK: u16 = 0x08;
    const VK_TAB: u16 = 0x09;
    const VK_RETURN: u16 = 0x0D;
    const VK_SHIFT: u16 = 0x10;
    const VK_CONTROL: u16 = 0x11;
    const VK_MENU: u16 = 0x12;
    const VK_ESCAPE: u16 = 0x1B;
    const VK_PRIOR: u16 = 0x21;
    const VK_NEXT: u16 = 0x22;
    const VK_END: u16 = 0x23;
    const VK_HOME: u16 = 0x24;
    const VK_LEFT: u16 = 0x25;
    const VK_UP: u16 = 0x26;
    const VK_RIGHT: u16 = 0x27;
    const VK_DOWN: u16 = 0x28;
    const VK_DELETE: u16 = 0x2E;

    /// The event the console writes for a key.
    const KEY_EVENT: u16 = 1;
    /// The event the console writes when the window changed size.
    const WINDOW_BUFFER_SIZE_EVENT: u16 = 4;

    /// The wait that says "the object is ready".
    const WAIT_OBJECT_0: u32 = 0;
    /// The wait that says "the time is up".
    const WAIT_TIMEOUT: u32 = 0x0102;

    unsafe extern "system" {
        fn GetStdHandle(which: u32) -> Handle;
        fn GetConsoleMode(handle: Handle, mode: *mut u32) -> i32;
        fn SetConsoleMode(handle: Handle, mode: u32) -> i32;
        fn GetConsoleScreenBufferInfo(handle: Handle, info: *mut ScreenBufferInfo) -> i32;
        fn ReadConsoleInputW(
            handle: Handle,
            buffer: *mut InputRecord,
            length: u32,
            read: *mut u32,
        ) -> i32;
        fn WaitForSingleObject(handle: Handle, millis: u32) -> u32;
    }

    /// A window, as the platform's handles are.
    type Window = *mut core::ffi::c_void;

    /// A keyboard layout, as the platform's handles are.
    type Layout = *mut core::ffi::c_void;

    // The layout calls.
    //
    // The console answers a key held with the control key with a control
    // code and not with the character the key names, and the keyboard
    // layout is what knows that character. The layout belongs to the
    // window in front, and the platform is asked which thread that window
    // belongs to.
    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetForegroundWindow() -> Window;
        fn GetWindowThreadProcessId(window: Window, process: *mut u32) -> u32;
        fn GetKeyboardLayout(thread: u32) -> Layout;
        fn ToUnicodeEx(
            virtual_key: u32,
            scan_code: u32,
            key_state: *const u8,
            buffer: *mut u16,
            count: i32,
            flags: u32,
            layout: Layout,
        ) -> i32;
    }

    /// The flag that says the kernel's keyboard state is not to be changed.
    const DONT_CHANGE_KERNEL_KEYBOARD_STATE: u32 = 0x0004;

    /// A position on the screen, in cells.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Coord {
        x: i16,
        y: i16,
    }

    /// The rectangle of the screen the window shows.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct SmallRect {
        left: i16,
        top: i16,
        right: i16,
        bottom: i16,
    }

    /// What the console says about its screen buffer.
    #[repr(C)]
    struct ScreenBufferInfo {
        size: Coord,
        cursor_position: Coord,
        attributes: u16,
        window: SmallRect,
        maximum_window_size: Coord,
    }

    /// The character a key carries, in the two spellings the union has.
    ///
    /// The union is as wide as its widest arm — four characters — even
    /// though the character in it is one.
    #[repr(C)]
    #[derive(Clone, Copy)]
    union KeyChar {
        character: u16,
        characters: [u16; 4],
    }

    /// A key the console wrote.
    ///
    /// The platform's struct, whole: the arms are the platform's, in the
    /// platform's order, and the union in the middle is four characters
    /// wide even though the character in it is one.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct KeyEventRecord {
        key_down: i32,
        repeat_count: u16,
        virtual_key_code: u16,
        virtual_scan_code: u16,
        character: KeyChar,
        control_key_state: u32,
    }

    impl KeyEventRecord {
        /// A record with nothing in it.
        fn cleared() -> KeyEventRecord {
            KeyEventRecord {
                key_down: 0,
                repeat_count: 0,
                virtual_key_code: 0,
                virtual_scan_code: 0,
                character: KeyChar { characters: [0; 4] },
                control_key_state: 0,
            }
        }
    }

    /// The record the console writes for a mouse.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MouseEventRecord {
        position: Coord,
        button_state: u32,
        control_key_state: u32,
        event_flags: u32,
    }

    /// The event the console writes, in the union it writes.
    #[repr(C)]
    #[derive(Clone, Copy)]
    union ConsoleEvent {
        key: KeyEventRecord,
        mouse: MouseEventRecord,
    }

    impl ConsoleEvent {
        /// The union with nothing in it.
        ///
        /// The key arm is the widest one, so writing it writes the whole
        /// union — a union with half of it unwritten is a union read past.
        fn cleared() -> ConsoleEvent {
            ConsoleEvent {
                key: KeyEventRecord::cleared(),
            }
        }
    }

    /// The record the console writes.
    ///
    /// The union is as wide as its widest member, and the event type is a
    /// word in front of it.
    #[repr(C)]
    struct InputRecord {
        event_type: u16,
        padding: u16,
        event: ConsoleEvent,
    }

    // The platform's structs are the platform's sizes: a record that is
    // short of them is a record the console writes past.
    const _: () = assert!(std::mem::size_of::<KeyEventRecord>() == 24);
    const _: () = assert!(std::mem::size_of::<MouseEventRecord>() == 16);
    const _: () = assert!(std::mem::size_of::<ConsoleEvent>() == 24);
    const _: () = assert!(std::mem::size_of::<InputRecord>() == 28);
    const _: () = assert!(std::mem::size_of::<ScreenBufferInfo>() == 22);

    /// The standard handle the console gave.
    fn standard_handle(which: u32) -> io::Result<Handle> {
        let handle = unsafe { GetStdHandle(which) };
        if handle.is_null() || handle == invalid() {
            return Err(io::Error::last_os_error());
        }
        Ok(handle)
    }

    /// The console's input handle.
    fn input_handle() -> io::Result<Handle> {
        standard_handle(STD_INPUT_HANDLE)
    }

    /// The console's output handle.
    fn output_handle() -> io::Result<Handle> {
        standard_handle(STD_OUTPUT_HANDLE)
    }

    /// The console's mode.
    fn console_mode(handle: Handle) -> io::Result<u32> {
        let mut mode = 0u32;
        if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(mode)
    }

    /// Ask the console to read the escape sequences.
    ///
    /// The console has to be told to interpret them: without the flag the
    /// sequences are text. An output that is not a console at all is left
    /// alone — the bytes go to a pipe or to a file, and what they mean
    /// there is the reader's business. A console that refuses the flag is a
    /// console that cannot run this interface, and that is an error.
    fn enable_virtual_terminal_processing() -> io::Result<()> {
        let handle = output_handle()?;
        let mode = match console_mode(handle) {
            Ok(mode) => mode,
            Err(_) => return Ok(()),
        };
        if mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING == 0
            && unsafe { SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn enable_raw_mode() -> io::Result<()> {
        // The escape sequences are the console's own language, and the
        // console has to be told to read them.
        enable_virtual_terminal_processing()?;
        let handle = input_handle()?;
        let mode = console_mode(handle)?;
        // The keys come to the program one at a time, and the console does
        // not echo them.
        if unsafe {
            SetConsoleMode(
                handle,
                mode & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn disable_raw_mode() -> io::Result<()> {
        let handle = input_handle()?;
        let mode = console_mode(handle)?;
        if unsafe {
            SetConsoleMode(
                handle,
                mode | (ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn size() -> io::Result<(u16, u16)> {
        let handle = output_handle()?;
        let mut info = ScreenBufferInfo {
            size: Coord { x: 0, y: 0 },
            cursor_position: Coord { x: 0, y: 0 },
            attributes: 0,
            window: SmallRect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            maximum_window_size: Coord { x: 0, y: 0 },
        };
        if unsafe { GetConsoleScreenBufferInfo(handle, &mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // The window, not the buffer: the buffer is as tall as the
        // scrollback, and the window is what the user sees.
        let width = info.window.right as i32 - info.window.left as i32 + 1;
        let height = info.window.bottom as i32 - info.window.top as i32 + 1;
        Ok((width as u16, height as u16))
    }

    pub fn poll(timeout: Duration) -> io::Result<bool> {
        let handle = input_handle()?;
        let millis = timeout.as_millis().min(u32::MAX as u128) as u32;
        let waited = unsafe { WaitForSingleObject(handle, millis) };
        match waited {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => Err(io::Error::last_os_error()),
        }
    }

    /// The next record the console wrote.
    fn next_record() -> io::Result<InputRecord> {
        let handle = input_handle()?;
        let mut record = InputRecord {
            event_type: 0,
            padding: 0,
            event: ConsoleEvent::cleared(),
        };
        let mut read = 0u32;
        if unsafe { ReadConsoleInputW(handle, &mut record, 1, &mut read) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(record)
    }

    pub fn read() -> io::Result<Event> {
        // A character outside the basic plane comes in two records, and the
        // second one is only a character when the first one is still in
        // hand.
        let mut surrogate: Option<u16> = None;
        loop {
            let record = next_record()?;
            if record.event_type == KEY_EVENT {
                // The record is a union, and only the key arm is read.
                let key = unsafe { record.event.key };
                if let Some(event) = key_event(&key, &mut surrogate) {
                    return Ok(Event::Key(event));
                }
                continue;
            }
            if record.event_type == WINDOW_BUFFER_SIZE_EVENT {
                // The window is what the user sees, and the buffer is as
                // tall as the scrollback.
                let (width, height) = size()?;
                return Ok(Event::Resize(width, height));
            }
            // A mouse or a focus change: not this program's business, and
            // the console has more where that came from.
        }
    }

    /// The key a console record names.
    fn key_event(record: &KeyEventRecord, surrogate: &mut Option<u16>) -> Option<KeyEvent> {
        let modifiers = modifiers_of(record.control_key_state);
        let code = match record.virtual_key_code {
            VK_BACK => KeyCode::Backspace,
            VK_TAB => KeyCode::Tab,
            VK_RETURN => KeyCode::Enter,
            VK_ESCAPE => KeyCode::Esc,
            VK_LEFT => KeyCode::Left,
            VK_RIGHT => KeyCode::Right,
            VK_UP => KeyCode::Up,
            VK_DOWN => KeyCode::Down,
            VK_HOME => KeyCode::Home,
            VK_END => KeyCode::End,
            VK_PRIOR => KeyCode::PageUp,
            VK_NEXT => KeyCode::PageDown,
            VK_DELETE => KeyCode::Delete,
            VK_SHIFT | VK_CONTROL | VK_MENU => {
                // The modifier keys are not keys of their own.
                return None;
            }
            _ => {
                // A character: the console translated the key with the
                // user's own keyboard layout, so the character it carries
                // is the answer.
                let character = unsafe { record.character.character };
                match character {
                    0x00..=0x1f => {
                        // The control key was held, and the console
                        // answered with a control code. The key's own
                        // character is in the user's keyboard layout, and
                        // the shift key and the caps lock say which case of
                        // it was meant.
                        let c = layout_character(record)?;
                        KeyCode::Char(ensure_case(c, record.control_key_state))
                    }
                    0xd800..=0xdfff => {
                        // The two halves of a character outside the basic
                        // plane: the first one waits for the second.
                        let c = handle_surrogate(surrogate, character)?;
                        KeyCode::Char(c)
                    }
                    _ => {
                        let c = char::from_u32(character as u32)?;
                        KeyCode::Char(c)
                    }
                }
            }
        };
        let kind = if record.key_down != 0 {
            KeyEventKind::Press
        } else {
            KeyEventKind::Release
        };
        Some(KeyEvent {
            code,
            modifiers,
            kind,
        })
    }

    /// The character the key names, in the user's own keyboard layout.
    ///
    /// The layout belongs to the window in front, and the platform is asked
    /// which thread that window belongs to. When the platform cannot say,
    /// the layout of this thread is the one that is used.
    fn layout_character(record: &KeyEventRecord) -> Option<char> {
        // The modifiers are not part of the question: the layout is asked
        // for the key as it is, and the case is decided afterwards.
        let key_state = [0u8; 256];
        let mut buffer = [0u16; 16];
        let foreground = unsafe { GetForegroundWindow() };
        let thread = unsafe { GetWindowThreadProcessId(foreground, std::ptr::null_mut()) };
        let layout = unsafe { GetKeyboardLayout(thread) };
        let written = unsafe {
            ToUnicodeEx(
                record.virtual_key_code as u32,
                record.virtual_scan_code as u32,
                key_state.as_ptr(),
                buffer.as_mut_ptr(),
                buffer.len() as i32,
                DONT_CHANGE_KERNEL_KEYBOARD_STATE,
                layout,
            )
        };
        // A dead key is -1, and a key with no character is 0.
        if written < 1 {
            return None;
        }
        let mut chars = std::char::decode_utf16(buffer.into_iter().take(written as usize));
        let c = chars.next()?.ok()?;
        if chars.next().is_some() {
            // The key does not map to a single character.
            return None;
        }
        Some(c)
    }

    /// The two halves of a character outside the basic plane.
    ///
    /// The first half waits for the second one, and the character is made
    /// of the two.
    fn handle_surrogate(buffer: &mut Option<u16>, new_surrogate: u16) -> Option<char> {
        match *buffer {
            Some(high) => {
                *buffer = None;
                std::char::decode_utf16([high, new_surrogate])
                    .next()
                    .unwrap()
                    .ok()
            }
            None => {
                *buffer = Some(new_surrogate);
                None
            }
        }
    }

    /// The character in the case the shift key and the caps lock ask for.
    fn ensure_case(c: char, state: u32) -> char {
        if shift_xor_capslock(state) {
            let mut chars = c.to_uppercase();
            let upper = chars.next().unwrap_or(c);
            if chars.next().is_none() { upper } else { c }
        } else {
            let mut chars = c.to_lowercase();
            let lower = chars.next().unwrap_or(c);
            if chars.next().is_none() { lower } else { c }
        }
    }

    /// Whether the shift key and the caps lock disagree.
    fn shift_xor_capslock(state: u32) -> bool {
        let shift = state & SHIFT_PRESSED != 0;
        let capslock = state & CAPSLOCK_ON != 0;
        shift ^ capslock
    }

    /// The modifiers the console's control key state names.
    fn modifiers_of(state: u32) -> KeyModifiers {
        let mut modifiers = KeyModifiers::NONE;
        if state & SHIFT_PRESSED != 0 {
            modifiers = modifiers.union(KeyModifiers::SHIFT);
        }
        if state & (LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED) != 0 {
            modifiers = modifiers.union(KeyModifiers::CONTROL);
        }
        if state & (LEFT_ALT_PRESSED | RIGHT_ALT_PRESSED) != 0 {
            modifiers = modifiers.union(KeyModifiers::ALT);
        }
        modifiers
    }

    /// The console records, held to the oracle's keys.
    ///
    /// The console writes a record for every key: the key the platform
    /// names, the character the console translated it into, and the state
    /// of the modifier keys. The corpus says which key the oracle calls
    /// each of them.
    ///
    /// The corpus stops where the keyboard layout begins: a key held with
    /// the control key is answered by the console with a control code, and
    /// the character the key names is the layout's answer — a question
    /// with an answer that depends on the machine the test runs on.
    #[cfg(test)]
    mod tests {
        use crossterm::event::{
            KeyCode as OracleKeyCode, KeyEvent as OracleKeyEvent,
            KeyEventKind as OracleKeyEventKind, KeyModifiers as OracleKeyModifiers,
        };

        use super::{KeyChar, KeyEventRecord};

        /// The oracle's key, for the tree's key.
        fn oracle_key(
            key: &super::KeyEvent,
        ) -> (OracleKeyCode, OracleKeyModifiers, OracleKeyEventKind) {
            let code = match key.code {
                super::KeyCode::Backspace => OracleKeyCode::Backspace,
                super::KeyCode::Enter => OracleKeyCode::Enter,
                super::KeyCode::Left => OracleKeyCode::Left,
                super::KeyCode::Right => OracleKeyCode::Right,
                super::KeyCode::Up => OracleKeyCode::Up,
                super::KeyCode::Down => OracleKeyCode::Down,
                super::KeyCode::Home => OracleKeyCode::Home,
                super::KeyCode::End => OracleKeyCode::End,
                super::KeyCode::PageUp => OracleKeyCode::PageUp,
                super::KeyCode::PageDown => OracleKeyCode::PageDown,
                super::KeyCode::Delete => OracleKeyCode::Delete,
                super::KeyCode::Esc => OracleKeyCode::Esc,
                super::KeyCode::Tab => OracleKeyCode::Tab,
                super::KeyCode::Char(c) => OracleKeyCode::Char(c),
            };
            let mut modifiers = OracleKeyModifiers::NONE;
            if key.modifiers.contains(super::KeyModifiers::SHIFT) {
                modifiers |= OracleKeyModifiers::SHIFT;
            }
            if key.modifiers.contains(super::KeyModifiers::CONTROL) {
                modifiers |= OracleKeyModifiers::CONTROL;
            }
            if key.modifiers.contains(super::KeyModifiers::ALT) {
                modifiers |= OracleKeyModifiers::ALT;
            }
            let kind = match key.kind {
                super::KeyEventKind::Press => OracleKeyEventKind::Press,
                super::KeyEventKind::Repeat => OracleKeyEventKind::Repeat,
                super::KeyEventKind::Release => OracleKeyEventKind::Release,
            };
            (code, modifiers, kind)
        }

        /// One entry of the corpus: the record the console wrote, and the
        /// key the oracle calls it.
        struct Case {
            /// The key the platform names.
            virtual_key_code: u16,
            /// The character the console carried.
            character: u16,
            /// The state of the modifier keys.
            control_key_state: u32,
            /// The key the oracle calls the record.
            key: Option<OracleKeyEvent>,
        }

        /// The corpus.
        fn corpus() -> Vec<Case> {
            let plain = |code| Some(OracleKeyEvent::new(code, OracleKeyModifiers::NONE));
            let held = |code, modifiers| Some(OracleKeyEvent::new(code, modifiers));
            vec![
                // The keys that have a name of their own.
                Case {
                    virtual_key_code: super::VK_RETURN,
                    character: b'\r' as u16,
                    control_key_state: 0,
                    key: plain(OracleKeyCode::Enter),
                },
                Case {
                    virtual_key_code: super::VK_TAB,
                    character: b'\t' as u16,
                    control_key_state: 0,
                    key: plain(OracleKeyCode::Tab),
                },
                Case {
                    virtual_key_code: super::VK_ESCAPE,
                    character: 0x1b,
                    control_key_state: 0,
                    key: plain(OracleKeyCode::Esc),
                },
                Case {
                    virtual_key_code: super::VK_BACK,
                    character: 0x08,
                    control_key_state: 0,
                    key: plain(OracleKeyCode::Backspace),
                },
                Case {
                    virtual_key_code: super::VK_LEFT,
                    character: 0,
                    control_key_state: 0,
                    key: plain(OracleKeyCode::Left),
                },
                Case {
                    virtual_key_code: super::VK_RIGHT,
                    character: 0,
                    control_key_state: 0,
                    key: plain(OracleKeyCode::Right),
                },
                Case {
                    virtual_key_code: super::VK_UP,
                    character: 0,
                    control_key_state: 0,
                    key: plain(OracleKeyCode::Up),
                },
                Case {
                    virtual_key_code: super::VK_DOWN,
                    character: 0,
                    control_key_state: 0,
                    key: plain(OracleKeyCode::Down),
                },
                Case {
                    virtual_key_code: super::VK_HOME,
                    character: 0,
                    control_key_state: 0,
                    key: plain(OracleKeyCode::Home),
                },
                Case {
                    virtual_key_code: super::VK_END,
                    character: 0,
                    control_key_state: 0,
                    key: plain(OracleKeyCode::End),
                },
                Case {
                    virtual_key_code: super::VK_PRIOR,
                    character: 0,
                    control_key_state: 0,
                    key: plain(OracleKeyCode::PageUp),
                },
                Case {
                    virtual_key_code: super::VK_NEXT,
                    character: 0,
                    control_key_state: 0,
                    key: plain(OracleKeyCode::PageDown),
                },
                Case {
                    virtual_key_code: super::VK_DELETE,
                    character: 0,
                    control_key_state: 0,
                    key: plain(OracleKeyCode::Delete),
                },
                // The modifiers the console's state names.
                Case {
                    virtual_key_code: super::VK_LEFT,
                    character: 0,
                    control_key_state: super::SHIFT_PRESSED,
                    key: held(OracleKeyCode::Left, OracleKeyModifiers::SHIFT),
                },
                Case {
                    virtual_key_code: super::VK_RETURN,
                    character: b'\r' as u16,
                    control_key_state: super::LEFT_CTRL_PRESSED,
                    key: held(OracleKeyCode::Enter, OracleKeyModifiers::CONTROL),
                },
                Case {
                    virtual_key_code: super::VK_RETURN,
                    character: b'\r' as u16,
                    control_key_state: super::LEFT_CTRL_PRESSED | super::LEFT_ALT_PRESSED,
                    key: held(
                        OracleKeyCode::Enter,
                        OracleKeyModifiers::CONTROL | OracleKeyModifiers::ALT,
                    ),
                },
                // The modifier keys are not keys of their own.
                Case {
                    virtual_key_code: super::VK_SHIFT,
                    character: 0,
                    control_key_state: super::SHIFT_PRESSED,
                    key: None,
                },
                Case {
                    virtual_key_code: super::VK_CONTROL,
                    character: 0,
                    control_key_state: super::LEFT_CTRL_PRESSED,
                    key: None,
                },
                Case {
                    virtual_key_code: super::VK_MENU,
                    character: 0,
                    control_key_state: super::LEFT_ALT_PRESSED,
                    key: None,
                },
                // A character the console translated.
                Case {
                    virtual_key_code: 0x41,
                    character: 'a' as u16,
                    control_key_state: 0,
                    key: plain(OracleKeyCode::Char('a')),
                },
                Case {
                    virtual_key_code: 0x41,
                    character: 'A' as u16,
                    control_key_state: super::SHIFT_PRESSED,
                    key: held(OracleKeyCode::Char('A'), OracleKeyModifiers::SHIFT),
                },
                Case {
                    virtual_key_code: 0x41,
                    character: 'ç' as u16,
                    control_key_state: 0,
                    key: plain(OracleKeyCode::Char('ç')),
                },
            ]
        }

        /// The records are the oracle's keys.
        #[test]
        fn the_records_are_the_oracles_keys() {
            for case in corpus() {
                let record = KeyEventRecord {
                    key_down: 1,
                    repeat_count: 1,
                    virtual_key_code: case.virtual_key_code,
                    virtual_scan_code: 0,
                    character: KeyChar {
                        character: case.character,
                    },
                    control_key_state: case.control_key_state,
                };
                let mut surrogate = None;
                let key = super::key_event(&record, &mut surrogate);
                match (&case.key, &key) {
                    (None, None) => {}
                    (Some(wanted), Some(got)) => {
                        let (code, modifiers, kind) = oracle_key(got);
                        assert_eq!(
                            (code, modifiers, kind),
                            (wanted.code, wanted.modifiers, wanted.kind),
                            "the record of {:?} is not the key the oracle calls it",
                            case.virtual_key_code
                        );
                    }
                    _ => panic!(
                        "the record of {:?} is {:?}, and the oracle calls it {:?}",
                        case.virtual_key_code, key, case.key
                    ),
                }
            }
        }
    }
}
