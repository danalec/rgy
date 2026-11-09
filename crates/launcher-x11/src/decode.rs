// SPDX-License-Identifier: Apache-2.0

//! A keysym is a key: the byte-level decision.
//!
//! X11 reports a **KeySym** — a number the server's keyboard map chose —
//! and a modifier mask. The window driver knows a [`Key`], not a keysym,
//! and the translation is a decision about numbers. It is here, un-gated
//! by `target_os`, so the host that cannot open an X display can still
//! prove that Ctrl+V is a paste and that a Portuguese `ç` is a character.
//!
//! Two rules are worth saying: the control letters are read **before**
//! the characters (a Ctrl+C is a copy, not a `c` in the field), and a
//! keysym that names no character answers `None` — a modifier on its own
//! is not a key the field sees.

use rgy_launcher::TapKey;

use crate::keys as x;
use crate::types::Key;

/// The Control bit of the X11 modifier mask.
pub fn ctrl_of(state: u32) -> bool {
    state & x::ControlMask != 0
}

/// Whether the Shift bit of the mask is set.
pub fn shift_of(state: u32) -> bool {
    state & x::ShiftMask != 0
}

/// The modifier a keysym belongs to, if it is a modifier key at all.
///
/// X11 reports one idea three ways — the generic key and a left and a
/// right one — and the double-tap hotkey answers to whichever of them
/// the user's hand found.
pub fn modifier_of(keysym: u64) -> Option<TapKey> {
    match keysym {
        x::XK_Control_L | x::XK_Control_R => Some(TapKey::Ctrl),
        x::XK_Alt_L | x::XK_Alt_R => Some(TapKey::Alt),
        x::XK_Shift_L | x::XK_Shift_R => Some(TapKey::Shift),
        _ => None,
    }
}

/// A keysym that names a character, as that character.
///
/// Latin-1 keysyms are the byte itself; X11's Unicode keysyms carry the
/// codepoint above `0x0100_0000`. Anything else — a function key, a
/// modifier, a dead key — names no character.
pub fn character_of(keysym: u64) -> Option<char> {
    if (0x20..=0x7e).contains(&keysym) || (0xa0..=0xff).contains(&keysym) {
        return char::from_u32(keysym as u32);
    }
    if (0x0100_0000..=0x0110_ffff).contains(&keysym) {
        return char::from_u32((keysym - 0x0100_0000) as u32);
    }
    None
}

/// A printable key, with the modifiers Xlib has already applied.
///
/// `XLookupKeysym(event, 0)` is always the **unshifted** level, so
/// `Shift+.` reads as `XK_period` there and `>` never reaches the field —
/// which made every `>` command (settings, menu, preview) unreachable on
/// X11. `XLookupString` applies Shift, Caps Lock and AltGr and returns the
/// actual bytes; this is the pure decision that prefers those bytes over
/// the bare keysym, so it can be tested on a host with no display.
///
/// `text` is empty for a key that spells nothing (Return, an arrow, a
/// modifier), and those fall back to [`key_of`]. Control and Alt keep the
/// keysym path: a Ctrl+C is a command, not a control character, and an
/// Alt+letter belongs to the window manager.
pub fn typed_key(keysym: u64, ctrl: bool, alt: bool, text: &[u8]) -> Option<Key> {
    if let (false, false, Some(&byte)) = (ctrl, alt, text.first()) {
        // `XLookupString` hands back Latin-1 bytes. Latin-1 is the
        // first 256 Unicode codepoints, so the byte *is* the char.
        // A control byte (below the space, or DEL) is not text at all.
        if (0x20..=0x7e).contains(&byte) || byte >= 0xa0 {
            return char::from_u32(byte as u32).map(Key::Char);
        }
    }
    key_of(keysym, ctrl)
}

/// Turn a keysym and a modifier mask into the launcher's own key.
///
/// `None` says the event is not a key the field sees: a modifier press, a
/// function key, or a dead key. The pump drops those rather than asking
/// the daemon a question nobody asked.
pub fn key_of(keysym: u64, ctrl: bool) -> Option<Key> {
    Some(match keysym {
        x::XK_BackSpace => Key::Backspace,
        x::XK_Delete => Key::Delete,
        x::XK_Left => Key::Left,
        x::XK_Right => Key::Right,
        x::XK_Up => Key::Up,
        x::XK_Down => Key::Down,
        x::XK_Home => Key::Home,
        x::XK_End => Key::End,
        x::XK_Page_Up => Key::PageUp,
        x::XK_Page_Down => Key::PageDown,
        x::XK_Return => {
            if ctrl {
                Key::CtrlEnter
            } else {
                Key::Enter
            }
        }
        x::XK_Escape => Key::Esc,
        x::XK_Tab => Key::Tab,
        // A modifier is not a key, and a function key is not either.
        x::XK_Control_L
        | x::XK_Control_R
        | x::XK_Alt_L
        | x::XK_Alt_R
        | x::XK_Shift_L
        | x::XK_Shift_R
        | x::XK_Super_L
        | x::XK_Super_R => return None,
        _ if ctrl => control_letter(keysym)?,
        _ => Key::Char(character_of(keysym)?),
    })
}

