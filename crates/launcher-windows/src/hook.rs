// SPDX-License-Identifier: Apache-2.0

//! The global hotkey: Control pressed twice.
//!
//! `RegisterHotKey` cannot do this — a hotkey needs a modifier, and the
//! only modifier the user wants here is Control on its own. So the hotkey
//! is a **low-level keyboard hook**: it sees the key before any window
//! does, and the state machine that decides whether two presses were the
//! gesture lives in the launcher's model, where it can be tested.
//!
//! The hook does two things and no more: it counts the presses, and it
//! posts a message to the window. It never touches the window itself —
//! the window belongs to the thread that owns it, and the hook belongs to
//! the thread that installed it, and those are the same thread only by
//! accident.

use alloc::string::{String, ToString};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use rgy_launcher::DoubleCtrl;

use crate::win::*;

/// The window the hook posts to. Zero until the hook is installed.
static WINDOW: AtomicIsize = AtomicIsize::new(0);

/// Whether Control is already down.
///
/// The keyboard repeats a key that is held down, and the repeats are
/// the same key going down again: the hook sees them all, one after the
/// other. A gesture that counts them counts a key held for a second a
/// dozen times, and the launcher opens and closes on its own.
static CONTROL_DOWN: AtomicBool = AtomicBool::new(false);

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
                return Err("the keyboard hook could not be installed".to_string());
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
                // The edge and not the event: the key that was already
                // down is a repeat, and a repeat is not a press.
                let already = CONTROL_DOWN.swap(true, Ordering::SeqCst);
                if is_press(down, already) {
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
                // The key is up: the next one down is a new press.
                CONTROL_DOWN.store(false, Ordering::SeqCst);
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

/// Whether the key is one of the Controls.
///
/// The hook hears the generic Control as well as the left and the right
/// one: a keyboard that does not say which side was pressed says the
/// generic one, and a press the hook does not hear breaks the gesture
/// that opens the window.
fn is_control(vk_code: u32) -> bool {
    vk_code == VK_CONTROL || vk_code == VK_LCONTROL || vk_code == VK_RCONTROL
}

/// Whether a key event is a press, and not a repeat.
///
/// `already_down` is whether the key was down when the event arrived.
/// The decision is a function of that and of nothing else, so it can be
/// told apart from a repeat without a keyboard.
fn is_press(down: bool, already_down: bool) -> bool {
    down && !already_down
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hook_is_not_installed_by_the_tests() {
        // The tests do not install a hook: a hook is a system-wide thing,
        // and a test that installs one would swallow the keyboard of the
        // machine it runs on.
        assert_eq!(WINDOW.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_key_that_goes_down_is_a_press() {
        assert!(is_press(true, false));
    }

    #[test]
    fn a_key_that_is_held_down_is_not_pressed_again() {
        assert!(!is_press(true, true), "the repeat is not a press");
    }

    #[test]
    fn a_key_that_comes_up_is_not_a_press() {
        assert!(!is_press(false, true));
        assert!(!is_press(false, false));
    }

    #[test]
    fn the_hook_hears_every_control_key() {
        assert!(is_control(VK_CONTROL));
        assert!(is_control(VK_LCONTROL));
        assert!(is_control(VK_RCONTROL));
        assert!(!is_control(VK_ESCAPE), "a key that is not Control");
    }
}
