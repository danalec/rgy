// SPDX-License-Identifier: Apache-2.0

//! The global hotkey on X11: passive grabs on the root window.
//!
//! X11 has two shapes of global key, and both are installed with
//! [`XGrabKey`](crate::x::XGrabKey) on the **root** window, not on a
//! window of the launcher's own — a passive grab is the one mechanism
//! that sees a key no matter which window has the focus. The `hwnd` the
//! Windows backend hands to `install` is therefore ignored here: on X11
//! the grab belongs to the root, and the launcher's own window is only
//! used to tell a grabbed root event from an event it received itself.
//!
//! The two shapes are:
//!
//! - **Double tap** ([`Hook`]): a lone modifier pressed twice inside a
//!   window of milliseconds. `XGrabKey` cannot express "a modifier on
//!   its own" the way a registered hotkey wants one, so the left and
//!   right keysyms of the modifier are grabbed with no modifier mask and
//!   [`rgy_launcher::DoubleCtrl`] — the model's own state machine, the
//!   one the Windows hook uses — decides whether two presses were the
//!   gesture. The X event is replayed with `ReplayKeyboard` so the
//!   user's Ctrl still reaches the window they were typing in.
//! - **Combo** ([`Combo`]): modifiers held with one key. The settings
//!   carry Windows `MOD_*` bits and a virtual-key code; the pure
//!   functions [`combo_mask`] and [`combo_keysym`] translate them into
//!   X11 masks and keysyms, and the grab is the ordinary one.
//!
//! Only one arm is installed at a time, so the state is one `Arm` behind
//! one spinlock. [`on_key_press`] is called by the window pump for
//! **every** `KeyPress`; it is the decision, not the grab.
//!
//! # `no_std`
//!
//! `alloc`, `core` and the [`crate::x`] seam. The lock is an atomic flag
//! over an `UnsafeCell`, the same shape as `launcher-windows`' `Spin`:
//! the state is three words long, the section never blocks, and a `Mutex`
//! would need `std`. The translation functions are pure and tested on
//! the host that cannot open a display.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::ffi::{c_int, c_void};
use core::sync::atomic::{AtomicBool, Ordering};

use rgy_launcher::DoubleCtrl;
use rgy_launcher::settings::{TapKey, mods};

use crate::types::Event;
use crate::{decode, runtime, x};

/// A lock for one small piece of global state.
///
/// The lock is taken for a section that never blocks and never calls
/// back into the code that holds it, so the spin ends on the first try.
/// It is `pub(crate)` because `tray` and `actions` keep one global each
/// of their own and there is no reason for three copies of the same
/// lock in one crate.
pub(crate) struct Spin<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

/// The guard a [`Spin`] hands out; dropping it releases the lock.
pub(crate) struct Guard<'a, T> {
    spin: &'a Spin<T>,
}

impl<T> Spin<T> {
    /// A lock over `value`.
    pub(crate) const fn new(value: T) -> Spin<T> {
        Spin {
            locked: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    /// Take the lock.
    pub(crate) fn lock(&self) -> Guard<'_, T> {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        Guard { spin: self }
    }
}

impl<T> Guard<'_, T> {
    /// The value, with the lock held.
    pub(crate) fn get(&mut self) -> &mut T {
        // SAFETY: the lock is held, and it is held by one thread at a time.
        unsafe { &mut *self.spin.value.get() }
    }
}

impl<T> Drop for Guard<'_, T> {
    fn drop(&mut self) {
        self.spin.locked.store(false, Ordering::Release);
    }
}

// SAFETY: the lock is what makes the value reachable from more than one
// thread, and it is taken before the value is touched.
unsafe impl<T: Send> Sync for Spin<T> {}

/// Which shape of hotkey is armed, and the state it needs.
enum Arm {
    /// Nothing is armed.
    None,
    /// A lone modifier tapped twice.
    Tap {
        /// The modifier the gesture watches.
        key: TapKey,
        /// The model's own double-tap state machine.
        machine: DoubleCtrl,
        /// The keycodes that were grabbed, for `Drop` to ungrab.
        codes: Vec<u8>,
    },
    /// Modifiers held with one key.
    Combo {
        /// The X11 modifier mask.
        mask: u32,
        /// The keysym of the key that completes the combo.
        keysym: u64,
        /// The grabbed keycode, for `Drop` to ungrab.
        code: u8,
    },
}

