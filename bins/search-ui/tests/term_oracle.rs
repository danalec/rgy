// SPDX-License-Identifier: Apache-2.0

//! The oracle: the tree's own terminal, held to the crate it replaced.
//!
//! `crossterm` is a dev-dependency of this crate: it is in the tree, and
//! it is not in the binary. This file is what it is there for — the
//! in-house backend writes the bytes, and the oracle says which bytes
//! those are.
//!
//! Two things are held to it:
//!
//!   * the **escape sequences** — the cursor, the screen, the colours and
//!     the text — byte for byte, against the bytes the oracle's own
//!     commands write;
//!   * the **keys** — a corpus of the bytes a terminal sends, in the
//!     oracle's own vocabulary of keys.
//!
//! The oracle asks the console whether it can read the escape sequences and
//! falls back to the platform's own calls when it cannot. The bytes do not
//! depend on the console — the fallback is another way of doing the same
//! thing — so the environment is told that the terminal is one, and the
//! oracle is asked for the bytes. The answer is then the same on every
//! host, and the comparison is a comparison.

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{
    KeyCode as OracleKeyCode, KeyEvent as OracleKeyEvent, KeyEventKind as OracleKeyEventKind,
    KeyModifiers as OracleKeyModifiers,
};
use crossterm::style::{
    Color as OracleColour, PrintStyledContent, ResetColor, SetBackgroundColor, SetForegroundColor,
};
use crossterm::terminal::{Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};

use rgy_search_ui::term::{
    self, Color, Decoded, Decoder, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, Paint, Screen,
};

/// The oracle writes the escape sequences when the console can read them.
///
/// crossterm asks the console first, and falls back to the platform's own
/// calls when the console cannot — the fallback writes no bytes at all, so
/// the oracle has no answer to compare on such a host. The bytes do not
/// depend on the console, so the environment is told that the terminal is
/// one.
fn the_terminal_is_one() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // SAFETY: the variables are set once, to one value, before any test
        // in this binary reaches the oracle: every test calls this first,
        // and the `Once` makes the writes happen before the first read.
        unsafe {
            std::env::set_var("TERM", "xterm-256color");
            // The oracle answers whether to write a colour **at write
            // time**, and `NO_COLOR` is its answer to a question that is
            // not this test's: this test pins the encoding of a colour,
            // and the tree's own answer to `NO_COLOR` is the palette's
            // (`palette::no_colour`), made once and carried by the colours
            // the frame is drawn in. Measured on 2026-11-04 in a shell
            // with `NO_COLOR=1`: the tree wrote `\x1b[39m` and the oracle
            // `\x1b[m`, and the test called the encoding wrong.
            std::env::remove_var("NO_COLOR");
        }
    });
}

/// The bytes an oracle command writes.
fn oracle_bytes(command: impl crossterm::Command) -> String {
    let mut bytes = String::new();
    command
        .write_ansi(&mut bytes)
        .expect("a string cannot fail to be written");
    bytes
}

/// The bytes the in-house backend writes.
fn written(write: impl FnOnce(&mut Screen<&mut Vec<u8>>) -> std::io::Result<()>) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    {
        let mut screen = Screen::new(&mut out);
        write(&mut screen).expect("writing to a vector cannot fail");
    }
    out
}

/// The oracle's colour for the tree's colour.
fn oracle_colour(colour: Color) -> OracleColour {
    match colour {
        Color::Reset => OracleColour::Reset,
        Color::Black => OracleColour::Black,
        Color::DarkGrey => OracleColour::DarkGrey,
        Color::Red => OracleColour::Red,
        Color::DarkRed => OracleColour::DarkRed,
        Color::Green => OracleColour::Green,
        Color::DarkGreen => OracleColour::DarkGreen,
        Color::Yellow => OracleColour::Yellow,
        Color::DarkYellow => OracleColour::DarkYellow,
        Color::Blue => OracleColour::Blue,
        Color::DarkBlue => OracleColour::DarkBlue,
        Color::Magenta => OracleColour::Magenta,
        Color::DarkMagenta => OracleColour::DarkMagenta,
        Color::Cyan => OracleColour::Cyan,
        Color::DarkCyan => OracleColour::DarkCyan,
        Color::White => OracleColour::White,
        Color::Grey => OracleColour::Grey,
        Color::Rgb { r, g, b } => OracleColour::Rgb { r, g, b },
    }
}

