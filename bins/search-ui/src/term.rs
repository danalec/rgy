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

use std::collections::VecDeque;
use std::io::Write;
use std::sync::Mutex;

/// The keys the decoder has already produced from bytes it was given.
///
/// A read hands over a run of bytes that may hold several keys, and the
/// decoder produces one token at a time. Keeping the untokenised bytes in
/// a buffer of its own left the decoded ones invisible to [`poll`]: the
/// buffer is this program's, so the platform answers "nothing waiting",
/// and a caller that polls before it reads — the terminal UI's drain loop
/// does exactly that — took the first key of a paste and held the rest
/// until the next keypress. This queue is the answer to the question
/// `poll` is really asked: *is a key available now?*
static READY: Mutex<VecDeque<KeyEvent>> = Mutex::new(VecDeque::new());

/// Take the next key that has already been decoded, if there is one.
fn take_ready() -> Option<KeyEvent> {
    let mut ready = READY.lock().unwrap_or_else(|e| e.into_inner());
    ready.pop_front()
}

/// Whether a key has already been decoded and is waiting to be taken.
fn has_ready() -> bool {
    !READY.lock().unwrap_or_else(|e| e.into_inner()).is_empty()
}

/// Forget every decoded key, for a raw-mode session that is ending.
fn clear_ready() {
    READY.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// Turn every complete key in `pending` into an entry of `ready`, leaving
/// only a token that is still cut short.
///
/// This is the whole of the "several keys in one read" rule: the tokens
/// are produced here, in order, and the queue is what [`poll`] reads. An
/// unknown sequence is dropped, a cut sequence stays in `pending` for the
/// next read to finish, and a cut sequence with no more bytes coming is
/// dropped rather than waited on for ever.
fn drain_ready(
    pending: &mut Vec<u8>,
    decoder: &mut Decoder,
    more_may_come: bool,
    ready: &mut VecDeque<KeyEvent>,
) {
    while !pending.is_empty() {
        match decoder.decode(pending, more_may_come) {
            Decoded::Key(key, len) => {
                pending.drain(..len);
                ready.push_back(key);
            }
            Decoded::Unknown(len) => {
                pending.drain(..len);
            }
            Decoded::Incomplete => break,
        }
    }
}

#[cfg(test)]
mod ready_tests {
    use super::{Decoder, KeyCode, READY, drain_ready, has_ready, take_ready};

    /// A run of bytes with several keys in it is several keys, in order.
    ///
    /// This is the rule the manual console test found broken: the terminal
    /// hands over a chunk that holds more than one key, and a caller that
    /// polls before it reads must see the second key without waiting for
    /// the next keystroke.
    #[test]
    fn every_key_in_one_run_is_produced_in_order() {
        let mut pending = b"abc".to_vec();
        let mut decoder = Decoder::new();
        let mut queue = std::collections::VecDeque::new();
        drain_ready(&mut pending, &mut decoder, true, &mut queue);
        assert!(pending.is_empty(), "the run was consumed");
        let codes: Vec<KeyCode> = queue.iter().map(|key| key.code).collect();
        assert_eq!(
            codes,
            vec![KeyCode::Char('a'), KeyCode::Char('b'), KeyCode::Char('c')]
        );
    }

    /// A paste in one run is text, and its `\r` is a character.
    #[test]
    fn a_bracketed_paste_run_keeps_its_carriage_return() {
        let mut pending = b"\x1b[200~a\rb\x1b[201~".to_vec();
        let mut decoder = Decoder::new();
        let mut queue = std::collections::VecDeque::new();
        drain_ready(&mut pending, &mut decoder, true, &mut queue);
        assert!(pending.is_empty(), "the burst was consumed");
        let codes: Vec<KeyCode> = queue.iter().map(|key| key.code).collect();
        assert_eq!(
            codes,
            vec![KeyCode::Char('a'), KeyCode::Char('\r'), KeyCode::Char('b')],
            "no Enter, and the markers carry no key of their own"
        );
        assert!(!codes.contains(&KeyCode::Enter), "the paste did not submit");
    }

    /// A sequence cut in two waits for the rest while more may come.
    #[test]
    fn a_cut_sequence_waits_while_more_may_come() {
        let mut pending = b"\xc3".to_vec();
        let mut decoder = Decoder::new();
        let mut queue = std::collections::VecDeque::new();
        drain_ready(&mut pending, &mut decoder, true, &mut queue);
        assert!(queue.is_empty(), "half a character is not a key");
        assert_eq!(pending, b"\xc3", "and the half is kept for the next read");
    }

    /// A sequence cut with nothing following is dropped, not waited on.
    #[test]
    fn a_cut_sequence_with_nothing_more_is_dropped() {
        let mut pending = b"\xc3".to_vec();
        let mut decoder = Decoder::new();
        let mut queue = std::collections::VecDeque::new();
        drain_ready(&mut pending, &mut decoder, false, &mut queue);
        assert!(queue.is_empty(), "no key was invented");
        assert!(pending.is_empty(), "and nothing waits for ever");
    }

    /// The queue is the answer `poll` reads.
    #[test]
    fn the_ready_queue_is_what_poll_reads() {
        // This is the only test that touches the shared queue, so it is
        // deterministic: nothing else in the binary ever fills it.
        READY.lock().unwrap_or_else(|e| e.into_inner()).clear();
        assert!(!has_ready(), "the queue starts empty");
        assert!(take_ready().is_none(), "and hands back nothing");
        READY
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(super::KeyEvent::new(
                KeyCode::Char('x'),
                super::KeyModifiers::NONE,
            ));
        assert!(has_ready(), "a decoded key is a key waiting");
        assert_eq!(take_ready().map(|key| key.code), Some(KeyCode::Char('x')));
        assert!(!has_ready(), "and it is taken once");
    }
}

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

/// The escape that sets a colour.
///
/// The channel is the prefix (`38;` for the foreground, `48;` for the
/// background) and the parameters are the colour itself: the ANSI palette
/// is an index, the 24-bit form is three numbers.
///
/// What the caller is given is what the terminal is told. Whether the
/// terminal is to be painted at all is the **palette's** question
/// (`palette::no_colour`): an emitter that read the environment itself
/// answered that question again for every span, and made a test that pins
/// a colour's bytes depend on the shell it ran in.
fn colour_escape(foreground: bool, colour: Color) -> String {
    let mut out = String::from("\x1b[");
    match colour {
        // The reset is the channel's own number, so it is written
        // without the prefix.
        Color::Reset => out.push_str(if foreground { "39" } else { "49" }),
        _ => {
            out.push_str(if foreground { "38;" } else { "48;" });
            out.push_str(&parameters(colour));
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

    /// Ask the terminal to bracket pastes.
    ///
    /// A terminal that brackets a paste wraps the text in `ESC [ 200 ~` and
    /// `ESC [ 201 ~`, so a line break inside the pasted text can be told from
    /// the enter key and delivered to the query field as a character.
    pub fn enable_bracketed_paste(&mut self) -> std::io::Result<()> {
        self.out.write_all(b"\x1b[?2004h")?;
        Ok(())
    }

    /// Stop asking the terminal to bracket pastes, on the way out.
    pub fn disable_bracketed_paste(&mut self) -> std::io::Result<()> {
        self.out.write_all(b"\x1b[?2004l")?;
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

/// The bytes a terminal writes around a bracketed paste.
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// A decoder for the bytes a terminal sends.
///
/// A bracketed paste burst (`ESC [ 200 ~` … `ESC [ 201 ~`) can be split across
/// two reads, and the bytes inside it, including `\r` and `\n`, are text for
/// the query field rather than keys. This type carries the "inside a paste"
/// bit between calls, so `imp::read()` can hand it one run of bytes at a time
/// and still keep a burst whole.
#[derive(Debug, Clone, Copy, Default)]
pub struct Decoder {
    paste: bool,
}

impl Decoder {
    /// A decoder that is not inside a paste.
    pub const fn new() -> Decoder {
        Decoder { paste: false }
    }

    /// The first token of `bytes`, and what the run says.
    ///
    /// `more_may_come` says whether the terminal may still have bytes to
    /// send: a marker or a character cut in two is [`Decoded::Incomplete`]
    /// while more may come, so the caller reads the rest before deciding.
    pub fn decode(&mut self, bytes: &[u8], more_may_come: bool) -> Decoded {
        if bytes.is_empty() {
            return Decoded::Incomplete;
        }
        if self.paste {
            // The closing marker ends the burst; it carries no key.
            if bytes.starts_with(PASTE_END) {
                self.paste = false;
                return Decoded::Unknown(PASTE_END.len());
            }
            if PASTE_END.starts_with(bytes) && more_may_come {
                return Decoded::Incomplete;
            }
            // Everything else in the burst is literal text: a `\r` or `\n`
            // is a character for the query field, not the enter key that
            // would choose a path and end the session.
            return decode_char(bytes, more_may_come);
        }
        // The opening marker starts the burst; it carries no key.
        if bytes.starts_with(PASTE_START) {
            self.paste = true;
            return Decoded::Unknown(PASTE_START.len());
        }
        // A marker cut in two: wait for the rest, so the whole marker reaches
        // the branch above and opens the burst instead of being dropped by
        // `decode_csi`.
        if PASTE_START.starts_with(bytes) && more_may_come {
            return Decoded::Incomplete;
        }
        decode_key(bytes, more_may_come)
    }
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
///
/// This is the stateless form, for one run that holds a whole key. The reader
/// that must carry bracketed-paste state across reads uses a [`Decoder`].
pub fn decode(bytes: &[u8], more_may_come: bool) -> Decoded {
    Decoder::new().decode(bytes, more_may_come)
}

/// One key of a run of bytes, with no paste state.
fn decode_key(bytes: &[u8], more_may_come: bool) -> Decoded {
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
        _ => decode_char(bytes, more_may_come),
    }
}

/// The character at the start of the run.
///
/// A character that is not whole yet is not a character: the answer is
/// [`Decoded::Incomplete`] and the caller reads more — but only when more
/// bytes may actually come. A truncated lead byte with nothing else behind
/// it (a paste cut in two) is not the beginning of a character, and
/// returning `Incomplete` for it makes `imp::read()` block on a byte that
/// will never arrive. It is dropped instead, the way any other unknown run
/// is.
fn decode_char(bytes: &[u8], more_may_come: bool) -> Decoded {
    let width = utf8_width(bytes[0]);
    if bytes.len() < width {
        return if more_may_come {
            Decoded::Incomplete
        } else {
            Decoded::Unknown(bytes.len())
        };
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

/// How long `read()` waits for a key before it re-checks the terminal's size.
///
/// A resize while idle is invisible to a blocking read: without a bounded
/// wait the frame stays at the old size until the next keystroke. A ~100 ms
/// tick keeps both platforms' wait/poll loops bounded, and there is no
/// signal handler to install on unix.
const RESIZE_TICK: std::time::Duration = std::time::Duration::from_millis(100);

/// Whether a fresh size differs from the size the frame was drawn at.
///
/// Pure, so a simulated size change can be held to the resize path without
/// a terminal: a change is a resize to repaint, an unchanged size is not.
pub(crate) fn size_changed(drawn: (u16, u16), now: (u16, u16)) -> bool {
    drawn != now
}

/// The [`Event::Resize`] a fresh reading of the terminal's size implies.
///
/// `drawn` is the size the frame was last painted at, `now` the size it has
/// now. Both platform read loops call this on every tick; a change makes
/// `read()` return a resize the runner answers with a repaint.
pub(crate) fn resize_event(drawn: (u16, u16), now: (u16, u16)) -> Option<Event> {
    size_changed(drawn, now).then_some(Event::Resize(now.0, now.1))
}

// --- the unix half ----------------------------------------------------------

/// The unix half: the terminal, the line discipline and the bytes.
#[cfg(unix)]
mod imp {
    use std::io;
    use std::os::fd::{AsRawFd, RawFd};
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    use super::{Decoder, Event, READY, clear_ready, drain_ready, has_ready, take_ready};

    /// The terminal's own attributes.
    ///
    /// The platform's struct, whole: the fields are the platform's, in the
    /// platform's order. There are three shapes, not one:
    ///
    /// * Linux: `tcflag_t` and `speed_t` are `u32`, plus the line
    ///   discipline's own byte and 32 control characters (60 bytes).
    /// * macOS: `tcflag_t` and `speed_t` are `c_ulong` — 8 bytes on the
    ///   64-bit targets this tree ships — with 20 control characters
    ///   (72 bytes). A 44-byte BSD struct here is 28 bytes short of what
    ///   `tcgetattr` writes, so it is a stack overwrite, not a wrong flag.
    /// * The other BSDs (FreeBSD carries this tree's CI): `u32` flags and
    ///   speeds, 20 control characters (44 bytes).
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

    #[cfg(target_os = "macos")]
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Termios {
        c_iflag: u64,
        c_oflag: u64,
        c_cflag: u64,
        c_lflag: u64,
        c_cc: [u8; 20],
        c_ispeed: u64,
        c_ospeed: u64,
    }

    #[cfg(all(not(target_os = "linux"), not(target_os = "macos")))]
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
    // short of them is a struct the platform writes past. macOS's 72 bytes
    // are the reason there is a third shape — the BSD-u32 shape is correct
    // for FreeBSD but 28 bytes short on Darwin.
    #[cfg(target_os = "linux")]
    const _: () = assert!(std::mem::size_of::<Termios>() == 60);
    #[cfg(target_os = "macos")]
    const _: () = assert!(std::mem::size_of::<Termios>() == 72);
    #[cfg(all(not(target_os = "linux"), not(target_os = "macos")))]
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

    /// The termios flags in the platform's own spelling.
    ///
    /// Linux and the BSDs do not agree on the numbers: Linux spells `ICANON`
    /// as `0o2` and the BSDs as `0x100`, and macOS's flags are 8 bytes wide
    /// while Linux's and FreeBSD's are 4. One set of Linux octal constants
    /// silently cleared the wrong bits on every BSD, so raw mode was never
    /// fully on there. The values below are the ones `libc` ships for each
    /// family (`src/unix/linux_like` and `src/unix/bsd`).
    #[cfg(target_os = "linux")]
    mod flags {
        pub type Flag = u32;
        pub const IGNBRK: Flag = 0o000001;
        pub const BRKINT: Flag = 0o000002;
        pub const PARMRK: Flag = 0o000010;
        pub const ISTRIP: Flag = 0o000040;
        pub const INLCR: Flag = 0o000100;
        pub const IGNCR: Flag = 0o000200;
        pub const ICRNL: Flag = 0o000400;
        pub const IXON: Flag = 0o002000;
        pub const OPOST: Flag = 0o000001;
        pub const ISIG: Flag = 0o000001;
        pub const ICANON: Flag = 0o000002;
        pub const ECHO: Flag = 0o000010;
        pub const ECHONL: Flag = 0o000100;
        pub const IEXTEN: Flag = 0o100000;
        pub const CSIZE: Flag = 0o000060;
        pub const PARENB: Flag = 0o000400;
        pub const CS8: Flag = 0o000060;
    }

    #[cfg(not(target_os = "linux"))]
    mod flags {
        /// `tcflag_t` is `c_ulong` on Darwin, `c_uint` on the other BSDs.
        #[cfg(target_os = "macos")]
        pub type Flag = u64;
        #[cfg(not(target_os = "macos"))]
        pub type Flag = u32;

        pub const IGNBRK: Flag = 0x00000001;
        pub const BRKINT: Flag = 0x00000002;
        pub const PARMRK: Flag = 0x00000008;
        pub const ISTRIP: Flag = 0x00000020;
        pub const INLCR: Flag = 0x00000040;
        pub const IGNCR: Flag = 0x00000080;
        pub const ICRNL: Flag = 0x00000100;
        pub const IXON: Flag = 0x00000200;
        pub const OPOST: Flag = 0x00000001;
        pub const ISIG: Flag = 0x00000080;
        pub const ICANON: Flag = 0x00000100;
        pub const ECHO: Flag = 0x00000008;
        pub const ECHONL: Flag = 0x00000010;
        pub const IEXTEN: Flag = 0x00000400;
        pub const CSIZE: Flag = 0x00000300;
        pub const PARENB: Flag = 0x00001000;
        pub const CS8: Flag = 0x00000300;
    }

    #[cfg(test)]
    mod termios_tests {
        use super::*;

        /// The flag values are the platform's own. One set of Linux octal
        /// constants silently cleared the wrong bits on every BSD, so raw
        /// mode was never fully on there; pin the values the platform
        /// headers (and `libc`) actually define.
        #[test]
        fn the_flags_are_the_platforms_own() {
            #[cfg(target_os = "linux")]
            {
                assert_eq!(flags::ICANON, 0o000002);
                assert_eq!(flags::ISIG, 0o000001);
                assert_eq!(flags::IEXTEN, 0o100000);
            }
            #[cfg(not(target_os = "linux"))]
            {
                assert_eq!(flags::ICANON, 0x00000100);
                assert_eq!(flags::ISIG, 0x00000080);
                assert_eq!(flags::IEXTEN, 0x00000400);
            }
        }

        /// A struct shorter than the platform's is a buffer `tcgetattr`
        /// writes past. Darwin's 72 bytes are the reason there are three
        /// shapes rather than the old Linux/BSD pair.
        #[test]
        fn the_struct_is_the_platforms_size() {
            #[cfg(target_os = "linux")]
            assert_eq!(std::mem::size_of::<Termios>(), 60);
            #[cfg(target_os = "macos")]
            assert_eq!(std::mem::size_of::<Termios>(), 72);
            #[cfg(all(not(target_os = "linux"), not(target_os = "macos")))]
            assert_eq!(std::mem::size_of::<Termios>(), 44);
        }

        /// The terminal's own numbers are the platform's, and nothing else
        /// in the tree pins them.
        ///
        /// `TIOCGWINSZ` is a request number the tree declares by hand, and
        /// `libc` stopped exposing the Apple one after 0.2.186 — so this
        /// test is the only place the value is written down twice. It is
        /// `_IOR('t', 104, struct winsize)`: out, length 8, group `t`,
        /// number 104, which is `0x40087468` on every BSD. The three
        /// indices are the ones `c_cc` uses for "a character is enough",
        /// "no timer" and the platform's control-character count.
        #[test]
        fn the_terminal_numbers_are_the_platforms_own() {
            #[cfg(target_os = "linux")]
            {
                assert_eq!(TIOCGWINSZ, 0x5413);
                assert_eq!(Termios::NCCS, 32);
                assert_eq!((VMIN, VTIME), (6, 5));
            }
            #[cfg(not(target_os = "linux"))]
            {
                assert_eq!(TIOCGWINSZ, 0x4008_7468);
                assert_eq!(Termios::NCCS, 20);
                assert_eq!((VMIN, VTIME), (16, 17));
            }
            assert_eq!(TCSANOW, 0);
            assert_eq!(POLLIN, 0x001);
        }

        /// `raw()` clears the flags it names and arms the one-character read.
        #[test]
        fn raw_mode_clears_the_local_flags() {
            let mut attributes = Termios::cleared();
            attributes.c_lflag = !0;
            attributes.c_iflag = !0;
            attributes.c_oflag = !0;
            raw(&mut attributes);
            assert_eq!(
                attributes.c_lflag
                    & (flags::ECHO | flags::ECHONL | flags::ICANON | flags::ISIG | flags::IEXTEN),
                0
            );
            assert_eq!(attributes.c_oflag & flags::OPOST, 0);
            assert_eq!(attributes.c_cc[VMIN], 1);
            assert_eq!(attributes.c_cc[VTIME], 0);
        }
    }

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

    /// The decoder that turns the pending bytes into keys. It carries the
    /// "inside a bracketed paste" bit, which has to survive a paste split
    /// across two reads.
    static DECODER: Mutex<Decoder> = Mutex::new(Decoder::new());

    /// The attributes of a terminal that gives the program the keyboard.
    fn raw(attributes: &mut Termios) {
        // Imported here, not at module scope: every constant is used in this
        // function, so the import cannot be an unused one on any platform.
        use self::flags::*;
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
        // A key of this session is not a key of the next one.
        clear_ready();
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
        // A key the decoder produced but nobody took yet is a key waiting,
        // even though the platform has nothing left to hand over.
        if has_ready() {
            return Ok(true);
        }
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
        // The size the frame was drawn at. A resize while idle is measured
        // against it on each tick of the bounded wait below.
        let drawn = size()?;
        let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        let mut decoder = DECODER.lock().unwrap_or_else(|e| e.into_inner());
        let mut chunk = [0u8; 64];
        loop {
            // A key the decoder has already produced is a key this read
            // owes the caller before it waits for anything.
            if let Some(key) = take_ready() {
                return Ok(Event::Key(key));
            }
            // A run of bytes that arrived whole can hold several keys; they
            // are all turned into events here, so `poll` can see them.
            if !pending.is_empty() {
                let more = ready(fd)?;
                let mut queue = READY.lock().unwrap_or_else(|e| e.into_inner());
                drain_ready(&mut pending, &mut decoder, more, &mut queue);
                drop(queue);
                if let Some(key) = take_ready() {
                    return Ok(Event::Key(key));
                }
            }
            // Wait a bounded tick for the next bytes. A tick that expires
            // with nothing to read is a chance to notice that the terminal
            // changed size while idle, so a resize is not left until the
            // next keystroke.
            if !poll(super::RESIZE_TICK)? {
                let now = size()?;
                if let Some(event) = super::resize_event(drawn, now) {
                    return Ok(event);
                }
                continue;
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
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::{Decoder, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    // The decodable token is named by the tests only: production drains
    // through the parent module's rule and never matches on it here.
    #[cfg(test)]
    use super::Decoded;

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
    /// Makes the console report window-buffer-size changes as input records;
    /// without it the `WINDOW_BUFFER_SIZE_EVENT` branch in `read()` is dead.
    const ENABLE_WINDOW_INPUT: u32 = 0x0008;

    /// The console mode bit that makes the console read escape sequences.
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;

    /// The console mode bit that turns key input into VT byte sequences for
    /// `ReadFile`. It does not affect `ReadConsoleInputW`, so the key-record
    /// path stays as the fallback when a console refuses this flag.
    const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;

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
        fn ReadFile(
            handle: Handle,
            buffer: *mut u8,
            count: u32,
            read: *mut u32,
            overlapped: *mut core::ffi::c_void,
        ) -> i32;
        fn WaitForSingleObject(handle: Handle, millis: u32) -> u32;
        fn GetConsoleWindow() -> Handle;
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
        fn GetClassNameW(window: Window, buffer: *mut u16, max: i32) -> i32;
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

    /// The console modes the program found before it touched them.
    ///
    /// The doc says the program leaves the terminal the way it was found,
    /// and an OR of three flags is not that: a console that had echo or
    /// processed input off gets them forced on, and the output console stays
    /// in VT-processing mode forever. Both handles' original modes are kept
    /// and restored exactly.
    static ORIGINAL_MODES: Mutex<Option<ConsoleModes>> = Mutex::new(None);

    #[derive(Clone, Copy)]
    struct ConsoleModes {
        input: u32,
        /// `None` when the output is a pipe or a file, which has no console
        /// mode to save or restore.
        output: Option<u32>,
    }

    /// Whether the console accepted `ENABLE_VIRTUAL_TERMINAL_INPUT`.
    ///
    /// `read()` uses this to choose between the VT byte path (`ReadFile` and
    /// the shared `Decoder`) and the legacy `ReadConsoleInputW` key-record
    /// path. It starts false — before raw mode is enabled no console has
    /// been asked — and `disable_raw_mode` resets it, so a console that
    /// refused the flag keeps delivering key records on the next session.
    static VT_INPUT: AtomicBool = AtomicBool::new(false);

    /// The input mode `enable_raw_mode` asks the console for.
    ///
    /// The three line-editing flags are always off: `ENABLE_LINE_INPUT`
    /// must stay off so `ReadFile` returns as soon as bytes are available.
    /// `ENABLE_WINDOW_INPUT` is kept for the key-record fallback, where a
    /// resize arrives as an input record; the VT byte path ignores those
    /// records (`ReadFile` filters them) and uses the bounded `size()` tick.
    /// `vt` adds `ENABLE_VIRTUAL_TERMINAL_INPUT` for the first attempt.
    fn raw_input_mode(input_mode: u32, vt: bool) -> u32 {
        let mut mode = (input_mode
            & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT))
            | ENABLE_WINDOW_INPUT;
        if vt {
            mode |= ENABLE_VIRTUAL_TERMINAL_INPUT;
        } else {
            // The fallback is genuinely *without* the VT flag, even when the
            // original mode happened to carry it: refusing the first attempt
            // must put the console back on the key-record path.
            mode &= !ENABLE_VIRTUAL_TERMINAL_INPUT;
        }
        mode
    }

    /// The read path `enable_raw_mode` chose.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum InputPath {
        /// The console accepted the VT flag: `ReadFile` + `Decoder`.
        VtBytes,
        /// The console refused it: `ReadConsoleInputW` key records.
        KeyRecords,
    }

    /// Choose the read path from the two `SetConsoleMode` attempts.
    ///
    /// A pure seam: `vt_accepted` is whether the console took the mode with
    /// `ENABLE_VIRTUAL_TERMINAL_INPUT`, and `raw_accepted` whether it took
    /// the mode without it. Refusing the VT flag degrades to key records
    /// with no error; `None` means both were refused and is an error.
    fn input_path(vt_accepted: bool, raw_accepted: bool) -> Option<InputPath> {
        match (vt_accepted, raw_accepted) {
            (true, _) => Some(InputPath::VtBytes),
            (false, true) => Some(InputPath::KeyRecords),
            (false, false) => None,
        }
    }

    /// Whether the console is a pseudoconsole, where the VT byte path is
    /// worth taking.
    ///
    /// The legality of the flag is not the question: a legacy conhost
    /// accepts `ENABLE_VIRTUAL_TERMINAL_INPUT` as well, and it is the wrong
    /// console to take it on, because it implements no bracketed paste and
    /// so nothing the byte path could consume.
    ///
    /// Measured on 2026-11-07, driving a real console with this probe, in
    /// three states. In conhost's **own** mode the paste worked on `Ctrl+V`
    /// and on `Shift+Insert`, and the pasted line break arrived as the
    /// enter key — the defect this work started from. With **raw mode** on
    /// and the VT flag accepted, `Ctrl+V` arrived as the key with the
    /// control modifier and `Shift+Insert` delivered nothing. With **raw
    /// mode** on and no VT flag — the path taken now — `Ctrl+V` arrived as
    /// `Char('v')`, and no paste happened at all.
    ///
    /// So the earlier reading of these measurements was wrong, and the last
    /// one corrects it: the flag was never what cost conhost its paste
    /// shortcuts. **Raw mode** does, whichever read path is in use, because
    /// conhost pastes through the line discipline that raw mode turns off
    /// (`ENABLE_PROCESSED_INPUT` is one of the bits a raw-mode program
    /// clears, and it is what makes the console process a control chord
    /// instead of handing the key over). A terminal that speaks through a
    /// pseudoconsole does its own paste and marks it with the bracketed
    /// markers, which is why the byte path is worth taking only there:
    /// measured the same day, on the same probe, the flag is accepted, the
    /// burst is marked, and a pasted carriage return arrives as a character.
    ///
    /// The consequence for the user is recorded rather than hidden: a
    /// raw-mode terminal UI cannot paste at all in a legacy conhost. That is
    /// the console's design and not something consuming VT bytes could fix,
    /// and it is why the limit to record is "no bracketed paste" rather than
    /// "the pasted line break submits".
    ///
    /// The pseudoconsole's own window is what the console reports, and its
    /// class is the platform's name for it. A console this cannot classify
    /// keeps the key-record path, which is the behaviour it had before the
    /// byte path existed.
    fn pseudoconsole() -> bool {
        let window = unsafe { GetConsoleWindow() };
        if window.is_null() {
            return false;
        }
        let mut buffer = [0u16; 64];
        let count = unsafe { GetClassNameW(window, buffer.as_mut_ptr(), buffer.len() as i32) };
        if count <= 0 {
            return false;
        }
        let class = String::from_utf16_lossy(&buffer[..count as usize]);
        is_pseudoconsole_class(&class)
    }

    /// The class of the window a pseudoconsole reports.
    const PSEUDOCONSOLE_WINDOW_CLASS: &str = "PseudoConsoleWindow";

    /// The class of a legacy conhost window, which the production code
    /// does not need to name: it takes the byte path for one class and the
    /// key-record path for every other. The tests name it so the rule they
    /// state is the one this box measured.
    #[cfg(test)]
    const CONHOST_WINDOW_CLASS: &str = "ConsoleWindowClass";

    /// Whether a console window's class names a pseudoconsole.
    ///
    /// A pure seam, so the classification is a rule the tests can state
    /// without a live console. Measured on 2026-11-07: Windows Terminal's
    /// console reports `PseudoConsoleWindow`, and a legacy conhost reports
    /// `ConsoleWindowClass`.
    fn is_pseudoconsole_class(class: &str) -> bool {
        class == PSEUDOCONSOLE_WINDOW_CLASS
    }

    pub fn enable_raw_mode() -> io::Result<()> {
        let input = input_handle()?;
        let output = output_handle()?;
        let input_mode = console_mode(input)?;
        // An output that is not a console has no mode: the bytes go to a
        // pipe or a file, and what they mean there is the reader's business.
        let output_mode = console_mode(output).ok();

        let mut saved = ORIGINAL_MODES.lock().unwrap_or_else(|e| e.into_inner());
        if saved.is_some() {
            return Ok(());
        }

        // Ask the console to interpret escape sequences, but only set the
        // flag when it was off; the saved mode is what disable puts back.
        if let Some(mode) = output_mode
            && mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING == 0
            && unsafe { SetConsoleMode(output, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) } == 0
        {
            return Err(io::Error::last_os_error());
        }

        // Try to put the console on the VT byte path, but only where the
        // path buys something: a pseudoconsole implements bracketed paste,
        // and a legacy conhost does not (and loses its own paste shortcuts
        // when the flag is on). The original input mode is saved above and
        // restored exactly by `disable_raw_mode`, so every bit changed here
        // is put back on the way out.
        let vt_accepted = pseudoconsole()
            && unsafe { SetConsoleMode(input, raw_input_mode(input_mode, true)) } != 0;
        let raw_accepted = if vt_accepted {
            // The second attempt is not needed when the first succeeded.
            true
        } else {
            let fell_back = unsafe { SetConsoleMode(input, raw_input_mode(input_mode, false)) };
            fell_back != 0
        };
        let Some(path) = input_path(vt_accepted, raw_accepted) else {
            let err = io::Error::last_os_error();
            // A failed enable must not leave the output half-changed.
            if let Some(mode) = output_mode {
                let _ = unsafe { SetConsoleMode(output, mode) };
            }
            return Err(err);
        };
        VT_INPUT.store(path == InputPath::VtBytes, Ordering::Relaxed);

        *saved = Some(ConsoleModes {
            input: input_mode,
            output: output_mode,
        });
        Ok(())
    }

    pub fn disable_raw_mode() -> io::Result<()> {
        // The next session must not inherit this one's VT answer, a half-read
        // sequence, or a decoder that is still inside a paste.
        reset_vt_state();
        let input = input_handle()?;
        let output = output_handle()?;
        let mut saved = ORIGINAL_MODES.lock().unwrap_or_else(|e| e.into_inner());
        let Some(modes) = saved.take() else {
            return Ok(());
        };

        let mut first_error = None;
        if unsafe { SetConsoleMode(input, modes.input) } == 0 {
            first_error = Some(io::Error::last_os_error());
        }
        if let Some(mode) = modes.output
            && unsafe { SetConsoleMode(output, mode) } == 0
            && first_error.is_none()
        {
            first_error = Some(io::Error::last_os_error());
        }
        match first_error {
            Some(err) => Err(err),
            None => Ok(()),
        }
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
        // A key the decoder produced but nobody took yet is a key waiting,
        // even though the console has nothing left to hand over.
        if super::has_ready() {
            return Ok(true);
        }
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

    /// The bytes `ReadFile` returned and not yet turned into keys.
    ///
    /// A paste burst can be split across reads, and one read can hold
    /// several keys; the bytes left over are kept here.
    static PENDING: Mutex<Vec<u8>> = Mutex::new(Vec::new());

    /// The decoder that turns the pending VT bytes into keys. It is the
    /// same `Decoder` across every read, so `ESC[200~` in one read and
    /// `ESC[201~` in the next still deliver the `\r` between them as a
    /// character and never as the enter key.
    static DECODER: Mutex<Decoder> = Mutex::new(Decoder::new());

    /// Put the VT byte path back to its initial state.
    ///
    /// `disable_raw_mode` calls this so a later session does not inherit a
    /// "VT accepted" answer, a half-read sequence, or a decoder that is
    /// still inside a paste.
    fn reset_vt_state() {
        VT_INPUT.store(false, Ordering::Relaxed);
        PENDING.lock().unwrap_or_else(|e| e.into_inner()).clear();
        *DECODER.lock().unwrap_or_else(|e| e.into_inner()) = Decoder::new();
        super::clear_ready();
    }

    /// Whether more input is waiting on the console right now.
    ///
    /// A zero-length wait: the Windows answer to unix's `ready(fd)`. It
    /// says whether a sequence cut at the end of a `ReadFile` chunk may be
    /// completed by bytes the console already holds, so a lone `ESC` is
    /// still the escape key when nothing follows it.
    fn ready(handle: Handle) -> io::Result<bool> {
        let waited = unsafe { WaitForSingleObject(handle, 0) };
        match waited {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => Err(io::Error::last_os_error()),
        }
    }

    /// Decode the next key from `pending` and remove it from the buffer.
    ///
    /// The pure seam shared by `read_vt_bytes()` and its tests. Unknown
    /// sequences are dropped on the way, exactly as the unix read loop
    /// drops them. `None` means `pending` is empty or holds only a sequence
    /// that is not whole yet, so the caller must read more bytes. The
    /// `Decoder` is passed in and kept across calls, which is what keeps a
    /// bracketed paste whole when it is split across reads.
    /// The next event on the VT byte path.
    ///
    /// Mirrors the unix read loop: decode what is already pending, wait a
    /// bounded tick (a timeout re-reads `size()` and can emit a resize),
    /// then `ReadFile` a chunk of VT bytes and decode again. Resize comes
    /// only from that bounded `size()` comparison — `ENABLE_WINDOW_INPUT`
    /// records are filtered by `ReadFile`, so the byte path never sees one.
    fn read_vt_bytes() -> io::Result<Event> {
        let handle = input_handle()?;
        // The size the frame was drawn at. A resize while idle is measured
        // against it on each tick of the bounded wait below.
        let drawn = size()?;
        let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        let mut decoder = DECODER.lock().unwrap_or_else(|e| e.into_inner());
        let mut chunk = [0u8; 64];
        loop {
            // A key the decoder has already produced is a key this read
            // owes the caller before it waits for anything.
            if let Some(key) = super::take_ready() {
                return Ok(Event::Key(key));
            }
            // A read of VT bytes can hold several keys; they are all turned
            // into events here, so `poll` can see them.
            if !pending.is_empty() {
                let more = ready(handle)?;
                let mut queue = super::READY.lock().unwrap_or_else(|e| e.into_inner());
                super::drain_ready(&mut pending, &mut decoder, more, &mut queue);
                drop(queue);
                if let Some(key) = super::take_ready() {
                    return Ok(Event::Key(key));
                }
            }
            // Wait a bounded tick for the next bytes. A tick that expires
            // with nothing to read is a chance to notice that the terminal
            // changed size while idle, so a resize is not left until the
            // next keystroke.
            if !poll(super::RESIZE_TICK)? {
                let now = size()?;
                if let Some(event) = super::resize_event(drawn, now) {
                    return Ok(event);
                }
                continue;
            }
            let mut count = 0u32;
            if unsafe {
                ReadFile(
                    handle,
                    chunk.as_mut_ptr(),
                    chunk.len() as u32,
                    &mut count,
                    std::ptr::null_mut(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            if count == 0 {
                // A window-input record is filtered out by ReadFile and
                // leaves no bytes: re-check the size and wait for a key,
                // rather than treating it as the terminal closing.
                continue;
            }
            pending.extend_from_slice(&chunk[..count as usize]);
        }
    }

    pub fn read() -> io::Result<Event> {
        // A console that accepted `ENABLE_VIRTUAL_TERMINAL_INPUT` delivers
        // VT byte sequences — including bracketed-paste markers — through
        // `ReadFile`; every other console keeps the key-record path below.
        if VT_INPUT.load(Ordering::Relaxed) {
            return read_vt_bytes();
        }
        // The size the frame was drawn at. A resize while idle is measured
        // against it on each tick of the bounded wait below.
        let drawn = size()?;
        // A character outside the basic plane comes in two records, and the
        // second one is only a character when the first one is still in
        // hand.
        let mut surrogate: Option<u16> = None;
        loop {
            // A bounded wait for a console record. A tick that expires with
            // no record is a chance to notice that the terminal changed size
            // while idle, so a resize is not left until the next keystroke.
            // With `ENABLE_WINDOW_INPUT` set, a resize also wakes the wait
            // at once as a `WINDOW_BUFFER_SIZE_EVENT` handled below.
            if !poll(super::RESIZE_TICK)? {
                let now = size()?;
                if let Some(event) = super::resize_event(drawn, now) {
                    return Ok(event);
                }
                continue;
            }
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

        use super::{
            DECODER, Decoded, Decoder, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT,
            ENABLE_PROCESSED_INPUT, ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_WINDOW_INPUT, Event,
            InputPath, KeyChar, KeyCode, KeyEvent, KeyEventRecord, KeyModifiers, Ordering, PENDING,
            VT_INPUT, input_path, is_pseudoconsole_class, raw_input_mode, reset_vt_state,
        };

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

        // -- the Windows VT byte path -------------------------------------
        //
        // This `mod tests` lives inside `#[cfg(windows)] mod imp`, so every
        // test here is a Windows-only test of the VT byte reader. It is fed
        // simulated VT bytes through `decode_vt`, the same pure seam
        // `read_vt_bytes()` calls, with no live console involved.

        /// Feed a chunk of simulated VT bytes through the rule
        /// `read_vt_bytes()` drains its pending buffer with, and collect
        /// the events. One decoder is kept across calls so a burst split
        /// across chunks still stays a burst. There is one implementation
        /// of the rule, in the parent module, and this is it.
        fn drain_vt(
            decoder: &mut Decoder,
            pending: &mut Vec<u8>,
            chunk: &[u8],
            more_may_come: bool,
        ) -> Vec<Event> {
            pending.extend_from_slice(chunk);
            let mut queue = std::collections::VecDeque::new();
            super::super::drain_ready(pending, decoder, more_may_come, &mut queue);
            queue.into_iter().map(Event::Key).collect()
        }

        /// The VT byte path is taken on a pseudoconsole and not on the
        /// legacy host: conhost accepts the flag and implements no
        /// bracketed paste, and taking the flag there costs it its own
        /// paste shortcuts (measured on 2026-11-07).
        #[test]
        fn a_pseudoconsole_is_what_earns_the_byte_path() {
            assert!(
                is_pseudoconsole_class(super::PSEUDOCONSOLE_WINDOW_CLASS),
                "the pseudoconsole's class is the one that takes the byte path"
            );
            assert!(
                !is_pseudoconsole_class(super::CONHOST_WINDOW_CLASS),
                "a legacy conhost keeps the key-record path"
            );
            assert!(
                !is_pseudoconsole_class(""),
                "a console this cannot classify keeps the old behaviour"
            );
        }

        /// The code of a key event, for a run that must hold only keys.
        fn key_codes(events: &[Event]) -> Vec<KeyCode> {
            events
                .iter()
                .map(|event| match event {
                    Event::Key(key) => key.code,
                    Event::Resize(_, _) => panic!("a paste is not a resize"),
                })
                .collect()
        }

        /// A bracketed paste on the Windows VT byte path is literal text:
        /// the `\r` inside the burst is a character, never the enter key
        /// that would choose a path and end the session.
        #[test]
        fn a_vt_paste_burst_is_characters_not_enter() {
            let mut decoder = Decoder::new();
            let mut pending = Vec::new();
            let events = drain_vt(&mut decoder, &mut pending, b"\x1b[200~a\rb\x1b[201~", false);
            let codes = key_codes(&events);
            assert_eq!(
                codes,
                vec![KeyCode::Char('a'), KeyCode::Char('\r'), KeyCode::Char('b')],
                "a paste is literal text, not keys"
            );
            assert!(
                !codes.contains(&KeyCode::Enter),
                "a carriage return inside a paste must not be the enter key"
            );
            assert!(pending.is_empty(), "the whole burst was consumed");
        }

        /// A carriage return outside a paste is still the enter key, and the
        /// paste state does not leak past the closing marker.
        #[test]
        fn a_carriage_return_outside_a_vt_paste_is_enter() {
            let mut decoder = Decoder::new();
            let mut pending = Vec::new();
            let events = drain_vt(&mut decoder, &mut pending, b"\r", false);
            assert_eq!(
                events,
                vec![Event::Key(KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::NONE
                ))],
                "a \\r outside a paste ends the session"
            );

            // A burst followed by a \r: the burst is text, the \r is Enter.
            let events = drain_vt(&mut decoder, &mut pending, b"\x1b[200~a\x1b[201~\r", false);
            assert_eq!(key_codes(&events)[0], KeyCode::Char('a'));
            assert_eq!(
                events.last().copied(),
                Some(Event::Key(KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::NONE
                ))),
                "the \\r after the burst is the enter key"
            );
        }

        /// A paste split across two reads still delivers its text: the same
        /// `Decoder` is reused, so the `\r` between the markers is a
        /// character even though the markers arrived in different chunks.
        #[test]
        fn a_vt_paste_split_across_reads_keeps_the_burst_whole() {
            let mut decoder = Decoder::new();
            let mut pending = Vec::new();
            // The first chunk ends in the middle of the closing marker.
            let first = drain_vt(&mut decoder, &mut pending, b"\x1b[200~a\rb\x1b[20", true);
            assert_eq!(
                key_codes(&first),
                vec![KeyCode::Char('a'), KeyCode::Char('\r'), KeyCode::Char('b')]
            );
            assert_eq!(
                pending.as_slice(),
                &b"\x1b[20"[..],
                "the unfinished marker waits for the next read"
            );

            // The second chunk closes the burst; it carries no key.
            let second = drain_vt(&mut decoder, &mut pending, b"1~", false);
            assert!(second.is_empty(), "the closing marker carries no key");
            assert!(pending.is_empty(), "the whole marker was consumed");

            // A \r after the burst is the enter key again.
            let after = drain_vt(&mut decoder, &mut pending, b"\r", false);
            assert_eq!(
                after,
                vec![Event::Key(KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::NONE
                ))]
            );
        }

        /// The raw input mode keeps `ENABLE_LINE_INPUT` off (so `ReadFile`
        /// returns as soon as bytes are available) and adds only the VT flag
        /// on the first attempt.
        #[test]
        fn the_raw_input_mode_keeps_line_input_off() {
            // Every bit on except the VT flag: the fallback must leave a bit
            // the caller did not have off, and the first attempt must add it.
            let all = !ENABLE_VIRTUAL_TERMINAL_INPUT;
            let raw = raw_input_mode(all, false);
            assert_eq!(raw & ENABLE_LINE_INPUT, 0, "line input must stay off");
            assert_eq!(raw & ENABLE_ECHO_INPUT, 0, "echo must stay off");
            assert_eq!(
                raw & ENABLE_PROCESSED_INPUT,
                0,
                "processed input must stay off"
            );
            assert_ne!(
                raw & ENABLE_WINDOW_INPUT,
                0,
                "window input is kept for key records"
            );
            assert_eq!(
                raw & ENABLE_VIRTUAL_TERMINAL_INPUT,
                0,
                "the fallback omits the VT flag"
            );

            let vt = raw_input_mode(all, true);
            assert_eq!(vt, raw | 0x0200, "the first attempt adds the VT flag");
            assert_eq!(ENABLE_VIRTUAL_TERMINAL_INPUT, 0x0200);

            // A mode that already carried the VT flag is still put on the
            // key-record path when the first attempt is refused.
            assert_eq!(
                raw_input_mode(u32::MAX, false) & ENABLE_VIRTUAL_TERMINAL_INPUT,
                0,
                "the fallback clears the VT flag too"
            );
        }

        /// A console that refuses the VT flag still uses key records, and no
        /// error is raised; only a refusal of the plain raw mode is an error.
        #[test]
        fn a_refused_vt_flag_falls_back_to_key_records() {
            assert_eq!(input_path(true, true), Some(InputPath::VtBytes));
            assert_eq!(input_path(false, true), Some(InputPath::KeyRecords));
            assert_eq!(input_path(false, false), None);
            // (true, false) cannot happen in the reader, and the VT path
            // still wins when the first attempt succeeded.
            assert_eq!(input_path(true, false), Some(InputPath::VtBytes));
        }

        /// The VT byte path starts off and is reset, so a stale "inside a
        /// paste" decoder cannot leak into a new session.
        #[test]
        fn the_vt_path_is_reset_when_raw_mode_is_disabled() {
            assert!(!VT_INPUT.load(Ordering::Relaxed), "VT input starts off");
            // A session on the byte path leaves the flag set, a half-read
            // marker pending, and the decoder inside a paste.
            VT_INPUT.store(true, Ordering::Relaxed);
            PENDING
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .extend_from_slice(b"\x1b[20");
            let mut inside = Decoder::new();
            assert_eq!(inside.decode(b"\x1b[200~", false), Decoded::Unknown(6));
            *DECODER.lock().unwrap_or_else(|e| e.into_inner()) = inside;

            reset_vt_state();

            assert!(!VT_INPUT.load(Ordering::Relaxed), "disable resets the flag");
            assert!(
                PENDING.lock().unwrap_or_else(|e| e.into_inner()).is_empty(),
                "disable drops the half-read marker"
            );
            let mut decoder = DECODER.lock().unwrap_or_else(|e| e.into_inner());
            assert_eq!(
                decoder.decode(b"\r", false),
                Decoded::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 1),
                "a fresh decoder is not inside a paste"
            );
        }
    }
}
