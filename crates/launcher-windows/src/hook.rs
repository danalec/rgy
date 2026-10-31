// SPDX-License-Identifier: Apache-2.0

//! The global hotkey: Control pressed twice.
//!
//! `RegisterHotKey` cannot do this — a hotkey needs a modifier, and the
//! only modifier the user wants here is Control on its own. So the hotkey
//! is a **low-level keyboard hook**: it sees the key before any window
//! does, and the state machine that decides whether two presses were the
//! gesture is in two halves, and both of them can be tested. Which Control
//! keys are down is this module's `ControlKeys`: one bit per keycode, and
//! not a flag for the three of them, because the hook hears three keys for
//! one idea. The window between the two presses is the launcher's model,
//! and the clock is the caller's — so a test decides what time it is.
//!
//! The hook does two things and no more: it counts the presses, and it
//! posts a message to the window. It never touches the window itself —
//! the window belongs to the thread that owns it, and the hook belongs to
//! the thread that installed it, and those are the same thread only by
//! accident.

use alloc::format;
use alloc::string::String;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use rgy_launcher::DoubleCtrl;

use crate::win::*;

/// The window the hook posts to. Zero until the hook is installed.
static WINDOW: AtomicIsize = AtomicIsize::new(0);

/// The Control keys that are down.
///
/// The keyboard repeats a key that is held down, and the repeats are
/// the same key going down again: the hook sees them all, one after the
/// other. A gesture that counts them counts a key held for a second a
/// dozen times, and the launcher opens and closes on its own — so what
/// keeps the repeats apart is one bit per keycode, and not a flag for
/// the three of them: see [`ControlKeys`].
static CONTROL_DOWN: Spin<ControlKeys> = Spin::new(ControlKeys::new());

/// The press counter, and the clock the model needs.
static PRESSES: Spin<Option<DoubleCtrl>> = Spin::new(None);

/// A lock for the hook's state.
///
/// The hook's procedure runs on the thread that installed it, between two
/// messages, and the state it guards is three words long. A mutex would do
/// the same job with a system call; this does it with one atomic exchange,
/// and the crate stays `no_std`.
///
/// It is safe because the lock is only ever taken on the installing
/// thread: a low-level keyboard hook is called back on the thread that
/// installed it, and that thread is the one running the message loop. Two
/// threads never contend for it, and the section it guards never blocks —
/// so the spin always ends on the first try.
struct Spin<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

/// The guard a [`Spin`] hands out.
struct Guard<'a, T> {
    spin: &'a Spin<T>,
}