/// Every colour the tree knows.
fn colours() -> Vec<Color> {
    vec![
        Color::Reset,
        Color::Black,
        Color::DarkGrey,
        Color::Red,
        Color::DarkRed,
        Color::Green,
        Color::DarkGreen,
        Color::Yellow,
        Color::DarkYellow,
        Color::Blue,
        Color::DarkBlue,
        Color::Magenta,
        Color::DarkMagenta,
        Color::Cyan,
        Color::DarkCyan,
        Color::White,
        Color::Grey,
        Color::Rgb {
            r: 19,
            g: 18,
            b: 21,
        },
        Color::Rgb {
            r: 255,
            g: 255,
            b: 255,
        },
    ]
}

/// The cursor move is the oracle's bytes.
#[test]
fn the_cursor_move_is_the_oracles_bytes() {
    the_terminal_is_one();
    // The cell the cursor moves to is counted from zero on the way in and
    // from one on the wire, so the last cell of the last line cannot be
    // named — the oracle's own arithmetic overflows there too.
    for (x, y) in [(0u16, 0u16), (0, 1), (3, 7), (79, 24), (65534, 65534)] {
        let mine = written(|screen| screen.move_to(x, y));
        let oracle = oracle_bytes(MoveTo(x, y));
        assert_eq!(
            String::from_utf8_lossy(&mine),
            oracle,
            "the move to ({x}, {y}) is not the oracle's bytes"
        );
    }
}

/// The clears are the oracle's bytes.
#[test]
fn the_clears_are_the_oracles_bytes() {
    the_terminal_is_one();
    let all = written(|screen| screen.clear_all());
    assert_eq!(
        String::from_utf8_lossy(&all),
        oracle_bytes(Clear(ClearType::All))
    );
    let line = written(|screen| screen.clear_line());
    assert_eq!(
        String::from_utf8_lossy(&line),
        oracle_bytes(Clear(ClearType::CurrentLine))
    );
}

/// The cursor's visibility is the oracle's bytes.
#[test]
fn the_cursors_visibility_is_the_oracles_bytes() {
    the_terminal_is_one();
    let hidden = written(|screen| screen.hide());
    assert_eq!(String::from_utf8_lossy(&hidden), oracle_bytes(Hide));
    let shown = written(|screen| screen.show());
    assert_eq!(String::from_utf8_lossy(&shown), oracle_bytes(Show));
}

/// The alternate screen is the oracle's bytes.
#[test]
fn the_alternate_screen_is_the_oracles_bytes() {
    the_terminal_is_one();
    let entered = written(|screen| screen.enter_alternate_screen());
    assert_eq!(
        String::from_utf8_lossy(&entered),
        oracle_bytes(EnterAlternateScreen)
    );
    let left = written(|screen| screen.leave_alternate_screen());
    assert_eq!(
        String::from_utf8_lossy(&left),
        oracle_bytes(LeaveAlternateScreen)
    );
}

/// The colours are the oracle's bytes.
#[test]
fn the_colours_are_the_oracles_bytes() {
    the_terminal_is_one();
    for colour in colours() {
        let mine = written(|screen| screen.foreground(colour));
        let oracle = oracle_bytes(SetForegroundColor(oracle_colour(colour)));
        assert_eq!(
            String::from_utf8_lossy(&mine),
            oracle,
            "the foreground {colour:?} is not the oracle's bytes"
        );

        let mine = written(|screen| screen.background(colour));
        let oracle = oracle_bytes(SetBackgroundColor(oracle_colour(colour)));
        assert_eq!(
            String::from_utf8_lossy(&mine),
            oracle,
            "the background {colour:?} is not the oracle's bytes"
        );
    }
}

/// The reset is the oracle's bytes.
#[test]
fn the_reset_is_the_oracles_bytes() {
    the_terminal_is_one();
    let mine = written(|screen| screen.reset_colour());
    assert_eq!(String::from_utf8_lossy(&mine), oracle_bytes(ResetColor));
}