/// The one arm, shared by the pump and the guards.
static ARM: Spin<Arm> = Spin::new(Arm::None);

/// The left and right keysyms of a modifier.
///
/// X11 reports one idea three ways — a generic key and a left and a
/// right one — and the gesture answers to whichever of them the user's
/// hand found. Pure, so the set can be asserted without a display.
fn tap_keysyms(key: TapKey) -> [u64; 2] {
    // `KeySym`/`c_ulong` is `u64` on the LP64 Linux ABI this seam is
    // pinned to (see `x.rs`'s compile-time layout assertions), so the
    // constants need no cast to fit `[u64; 2]`.
    match key {
        TapKey::Ctrl => [x::XK_Control_L, x::XK_Control_R],
        TapKey::Alt => [x::XK_Alt_L, x::XK_Alt_R],
        TapKey::Shift => [x::XK_Shift_L, x::XK_Shift_R],
    }
}

/// Translate the settings' Windows `MOD_*` bits into an X11 mask.
///
/// `MOD_ALT` is Mod1 on X11, `MOD_CONTROL` is Control, `MOD_SHIFT` is
/// Shift and `MOD_WIN` is Mod4. Pure: a translation table between two
/// spellings of a fact is a table that can be tested.
pub fn combo_mask(modifiers: u8) -> u32 {
    let mut mask = 0u32;
    if modifiers & mods::ALT != 0 {
        mask |= x::Mod1Mask;
    }
    if modifiers & mods::CONTROL != 0 {
        mask |= x::ControlMask;
    }
    if modifiers & mods::SHIFT != 0 {
        mask |= x::ShiftMask;
    }
    if modifiers & mods::WIN != 0 {
        mask |= x::Mod4Mask;
    }
    mask
}

/// Translate a Windows virtual-key code into an X11 keysym.
///
/// The common keys are named; `F1..F12` are `0xffbe + (n - 1)`; digits
/// and letters are their Latin-1 keysyms (letters use the unshifted,
/// lower-case spelling — `XKeysymToKeycode` finds the same physical key
/// for either case); and an unlisted OEM code falls back to its byte
/// value, which is the honest answer for a code nobody named.
pub fn combo_keysym(key: u32) -> u64 {
    match key {
        0x20 => x::XK_space,
        0x0d => x::XK_Return,
        0x09 => x::XK_Tab,
        0x1b => x::XK_Escape,
        0x25 => x::XK_Left,
        0x26 => x::XK_Up,
        0x27 => x::XK_Right,
        0x28 => x::XK_Down,
        // XK_F1 is 0xffbe and XK_F12 is 0xffc9, one per VK code.
        code @ 0x70..=0x7b => (0xffbe + (code - 0x70)) as u64,
        code @ 0x30..=0x39 => code as u64,
        code @ 0x41..=0x5a => (code + 0x20) as u64,
        0xba => b';' as u64,
        0xbb => b'=' as u64,
        0xbc => b',' as u64,
        0xbd => b'-' as u64,
        0xbe => b'.' as u64,
        0xbf => b'/' as u64,
        0xc0 => b'`' as u64,
        0xdb => b'[' as u64,
        0xdc => b'\\' as u64,
        0xdd => b']' as u64,
        0xde => b'\'' as u64,
        code => (code as u8) as u64,
    }
}

/// The installed double-tap hotkey.
///
/// The guard owns the grabbed keycodes: `Drop` gives them back even if
/// [`reset`] has already cleared the arm.
pub struct Hook {
    codes: Vec<u8>,
}