impl<T> Spin<T> {
    /// A lock over `value`.
    const fn new(value: T) -> Spin<T> {
        Spin {
            locked: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    /// Take the lock.
    fn lock(&self) -> Guard<'_, T> {
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
    fn get(&mut self) -> &mut T {
        // SAFETY: the lock is held, and the lock is held by one thread at
        // a time.
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

/// The Control keys that are down, one bit each.
///
/// The hook hears the generic Control as well as the left and the right
/// one, and each of them goes down and comes up on its own: the right
/// one can be tapped while the left one is still held. A flag for the
/// three of them cannot say that — the release of the right one would
/// clear the flag, the repeat of the left one would arrive at a flag
/// that says "no Control is down", and that repeat would be counted as
/// a press, so the next real press would be the one that is missed. The
/// mask says exactly which keys are down, and every question about it
/// has an answer.
///
/// The gesture is **two Control presses**, whichever side they come
/// from: the user presses Control with whichever hand is free, and a
/// launcher that answers to one side only misses the other. A press is
/// a Control that went down when **no Control was down** — not the
/// edge of one keycode — so a key that is held is one press and never
/// two, a Control that comes down while another one is held is not a
/// press at all, and the release of one Control says nothing about the
/// others.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ControlKeys {
    /// One bit per Control keycode: the generic one, the left one and
    /// the right one.
    down: u8,
}

impl ControlKeys {
    /// The generic Control.
    const GENERIC: u8 = 1 << 0;
    /// The left Control.
    const LEFT: u8 = 1 << 1;
    /// The right Control.
    const RIGHT: u8 = 1 << 2;

    /// A machine with no Control down.
    const fn new() -> ControlKeys {
        ControlKeys { down: 0 }
    }

    /// A key going down. True when this is a press.
    ///
    /// A press is a Control that went down when **no Control was
    /// down**. A Control that comes down while another one is held is
    /// not a press: the user is holding Control to press something
    /// else, and a window that opens in their face is a launcher they
    /// cannot trust. And a Control that is still down is the keyboard
    /// repeating it, which is not a press either.
    fn press(&mut self, vk_code: u32) -> bool {
        let Some(bit) = control_bit(vk_code) else {
            return false;
        };
        // The mask is kept whatever the answer is: which keys are down
        // is a fact, and the press is a question about it.
        let is_press = self.down == 0;
        self.down |= bit;
        is_press
    }

    /// A key coming up. Only that key is forgotten.
    ///
    /// The others are still down, and a key that is still down does not
    /// press again.
    fn release(&mut self, vk_code: u32) {
        if let Some(bit) = control_bit(vk_code) {
            self.down &= !bit;
        }
    }
}

/// The installed hook.
pub struct Hook {
    handle: HHOOK,
}

impl Hook {
    /// Install the hook for `hwnd`.
    ///
    /// The hook is installed on the calling thread, which is the thread
    /// that runs the message loop — so the callback is called on that
    /// thread, between two messages, and it may post to the window
    /// directly.
    pub fn install(hwnd: HWND) -> Result<Hook, String> {
        WINDOW.store(hwnd as isize, Ordering::SeqCst);
        *PRESSES.lock().get() = Some(DoubleCtrl::new());
        unsafe {
            let handle = SetWindowsHookExW(
                WH_KEYBOARD_LL,
                Some(hook_proc),
                GetModuleHandleW(core::ptr::null()),
                0,
            );
            if handle.is_null() {
                // The reason is in the last error, and the reason is the
                // difference between a policy that said no and a machine
                // that could not.
                let code = GetLastError();
                return Err(format!("the keyboard hook could not be installed ({code})"));
            }
            Ok(Hook { handle })
        }
    }
}

impl Drop for Hook {
    fn drop(&mut self) {
        unsafe {
            UnhookWindowsHookEx(self.handle);
        }
        WINDOW.store(0, Ordering::SeqCst);
    }
}

/// The hook's own procedure.
unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION && lparam != 0 {
        let ks = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        let down = wparam as u32 == WM_KEYDOWN || wparam as u32 == WM_SYSKEYDOWN;
        let up = wparam as u32 == WM_KEYUP || wparam as u32 == WM_SYSKEYUP;
        if is_control(ks.vkCode) {
            if down {
                // A press is a Control that went down when no Control
                // was down: the keyboard repeating a key that is held
                // is not a press, and neither is the second Control
                // that comes down while the first one is held.
                let is_press = {
                    let mut guard = CONTROL_DOWN.lock();
                    guard.get().press(ks.vkCode)
                };
                if is_press {
                    let now = unsafe { GetTickCount64() };
                    let fired = {
                        let mut guard = PRESSES.lock();
                        let machine = guard.get().get_or_insert_with(DoubleCtrl::new);
                        machine.press(now)
                    };
                    if fired {
                        let hwnd = WINDOW.load(Ordering::SeqCst) as HWND;
                        if !hwnd.is_null() {
                            unsafe {
                                PostMessageW(hwnd, crate::window::WM_TOGGLE, 0, 0);
                            }
                        }
                    }
                }
            } else if up {
                // The key is up: only that key is forgotten, and the
                // others keep their state — a Control that is still
                // held does not press again.
                let mut guard = CONTROL_DOWN.lock();
                guard.get().release(ks.vkCode);
            }
        } else if down {
            // Another key: the pair is broken.
            let mut guard = PRESSES.lock();
            if let Some(machine) = guard.get().as_mut() {
                machine.interrupt();
            }
        }
    }
    unsafe { CallNextHookEx(core::ptr::null_mut(), code, wparam, lparam) }
}

/// The bit of a Control keycode, and nothing for a key that is not one.
fn control_bit(vk_code: u32) -> Option<u8> {
    match vk_code {
        VK_CONTROL => Some(ControlKeys::GENERIC),
        VK_LCONTROL => Some(ControlKeys::LEFT),
        VK_RCONTROL => Some(ControlKeys::RIGHT),
        _ => None,
    }
}

/// Whether the key is one of the Controls.
///
/// The hook hears the generic Control as well as the left and the right
/// one: a keyboard that does not say which side was pressed says the
/// generic one, and a press the hook does not hear breaks the gesture
/// that opens the window.
fn is_control(vk_code: u32) -> bool {
    control_bit(vk_code).is_some()
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;

    #[test]
    fn the_hook_is_not_installed_by_the_tests() {
        // The tests do not install a hook: a hook is a system-wide thing,
        // and a test that installs one would swallow the keyboard of the
        // machine it runs on.
        assert_eq!(WINDOW.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn the_hook_hears_every_control_key() {
        assert!(is_control(VK_CONTROL));
        assert!(is_control(VK_LCONTROL));
        assert!(is_control(VK_RCONTROL));
        assert!(!is_control(VK_ESCAPE), "a key that is not Control");
    }

    #[test]
    fn a_key_that_goes_down_is_a_press() {
        let mut keys = ControlKeys::new();
        assert!(keys.press(VK_LCONTROL), "the key went down");
    }

    #[test]
    fn a_key_that_is_held_down_is_not_pressed_again() {
        let mut keys = ControlKeys::new();
        assert!(keys.press(VK_LCONTROL));
        assert!(!keys.press(VK_LCONTROL), "the repeat is not a press");
    }

    #[test]
    fn a_key_that_comes_up_can_be_pressed_again() {
        let mut keys = ControlKeys::new();
        keys.press(VK_LCONTROL);
        keys.release(VK_LCONTROL);
        assert!(keys.press(VK_LCONTROL), "the key went down again");
    }

    #[test]
    fn the_release_of_one_control_does_not_release_the_other() {
        let mut keys = ControlKeys::new();
        keys.press(VK_LCONTROL);
        keys.press(VK_RCONTROL);
        keys.release(VK_RCONTROL);
        assert!(
            !keys.press(VK_LCONTROL),
            "the left Control is still down: this is a repeat"
        );
    }

    #[test]
    fn one_control_press_does_not_open_the_window() {
        let (presses, fired) = run(&[(VK_LCONTROL, true), (VK_LCONTROL, false)]);
        assert_eq!(presses, [VK_LCONTROL], "one press is half a gesture");
        assert!(!fired, "one Control does not open the window");
    }

    #[test]
    fn two_control_presses_open_the_window() {
        let (presses, fired) = run(&[
            (VK_LCONTROL, true),
            (VK_LCONTROL, false),
            (VK_LCONTROL, true),
            (VK_LCONTROL, false),
        ]);
        assert_eq!(presses, [VK_LCONTROL, VK_LCONTROL]);
        assert!(fired, "two Control presses are the gesture");
    }

    #[test]
    fn a_held_control_does_not_open_the_window() {
        // The keyboard repeats a key that is held, and a repeat is the
        // same key going down again.
        let (presses, fired) = run(&[
            (VK_LCONTROL, true),
            (VK_LCONTROL, true),
            (VK_LCONTROL, true),
            (VK_LCONTROL, false),
        ]);
        assert_eq!(presses, [VK_LCONTROL], "a held Control is one press");
        assert!(!fired, "a held Control does not open the window");
    }

    #[test]
    fn holding_one_control_and_tapping_the_other_is_one_press() {
        // The left one is held and the right one is tapped: the user is
        // holding Control to press something else, and a Control that
        // goes down while another one is held is not a press.
        let (presses, fired) = run(&[
            (VK_LCONTROL, true),
            (VK_RCONTROL, true),
            (VK_RCONTROL, false),
            (VK_LCONTROL, false),
        ]);
        assert_eq!(presses, [VK_LCONTROL], "one press is half a gesture");
        assert!(!fired, "a held Control does not open the window");
    }

    #[test]
    fn the_repeat_of_a_control_that_is_still_down_is_not_a_press() {
        // The bug this fixes: the right Control comes up while the left
        // one is still held, and the repeat of the left one arrives
        // next. The release of one Control says nothing about the
        // other, so the repeat is a repeat — and the only press in the
        // lot is the left one's, the one that went down when no Control
        // was down.
        let (presses, _) = run(&[
            (VK_LCONTROL, true),
            (VK_RCONTROL, true),
            (VK_RCONTROL, false),
            (VK_LCONTROL, true),
            (VK_LCONTROL, false),
        ]);
        assert_eq!(
            presses,
            [VK_LCONTROL],
            "the repeat of the left Control is not a press"
        );
    }

    /// A tap of each Control is the gesture: the user presses Control
    /// with whichever hand is free, and a launcher that answers to one
    /// side only misses the other.
    #[test]
    fn a_tap_of_each_control_is_the_gesture() {
        // Nothing was held when the second one went down, so it is a
        // press.
        let (presses, fired) = run(&[
            (VK_LCONTROL, true),
            (VK_LCONTROL, false),
            (VK_RCONTROL, true),
            (VK_RCONTROL, false),
        ]);
        assert_eq!(presses, [VK_LCONTROL, VK_RCONTROL]);
        assert!(fired, "the gesture is two Control presses");
    }

    /// Both Controls at once is one press: the second one went down
    /// when the first one was already down.
    ///
    /// A window that opens while the user is holding Control is a
    /// window nobody asked for.
    #[test]
    fn holding_both_controls_does_not_open_the_window() {
        let (presses, fired) = run(&[
            (VK_LCONTROL, true),
            (VK_RCONTROL, true),
            (VK_LCONTROL, true),
            (VK_RCONTROL, true),
        ]);
        assert_eq!(presses, [VK_LCONTROL], "holding both is one press");
        assert!(!fired, "holding both Controls does not open the window");
    }

    /// The rule in one line: a press is a Control that went down when
    /// no Control was down.
    #[test]
    fn a_control_that_goes_down_while_another_is_held_is_not_a_press() {
        let mut keys = ControlKeys::new();
        assert!(keys.press(VK_LCONTROL), "the first Control went down");
        assert!(
            !keys.press(VK_RCONTROL),
            "a Control was already down: this is not a press"
        );
    }

    /// The presses a sequence of key events produces, and whether the
    /// gesture fired.
    ///
    /// The events are `(keycode, is down)`, and the clock moves one
    /// millisecond per event: the model reads the clock the caller
    /// gives it, and here the caller is the test.
    fn run(events: &[(u32, bool)]) -> (Vec<u32>, bool) {
        let mut keys = ControlKeys::new();
        let mut machine = DoubleCtrl::new();
        let mut fired = false;
        let mut presses = Vec::new();
        for (index, (vk_code, down)) in events.iter().enumerate() {
            let now = index as u64;
            if is_control(*vk_code) {
                if *down {
                    if keys.press(*vk_code) {
                        presses.push(*vk_code);
                        fired |= machine.press(now);
                    }
                } else {
                    keys.release(*vk_code);
                }
            } else if *down {
                // Another key breaks the pair, as it does in the hook.
                machine.interrupt();
            }
        }
        (presses, fired)
    }
}
