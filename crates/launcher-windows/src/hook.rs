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
        if down {
            if ks.vkCode == VK_CONTROL {
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
            } else {
                // Another key: the pair is broken.
                let mut guard = PRESSES.lock();
                if let Some(machine) = guard.get().as_mut() {
                    machine.interrupt();
                }
            }
        }
    }
    unsafe { CallNextHookEx(core::ptr::null_mut(), code, wparam, lparam) }
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
}