impl Hook {
    /// Grab the left and right keysyms of `key` on the root window.
    ///
    /// `hwnd` is ignored: on X11 the grab is on the root, not a window.
    /// Both `pointer_mode` and `keyboard_mode` are `GrabModeAsync`, and
    /// `owner_events = 0` so the root sees the key before any window
    /// does. A grab that the server refuses is an error that names the
    /// keysym and the status Xlib returned.
    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    pub fn install(_hwnd: *mut c_void, key: TapKey, window_ms: u32) -> Result<Hook, String> {
        let display = runtime::display();
        if display.is_null() {
            return Err("the X display is not open".to_string());
        }
        let root = runtime::root();
        let mut codes = Vec::new();
        for keysym in tap_keysyms(key) {
            let code = unsafe { x::XKeysymToKeycode(display, keysym) };
            if code == 0 {
                ungrab_all(display, root, &codes, 0);
                return Err(format!("the keysym {keysym:#x} has no key code"));
            }
            // Xlib answers a passive grab with a status: `Success` (0)
            // on some builds, 1 when the request was simply queued on
            // others. Anything else is a refusal (BadAccess and
            // friends), so both benign values count as installed.
            let status = unsafe {
                x::XGrabKey(
                    display,
                    code as c_int,
                    0,
                    root,
                    0,
                    x::GrabModeAsync,
                    x::GrabModeAsync,
                )
            };
            if status != x::Success && status != 1 {
                ungrab_all(display, root, &codes, 0);
                return Err(format!(
                    "the key grab for keysym {keysym:#x} was refused ({status})"
                ));
            }
            codes.push(code);
        }
        *ARM.lock().get() = Arm::Tap {
            key,
            machine: DoubleCtrl::with_window(window_ms as u64),
            codes: codes.clone(),
        };
        unsafe {
            x::XFlush(display);
        }
        Ok(Hook { codes })
    }
}

impl Drop for Hook {
    fn drop(&mut self) {
        ungrab_all(runtime::display(), runtime::root(), &self.codes, 0);
        *ARM.lock().get() = Arm::None;
    }
}

/// The installed registered-combo hotkey.
pub struct Combo {
    code: u8,
    mask: u32,
}

impl Combo {
    /// Grab `modifiers` + `key` on the root window.
    ///
    /// `hwnd` is ignored for the same reason as [`Hook::install`]: the
    /// grab belongs to the root. The Windows `MOD_*` bits and virtual-key
    /// code are translated by [`combo_mask`] and [`combo_keysym`] before
    /// the grab is made.
    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    pub fn install(_hwnd: *mut c_void, modifiers: u8, key: u32) -> Result<Combo, String> {
        let display = runtime::display();
        if display.is_null() {
            return Err("the X display is not open".to_string());
        }
        let root = runtime::root();
        let mask = combo_mask(modifiers);
        let keysym = combo_keysym(key);
        let code = unsafe { x::XKeysymToKeycode(display, keysym as x::KeySym) };
        if code == 0 {
            return Err(format!("the keysym {keysym:#x} has no key code"));
        }
        let status = unsafe {
            x::XGrabKey(
                display,
                code as c_int,
                mask,
                root,
                0,
                x::GrabModeAsync,
                x::GrabModeAsync,
            )
        };
        if status != x::Success && status != 1 {
            return Err(format!(
                "the key grab for keysym {keysym:#x} was refused ({status})"
            ));
        }
        *ARM.lock().get() = Arm::Combo { mask, keysym, code };
        unsafe {
            x::XFlush(display);
        }
        Ok(Combo { code, mask })
    }
}

impl Drop for Combo {
    fn drop(&mut self) {
        ungrab_all(runtime::display(), runtime::root(), &[self.code], self.mask);
        *ARM.lock().get() = Arm::None;
    }
}

/// Give back a set of passive grabs and push the requests out.
fn ungrab_all(display: *mut c_void, root: u64, codes: &[u8], mask: u32) {
    if display.is_null() {
        return;
    }
    unsafe {
        for &code in codes {
            x::XUngrabKey(display, code as c_int, mask, root);
        }
        x::XFlush(display);
    }
}

/// Decide what a `KeyPress` means for the armed hotkey.
///
/// Called by the window pump for **every** `KeyPress`. The keysym comes
/// from `XLookupKeysym` at shift level 0.
///
/// For a Tap arm: when the keysym is the watched modifier and the event
/// came from the root grab (its window is not the launcher's own), the
/// event is replayed with `ReplayKeyboard` so the user's Ctrl still
/// works in the window they were typing in. Then the model's machine
/// is fed the event's timestamp; a completed pair is a Toggle. Any
/// other key interrupts the pair.
///
/// For a Combo arm: the modifier state must contain the whole mask and
/// the keysym must be the watched one.
pub(crate) fn on_key_press(ev: &x::XEvent) -> Option<Event> {
    let display = runtime::display();
    // `KeySym` and `Time` are `c_ulong` = `u64` on this ABI, so the
    // accessors hand `u64` straight to the model's machine.
    let keysym = unsafe { x::XLookupKeysym(ev, 0) };
    let time = x::ev_time(ev);
    let mut guard = ARM.lock();
    match guard.get() {
        Arm::Tap { key, machine, .. } => {
            if decode::modifier_of(keysym) == Some(*key) {
                if x::ev_window(ev) != runtime::window() {
                    unsafe {
                        x::XAllowEvents(display, x::ReplayKeyboard, x::ev_time(ev));
                    }
                }
                if machine.press(time) {
                    return Some(Event::Toggle);
                }
                None
            } else {
                machine.interrupt();
                None
            }
        }
        Arm::Combo {
            mask,
            keysym: watched,
            ..
        } => {
            let state = x::ev_state(ev);
            if (state & *mask) == *mask && keysym == *watched {
                Some(Event::Toggle)
            } else {
                None
            }
        }
        Arm::None => None,
    }
}