/// The attributes are the oracle's bytes — through the tree's own code.
///
/// `Screen::print` writes whatever bytes it is handed, so feeding it a
/// literal `\x1b[1m` and comparing with `SetAttribute` proved nothing
/// about `Styled`: a regression in its bold/dim/reverse escapes or its
/// `\x1b[0m` reset could not make this test fail. The comparison now goes
/// through `Paint` and `Styled`'s `Display`, where the bytes are actually
/// written, and includes reverse — the attribute the selected row uses —
/// and the reset that undoes each one.
#[test]
fn the_attributes_are_the_oracles_bytes() {
    the_terminal_is_one();
    let bold_oracle = oracle_bytes(PrintStyledContent(crossterm::style::Stylize::bold(
        "zen-rs".to_string(),
    )));
    let dim_oracle = oracle_bytes(PrintStyledContent(crossterm::style::Stylize::dim(
        "zen-rs".to_string(),
    )));
    let reverse_oracle = oracle_bytes(PrintStyledContent(crossterm::style::Stylize::reverse(
        "zen-rs".to_string(),
    )));
    assert_eq!("zen-rs".bold().to_string(), bold_oracle);
    assert_eq!("zen-rs".dim().to_string(), dim_oracle);
    assert_eq!("zen-rs".reverse().to_string(), reverse_oracle);
    // The reset is part of the answer: an attribute that is never undone
    // would leave the rest of the frame in the wrong style.
    assert!(
        "zen-rs".reverse().to_string().ends_with("\x1b[0m"),
        "the reverse span does not reset the attributes"
    );
}

/// The text is the oracle's bytes.
#[test]
fn the_text_is_the_oracles_bytes() {
    the_terminal_is_one();
    for text in ["", "zen-rs", "❯ query", ">copy", "a path with a space"] {
        let mine = written(|screen| screen.print(text));
        let oracle = oracle_bytes(crossterm::style::Print(text));
        assert_eq!(
            String::from_utf8_lossy(&mine),
            oracle,
            "the text {text:?} is not the oracle's bytes"
        );
    }
}

/// A styled span is the oracle's bytes.
///
/// The span is the composition the oracle makes: the colour, the
/// attributes, the text, and the reset that undoes them. Reverse is held
/// to the oracle here too: `result_cell` draws the selected row with it,
/// and a span with all three attributes proves the SGR order and the
/// single reset at the end.
#[test]
fn a_styled_span_is_the_oracles_bytes() {
    the_terminal_is_one();
    let combinations = [
        (false, false, false),
        (true, false, false),
        (false, true, false),
        (false, false, true),
        (true, true, true),
    ];
    for colour in colours() {
        for (bold, dim, reverse) in combinations {
            let mut mine = "zen-rs".with(colour);
            if bold {
                mine = mine.bold();
            }
            if dim {
                mine = mine.dim();
            }
            if reverse {
                mine = mine.reverse();
            }
            let mine = mine.to_string();
            let oracle = oracle_bytes(PrintStyledContent(oracle_span(
                "zen-rs", colour, bold, dim, reverse,
            )));
            assert_eq!(
                mine, oracle,
                "the span {colour:?} (bold: {bold}, dim: {dim}, reverse: {reverse}) is not the oracle's bytes"
            );
        }
    }
}

/// The oracle's styled span, in the oracle's own vocabulary.
///
/// The trait is named in full: the tree has a trait of its own with the
/// same name, and the two cannot be in the same scope.
fn oracle_span(
    text: &str,
    colour: Color,
    bold: bool,
    dim: bool,
    reverse: bool,
) -> crossterm::style::StyledContent<String> {
    let styled = crossterm::style::Stylize::with(text.to_string(), oracle_colour(colour));
    let styled = if bold {
        crossterm::style::Stylize::bold(styled)
    } else {
        styled
    };
    let styled = if dim {
        crossterm::style::Stylize::dim(styled)
    } else {
        styled
    };
    if reverse {
        crossterm::style::Stylize::reverse(styled)
    } else {
        styled
    }
}

