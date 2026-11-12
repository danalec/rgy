// SPDX-License-Identifier: Apache-2.0

//! X11's numbers, without X11's calls.
//!
//! The event types, the modifier bits and the keysyms are **protocol
//! facts**, not platform handles. They live here, un-gated by
//! `target_os`, for two reasons: the decoder that turns a keysym into a
//! [`Key`](crate::types::Key) is tested on the host that cannot open an X
//! display, and `c_ulong` is four bytes on Windows but eight on Linux —
//! a constant typed as the platform's `long` would say one thing on one
//! host and another on the other. Plain Rust integers say the same thing
//! everywhere.
//!
//! `x.rs` re-exports this module so the Linux bodies can name `x::KeyPress`
//! and `crate::keys::KeyPress` and get the same number.

// The keysyms are X11's own names (`XK_Control_L`, `XK_Page_Up`): a
// renaming would make the decoder read as a translation of the platform's
// documentation instead of as the documentation's own words.
#![allow(non_upper_case_globals)]

/// The X11 event types the launcher listens for.
pub const KeyPress: i32 = 2;
/// A key went up.
pub const KeyRelease: i32 = 3;
/// A mouse button went down.
pub const ButtonPress: i32 = 4;
/// A mouse button went up.
pub const ButtonRelease: i32 = 5;
/// A window needs painting.
pub const Expose: i32 = 12;
/// A window was destroyed.
pub const DestroyNotify: i32 = 17;
/// A window changed size.
pub const ConfigureNotify: i32 = 22;
/// Another client wants the selection we own.
pub const SelectionRequest: i32 = 30;
/// Our selection request was answered.
pub const SelectionNotify: i32 = 31;
/// A client message arrived.
pub const ClientMessage: i32 = 33;
/// The keyboard map changed.
pub const MappingNotify: i32 = 34;
/// An extension event.
pub const GenericEvent: i32 = 35;

/// The event masks the launcher's windows select for. Xlib's masks are
/// `long`, but the values fit in a signed 64-bit integer everywhere.
pub const KeyPressMask: i64 = 1 << 0;
/// A key-release event.
pub const KeyReleaseMask: i64 = 1 << 1;
/// A mouse-button press.
pub const ButtonPressMask: i64 = 1 << 2;
/// A mouse-button release.
pub const ButtonReleaseMask: i64 = 1 << 3;
/// An expose event.
pub const ExposureMask: i64 = 1 << 15;
/// A configure/destroy event.
pub const StructureNotifyMask: i64 = 1 << 17;
/// A child window's geometry changed.
pub const SubstructureNotifyMask: i64 = 1 << 19;
/// A property changed on a window we watch.
pub const PropertyChangeMask: i64 = 1 << 22;
/// Every event the launcher's paint loop needs: expose and geometry.
pub const ALL_EXPOSURE: i64 = ExposureMask | StructureNotifyMask;

/// The modifier bits of an X11 state word.
pub const ShiftMask: u32 = 1 << 0;
/// Caps Lock.
pub const LockMask: u32 = 1 << 1;
/// Control.
pub const ControlMask: u32 = 1 << 2;
/// Mod1 (usually Alt).
pub const Mod1Mask: u32 = 1 << 3;
/// Mod4 (usually Super).
pub const Mod4Mask: u32 = 1 << 6;
/// Button 1 is held.
pub const Button1Mask: u32 = 1 << 8;

/// The keysyms the launcher names. Latin-1 keysyms equal the byte value;
/// the named ones are above it.
pub const XK_BackSpace: u64 = 0xff08;
/// Tab.
pub const XK_Tab: u64 = 0xff09;
/// Return.
pub const XK_Return: u64 = 0xff0d;
/// Escape.
pub const XK_Escape: u64 = 0xff1b;
/// Home.
pub const XK_Home: u64 = 0xff50;
/// Left arrow.
pub const XK_Left: u64 = 0xff51;
/// Up arrow.
pub const XK_Up: u64 = 0xff52;
/// Right arrow.
pub const XK_Right: u64 = 0xff53;
/// Down arrow.
pub const XK_Down: u64 = 0xff54;
/// Page Up.
pub const XK_Page_Up: u64 = 0xff55;
/// Page Down.
pub const XK_Page_Down: u64 = 0xff56;
/// End.
pub const XK_End: u64 = 0xff57;
/// Delete.
pub const XK_Delete: u64 = 0xffff;
/// Left Shift.
pub const XK_Shift_L: u64 = 0xffe1;
/// Right Shift.
pub const XK_Shift_R: u64 = 0xffe2;
/// Left Control.
pub const XK_Control_L: u64 = 0xffe3;
/// Right Control.
pub const XK_Control_R: u64 = 0xffe4;
/// Left Alt.
pub const XK_Alt_L: u64 = 0xffe9;
/// Right Alt.
pub const XK_Alt_R: u64 = 0xffea;
/// Left Super.
pub const XK_Super_L: u64 = 0xffeb;
/// Right Super.
pub const XK_Super_R: u64 = 0xffec;
/// Space.
pub const XK_space: u64 = 0x0020;

/// A printable ASCII byte is its own keysym.
pub fn keysym_of_ascii(c: u8) -> u64 {
    c as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// X11's own numbers, pinned: a decoder that named a different number
    /// for Control would never see a Ctrl+V.
    #[test]
    fn the_keysyms_are_x11s_own_numbers() {
        assert_eq!(XK_Control_L, 0xffe3);
        assert_eq!(XK_Return, 0xff0d);
        assert_eq!(XK_Delete, 0xffff);
        assert_eq!(keysym_of_ascii(b'v'), 0x76);
    }

    /// The modifier masks do not overlap.
    #[test]
    fn the_modifier_masks_are_distinct_bits() {
        assert_eq!(ControlMask & ShiftMask, 0);
        assert_eq!(ControlMask | ShiftMask, 0x5);
    }

    /// The event types the pump dispatches are the X11 ones.
    #[test]
    fn the_event_types_are_x11s_own_numbers() {
        assert_eq!(KeyPress, 2);
        assert_eq!(ClientMessage, 33);
        assert_eq!(SelectionNotify, 31);
    }
}