/// The control letters the launcher owns, and only those.
fn control_letter(keysym: u64) -> Option<Key> {
    // The keyboard map may hand the upper case when Shift is held with
    // Control; both spellings are the same key.
    let letter = match keysym {
        k if k == u64::from(b'c') || k == u64::from(b'C') => Key::CtrlC,
        k if k == u64::from(b'v') || k == u64::from(b'V') => Key::CtrlV,
        k if k == u64::from(b'p') || k == u64::from(b'P') => Key::CtrlP,
        k if k == u64::from(b'k') || k == u64::from(b'K') => Key::CtrlK,
        k if k == u64::from(b'w') || k == u64::from(b'W') => Key::CtrlW,
        k if k == u64::from(b'u') || k == u64::from(b'U') => Key::CtrlU,
        _ => return None,
    };
    Some(letter)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Shift+.` is `>`: the unshifted keysym says `.`, but Xlib's own
    /// string says `>` and that is what a `>` command needs.
    #[test]
    fn a_shifted_period_is_the_command_prefix() {
        assert_eq!(
            typed_key(u64::from(b'.'), false, false, b">"),
            Some(Key::Char('>'))
        );
    }

    /// Shift on a letter is still a letter, and a plain key with no text
    /// keeps the keysym's answer.
    #[test]
    fn shifted_letters_and_plain_keys_keep_their_spelling() {
        assert_eq!(
            typed_key(u64::from(b'a'), false, false, b"A"),
            Some(Key::Char('A'))
        );
        assert_eq!(
            typed_key(u64::from(b'r'), false, false, b"r"),
            Some(Key::Char('r'))
        );
        assert_eq!(
            typed_key(u64::from(b'r'), false, false, b""),
            Some(Key::Char('r'))
        );
    }

    /// A Ctrl combination is a command even though XLookupString would
    /// hand back a control byte; Alt belongs to the window manager.
    #[test]
    fn control_and_alt_keep_the_keysym_path() {
        assert_eq!(
            typed_key(u64::from(b'c'), true, false, &[0x03]),
            Some(Key::CtrlC)
        );
        assert_eq!(
            typed_key(u64::from(b'x'), false, true, b"x"),
            Some(Key::Char('x'))
        );
    }

    /// Return and the arrows spell no text, so the keysym decides.
    #[test]
    fn a_key_that_spells_nothing_falls_back_to_the_keysym() {
        assert_eq!(typed_key(x::XK_Return, false, false, b""), Some(Key::Enter));
        assert_eq!(typed_key(x::XK_Left, false, false, b""), Some(Key::Left));
        assert_eq!(
            typed_key(x::XK_BackSpace, false, false, b""),
            Some(Key::Backspace)
        );
    }

    /// A Latin-1 byte is a character, not a control byte.
    #[test]
    fn a_latin1_letter_survives_the_lookup() {
        assert_eq!(typed_key(0xe7, false, false, &[0xe7]), Some(Key::Char('ç')));
    }
    /// The arrows, Home, End and the pages are the model's own keys.
    #[test]
    fn the_navigation_keys_are_the_models_keys() {
        for (sym, key) in [
            (x::XK_Left, Key::Left),
            (x::XK_Right, Key::Right),
            (x::XK_Up, Key::Up),
            (x::XK_Down, Key::Down),
            (x::XK_Home, Key::Home),
            (x::XK_End, Key::End),
            (x::XK_Page_Up, Key::PageUp),
            (x::XK_Page_Down, Key::PageDown),
        ] {
            assert_eq!(key_of(sym, false), Some(key));
        }
    }

    /// Enter is an open; Ctrl+Enter opens the folder.
    #[test]
    fn ctrl_enter_is_not_enter() {
        assert_eq!(key_of(x::XK_Return, false), Some(Key::Enter));
        assert_eq!(key_of(x::XK_Return, true), Some(Key::CtrlEnter));
    }

    /// The control letters are actions, not characters.
    #[test]
    fn the_control_letters_are_actions_not_characters() {
        for (c, key) in [
            (b'c', Key::CtrlC),
            (b'v', Key::CtrlV),
            (b'p', Key::CtrlP),
            (b'k', Key::CtrlK),
            (b'w', Key::CtrlW),
            (b'u', Key::CtrlU),
        ] {
            assert_eq!(key_of(c as u64, true), Some(key));
            assert_eq!(key_of(c.to_ascii_uppercase() as u64, true), Some(key));
        }
        assert_eq!(key_of(b'z' as u64, true), None);
    }

    /// The same letter without Control is a character in the field.
    #[test]
    fn a_letter_without_control_is_a_character() {
        assert_eq!(key_of(b'v' as u64, false), Some(Key::Char('v')));
        assert_eq!(key_of(b'C' as u64, false), Some(Key::Char('C')));
    }

    /// A modifier on its own is not a key the field sees.
    #[test]
    fn a_modifier_is_not_a_key() {
        assert_eq!(key_of(x::XK_Control_L, false), None);
        assert_eq!(key_of(x::XK_Shift_R, true), None);
        assert_eq!(modifier_of(x::XK_Control_R), Some(TapKey::Ctrl));
        assert_eq!(modifier_of(x::XK_Alt_L), Some(TapKey::Alt));
        assert_eq!(modifier_of(x::XK_Shift_R), Some(TapKey::Shift));
        assert_eq!(modifier_of(b'a' as u64), None);
    }

    /// The languages the launcher speaks are Latin-1: `ç`, `ã`, `é`.
    #[test]
    fn latin1_letters_are_characters() {
        assert_eq!(key_of(0xe7, false), Some(Key::Char('ç')));
        assert_eq!(key_of(0xe3, false), Some(Key::Char('ã')));
        assert_eq!(key_of(0xe9, false), Some(Key::Char('é')));
        assert_eq!(character_of(0x0100_0100), Some('Ā'));
        assert_eq!(character_of(0xff08), None, "BackSpace is not a character");
    }

    /// The mask's Control and Shift bits are read out of the state word.
    #[test]
    fn the_mask_is_read_out_of_the_state_word() {
        assert!(ctrl_of(x::ControlMask | x::ShiftMask));
        assert!(!ctrl_of(x::ShiftMask));
        assert!(shift_of(x::ShiftMask));
        assert!(!shift_of(x::ControlMask));
    }
}