/// The oracle's key, for the tree's key.
fn oracle_key(event: &KeyEvent) -> (OracleKeyCode, OracleKeyModifiers, OracleKeyEventKind) {
    let code = match event.code {
        KeyCode::Backspace => OracleKeyCode::Backspace,
        KeyCode::Enter => OracleKeyCode::Enter,
        KeyCode::Left => OracleKeyCode::Left,
        KeyCode::Right => OracleKeyCode::Right,
        KeyCode::Up => OracleKeyCode::Up,
        KeyCode::Down => OracleKeyCode::Down,
        KeyCode::Home => OracleKeyCode::Home,
        KeyCode::End => OracleKeyCode::End,
        KeyCode::PageUp => OracleKeyCode::PageUp,
        KeyCode::PageDown => OracleKeyCode::PageDown,
        KeyCode::Delete => OracleKeyCode::Delete,
        KeyCode::Esc => OracleKeyCode::Esc,
        KeyCode::Tab => OracleKeyCode::Tab,
        KeyCode::Char(c) => OracleKeyCode::Char(c),
    };
    let mut modifiers = OracleKeyModifiers::NONE;
    if event.modifiers.contains(KeyModifiers::SHIFT) {
        modifiers |= OracleKeyModifiers::SHIFT;
    }
    if event.modifiers.contains(KeyModifiers::CONTROL) {
        modifiers |= OracleKeyModifiers::CONTROL;
    }
    if event.modifiers.contains(KeyModifiers::ALT) {
        modifiers |= OracleKeyModifiers::ALT;
    }
    let kind = match event.kind {
        KeyEventKind::Press => OracleKeyEventKind::Press,
        KeyEventKind::Repeat => OracleKeyEventKind::Repeat,
        KeyEventKind::Release => OracleKeyEventKind::Release,
    };
    (code, modifiers, kind)
}

/// One entry of the corpus: the bytes a terminal sends, and the key the
/// oracle calls them.
struct Case {
    bytes: &'static [u8],
    key: OracleKeyEvent,
}

/// The corpus: the bytes a terminal sends, and the key the oracle calls
/// them.
///
/// The keys are the ones the interface acts on, and the bytes are the ones
/// a terminal sends for them.
fn corpus() -> Vec<Case> {
    let plain = |code| OracleKeyEvent::new(code, OracleKeyModifiers::NONE);
    let control = |code| OracleKeyEvent::new(code, OracleKeyModifiers::CONTROL);
    vec![
        // The characters a user types.
        Case {
            bytes: b"a",
            key: plain(OracleKeyCode::Char('a')),
        },
        Case {
            bytes: b"z",
            key: plain(OracleKeyCode::Char('z')),
        },
        Case {
            bytes: b">",
            key: plain(OracleKeyCode::Char('>')),
        },
        Case {
            bytes: b" ",
            key: plain(OracleKeyCode::Char(' ')),
        },
        // The shift key is in the character itself.
        Case {
            bytes: b"A",
            key: OracleKeyEvent::new(OracleKeyCode::Char('A'), OracleKeyModifiers::SHIFT),
        },
        // A character that is not one byte. The terminal sends one
        // character at a time.
        Case {
            bytes: "❯".as_bytes(),
            key: plain(OracleKeyCode::Char('❯')),
        },
        Case {
            bytes: "ç".as_bytes(),
            key: plain(OracleKeyCode::Char('ç')),
        },
        Case {
            bytes: "ã".as_bytes(),
            key: plain(OracleKeyCode::Char('ã')),
        },
        // The control key: the console answers with a control code.
        Case {
            bytes: b"\x01",
            key: control(OracleKeyCode::Char('a')),
        },
        Case {
            bytes: b"\x03",
            key: control(OracleKeyCode::Char('c')),
        },
        Case {
            bytes: b"\x0e",
            key: control(OracleKeyCode::Char('n')),
        },
        Case {
            bytes: b"\x15",
            key: control(OracleKeyCode::Char('u')),
        },
        Case {
            bytes: b"\x17",
            key: control(OracleKeyCode::Char('w')),
        },
        // The keys that have a name of their own.
        Case {
            bytes: b"\r",
            key: plain(OracleKeyCode::Enter),
        },
        // The line feed is the control key with the letter J: the terminal
        // is in the raw mode, where the enter key sends a carriage return.
        Case {
            bytes: b"\n",
            key: control(OracleKeyCode::Char('j')),
        },
        Case {
            bytes: b"\t",
            key: plain(OracleKeyCode::Tab),
        },
        Case {
            bytes: b"\x7f",
            key: plain(OracleKeyCode::Backspace),
        },
        // The cursor keys, in the two spellings a terminal sends them.
        Case {
            bytes: b"\x1b[A",
            key: plain(OracleKeyCode::Up),
        },
        Case {
            bytes: b"\x1b[B",
            key: plain(OracleKeyCode::Down),
        },
        Case {
            bytes: b"\x1b[C",
            key: plain(OracleKeyCode::Right),
        },
        Case {
            bytes: b"\x1b[D",
            key: plain(OracleKeyCode::Left),
        },
        Case {
            bytes: b"\x1bOA",
            key: plain(OracleKeyCode::Up),
        },
        Case {
            bytes: b"\x1bOD",
            key: plain(OracleKeyCode::Left),
        },
        // The ends of the line, in the two spellings.
        Case {
            bytes: b"\x1b[H",
            key: plain(OracleKeyCode::Home),
        },
        Case {
            bytes: b"\x1b[F",
            key: plain(OracleKeyCode::End),
        },
        Case {
            bytes: b"\x1b[1~",
            key: plain(OracleKeyCode::Home),
        },
        Case {
            bytes: b"\x1b[4~",
            key: plain(OracleKeyCode::End),
        },
        Case {
            bytes: b"\x1bOH",
            key: plain(OracleKeyCode::Home),
        },
        Case {
            bytes: b"\x1bOF",
            key: plain(OracleKeyCode::End),
        },
        // The keys the numeric pad names.
        Case {
            bytes: b"\x1b[3~",
            key: plain(OracleKeyCode::Delete),
        },
        Case {
            bytes: b"\x1b[5~",
            key: plain(OracleKeyCode::PageUp),
        },
        Case {
            bytes: b"\x1b[6~",
            key: plain(OracleKeyCode::PageDown),
        },
        // The control key with a key that has a name.
        Case {
            bytes: b"\x1b[1;5A",
            key: control(OracleKeyCode::Up),
        },
        Case {
            bytes: b"\x1b[1;5C",
            key: control(OracleKeyCode::Right),
        },
        Case {
            bytes: b"\x1b[3;5~",
            key: control(OracleKeyCode::Delete),
        },
        // The alt key in front of a character.
        Case {
            bytes: b"\x1bx",
            key: OracleKeyEvent::new(OracleKeyCode::Char('x'), OracleKeyModifiers::ALT),
        },
        // The escape key on its own.
        Case {
            bytes: b"\x1b",
            key: plain(OracleKeyCode::Esc),
        },
    ]
}