/// Forget the armed hotkey, and give its grabs back.
///
/// Called by the window's `Drop`: after the window is gone there is no
/// pump to feed [`on_key_press`], and leaving an arm installed would
/// make a later press read a machine nobody owns. The arm keeps its own
/// record of what was grabbed, so `reset` releases it here too — a
/// Window whose guard was already dropped cannot leave a passive grab
/// on the root. An ungrab of a key that is no longer grabbed is a
/// harmless no-op, so this and the guards' own `Drop` may both run.
pub(crate) fn reset() {
    let mut guard = ARM.lock();
    {
        let arm = guard.get();
        match arm {
            Arm::Tap { codes, .. } => {
                ungrab_all(runtime::display(), runtime::root(), codes, 0);
            }
            Arm::Combo { mask, code, .. } => {
                ungrab_all(runtime::display(), runtime::root(), &[*code], *mask);
            }
            Arm::None => {}
        }
    }
    *guard.get() = Arm::None;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Space and Return are the two keys the settings name most often,
    /// and they are X11's own keysyms.
    #[test]
    fn the_common_keys_are_x11_keysyms() {
        assert_eq!(combo_keysym(0x20), x::XK_space);
        assert_eq!(combo_keysym(0x0d), x::XK_Return);
        assert_eq!(combo_keysym(0x09), x::XK_Tab);
        assert_eq!(combo_keysym(0x1b), x::XK_Escape);
    }

    /// The Windows `MOD_*` bits become the X11 masks, and two of them
    /// together are both bits.
    #[test]
    fn the_modifier_bits_become_x11_masks() {
        assert_eq!(
            combo_mask(mods::CONTROL | mods::SHIFT),
            x::ControlMask | x::ShiftMask
        );
        assert_eq!(combo_mask(mods::ALT), x::Mod1Mask);
        assert_eq!(combo_mask(mods::WIN), x::Mod4Mask);
        assert_eq!(combo_mask(0), 0);
    }

    /// F1 is 0xffbe and F12 is 0xffc9: twelve keys, no gap.
    #[test]
    fn the_function_keys_are_consecutive_keysyms() {
        assert_eq!(combo_keysym(0x70), 0xffbe);
        assert_eq!(combo_keysym(0x7b), 0xffc9);
    }

    /// Each modifier has a left and a right keysym, and the three sets
    /// do not overlap.
    #[test]
    fn tap_keysyms_are_the_left_and_right_of_the_modifier() {
        assert_eq!(
            tap_keysyms(TapKey::Ctrl),
            [x::XK_Control_L, x::XK_Control_R]
        );
        assert_eq!(tap_keysyms(TapKey::Alt), [x::XK_Alt_L, x::XK_Alt_R]);
        assert_eq!(tap_keysyms(TapKey::Shift), [x::XK_Shift_L, x::XK_Shift_R]);
        assert_ne!(tap_keysyms(TapKey::Ctrl)[0], tap_keysyms(TapKey::Alt)[0]);
    }

    /// The model's own machine: two presses inside the window fire
    /// once, and a different key in between breaks the pair. This is
    /// the exact decision `on_key_press` makes, without a display.
    #[test]
    fn two_presses_fire_once_and_another_key_interrupts() {
        let mut machine = DoubleCtrl::with_window(400);
        assert!(!machine.press(1000), "the first press is half a gesture");
        assert!(machine.press(1100), "the second press completes it");
        assert!(!machine.press(1200), "one window fires once");
        machine.interrupt();
        assert!(!machine.press(1300), "a letter in between breaks the pair");
        assert!(machine.press(1400), "and the next pair is a new one");
    }
}