/// The corpus of bytes is the oracle's corpus of keys.
#[test]
fn the_bytes_are_the_oracles_keys() {
    for case in corpus() {
        let decoded = term::decode(case.bytes, false);
        let Decoded::Key(event, len) = decoded else {
            panic!(
                "the bytes {:?} are not a key, but the oracle calls them {:?}",
                case.bytes, case.key
            );
        };
        assert_eq!(
            len,
            case.bytes.len(),
            "the key of {:?} took the wrong number of bytes",
            case.bytes
        );
        assert_eq!(
            oracle_key(&event),
            (case.key.code, case.key.modifiers, case.key.kind),
            "the bytes {:?} are not the key the oracle calls them",
            case.bytes
        );
    }
}

/// A sequence that is not whole yet is not a key.
///
/// The escape key and the beginning of a sequence look the same, so the
/// decoder is told whether more is coming.
#[test]
fn a_sequence_that_is_not_whole_is_not_a_key() {
    assert_eq!(term::decode(b"\x1b", true), Decoded::Incomplete);
    assert_eq!(term::decode(b"\x1b[", true), Decoded::Incomplete);
    assert_eq!(term::decode(b"\x1b[1;", true), Decoded::Incomplete);
    assert_eq!(term::decode(b"", false), Decoded::Incomplete);
    // And the whole sequence is a key.
    assert!(matches!(
        term::decode(b"\x1b[1;5A", true),
        Decoded::Key(_, _)
    ));
}

/// A cut-off multibyte character is dropped at the end of a run.
///
/// `more_may_come == false` says the run is over. A lone lead byte is not
/// the beginning of a character then, and answering `Incomplete` would make
/// `imp::read()` block on a byte that will never arrive — a paste cut in
/// two would freeze the interface. When more may come, the same bytes are
/// still incomplete, and the whole character is still a key.
#[test]
fn a_cut_off_character_is_dropped_at_the_end_of_a_run() {
    // 0xc3 starts the two-byte 'é'; 0xe2 0x82 starts the three-byte '€'.
    assert_eq!(term::decode(b"\xc3", false), Decoded::Unknown(1));
    assert_eq!(term::decode(b"\xc3", true), Decoded::Incomplete);
    assert_eq!(term::decode(b"\xe2\x82", false), Decoded::Unknown(2));
    assert_eq!(term::decode(b"\xe2\x82", true), Decoded::Incomplete);
    assert!(matches!(
        term::decode("é".as_bytes(), false),
        Decoded::Key(_, 2)
    ));
}

/// A sequence the program does not know is dropped; a bracketed paste,
/// which it does know, is delivered as literal text.
///
/// A `\r` inside a paste is a character for the query field, not the enter
/// key that would choose a path and end the session. The old assertion here
/// held the opening marker `ESC [ 200 ~` to `Unknown(6)`; it now holds the
/// whole burst to the characters it delivers.
#[test]
fn a_bracketed_paste_is_delivered_as_text() {
    // A sequence that is genuinely unknown is still dropped, not waited for.
    assert_eq!(term::decode(b"\x1b[999~", false), Decoded::Unknown(6));
    assert_eq!(term::decode(b"\x1b[999~", true), Decoded::Unknown(6));

    // A whole paste burst in one run: the markers carry no key, and "a\rb"
    // is three literal characters — the \r is a character, never Enter.
    let keys = pasted_keys(b"\x1b[200~a\rb\x1b[201~");
    let codes: Vec<KeyCode> = keys.iter().map(|key| key.code).collect();
    assert_eq!(
        codes,
        vec![KeyCode::Char('a'), KeyCode::Char('\r'), KeyCode::Char('b')],
        "a paste is literal text, not keys"
    );
    assert!(
        !codes.contains(&KeyCode::Enter),
        "a carriage return inside a paste must not be the enter key"
    );

    // Once the burst is closed, a \r is the enter key again: the paste state
    // does not leak past the closing marker.
    let mut decoder = Decoder::new();
    assert_eq!(
        decoder.decode(b"\r", false),
        Decoded::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 1)
    );
}

/// A paste cut across two reads still delivers its text.
///
/// The decoder carries the "inside a paste" bit between runs, and the
/// `Incomplete` answer makes the reader wait for the rest of a marker.
#[test]
fn a_paste_split_across_reads_still_delivers_its_text() {
    let mut decoder = Decoder::new();
    let mut buffer: Vec<u8> = Vec::new();
    // The first read ends in the middle of the closing marker.
    buffer.extend_from_slice(b"\x1b[200~a\rb\x1b[20");
    let first = drain(&mut decoder, &mut buffer, true);
    let codes: Vec<KeyCode> = first.iter().map(|key| key.code).collect();
    assert_eq!(
        codes,
        vec![KeyCode::Char('a'), KeyCode::Char('\r'), KeyCode::Char('b')]
    );
    assert_eq!(
        buffer.as_slice(),
        &b"\x1b[20"[..],
        "the unfinished marker waits for more"
    );

    // The second read finishes the marker and closes the burst.
    buffer.extend_from_slice(b"1~");
    let second = drain(&mut decoder, &mut buffer, false);
    assert!(second.is_empty(), "the closing marker carries no key");
    assert!(buffer.is_empty(), "the whole marker was consumed");

    // A \r after the burst is the enter key again.
    assert_eq!(
        decoder.decode(b"\r", false),
        Decoded::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 1)
    );
}

/// Drain a run of bytes through one decoder the way `imp::read()` does,
/// returning the keys and leaving any unfinished bytes in the buffer.
fn drain(decoder: &mut Decoder, buffer: &mut Vec<u8>, more_may_come: bool) -> Vec<KeyEvent> {
    let mut keys = Vec::new();
    loop {
        if buffer.is_empty() {
            break;
        }
        let decoded = decoder.decode(buffer.as_slice(), more_may_come);
        match decoded {
            Decoded::Key(event, len) => {
                keys.push(event);
                buffer.drain(..len);
            }
            Decoded::Unknown(len) => {
                buffer.drain(..len);
            }
            Decoded::Incomplete => break,
        }
    }
    keys
}

/// The keys one whole run of bytes yields, drained through a fresh decoder.
fn pasted_keys(bytes: &[u8]) -> Vec<KeyEvent> {
    let mut decoder = Decoder::new();
    let mut buffer = bytes.to_vec();
    drain(&mut decoder, &mut buffer, false)
}

/// Enabling and disabling bracketed paste are the terminal's own escapes.
#[test]
fn bracketed_paste_is_switched_on_and_off() {
    assert_eq!(
        written(|screen| screen.enable_bracketed_paste()),
        b"\x1b[?2004h".to_vec()
    );
    assert_eq!(
        written(|screen| screen.disable_bracketed_paste()),
        b"\x1b[?2004l".to_vec()
    );
}
