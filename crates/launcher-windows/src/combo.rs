// SPDX-License-Identifier: Apache-2.0

//! The combo hotkey: modifiers held with a key, registered with the
//! platform.
//!
//! A combo is what `RegisterHotKey` is for, and it needs no hook: the
//! platform watches the keys and **posts** `WM_HOTKEY` to the window —
//! a posted message comes out of the queue, so the pump is where the
//! answer is given and the window's procedure never sees it. The
//! double tap of a lone modifier is the other shape ([`crate::hook`]),
//! the one the registration cannot express; the settings choose the
//! shape, and only one of the two is ever armed.

// A handle is a pointer, and this file never writes through one: it
// hands it to the platform, which is the only thing that may read it.
// The lint is about a function that dereferences a pointer it was
// given, and a function that passes one on is a different thing — the
// seam's own file (crates/discovery-windows/src/win.rs) says the same.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use alloc::format;
use alloc::string::String;

use crate::win::*;

/// The one hotkey this window registers.
///
/// The id is the caller's own name for the registration, and the window
/// has exactly one combo: a name wider than the need is a name nobody
/// can check.
const ID: i32 = 1;

/// A registered combo.
///
/// The registration is given back when the guard is dropped: a combo
/// that outlived its window would hold the keys of every program on the
/// machine for nobody, and a guard is the one way to be sure it does
/// not.
pub struct Combo {
    /// The window the message is posted to, and the one the
    /// registration is given back to.
    hwnd: HWND,
}

impl Combo {
    /// Register a combo for `hwnd`.
    ///
    /// The modifiers are the platform's own `MOD_*` bits — the settings
    /// keep them in the platform's numbering, so they come here exactly
    /// as they were read — and the key is a virtual-key code. The
    /// answer is the refusal's reason, with the platform's own number
    /// in it: a combination another program already has and a machine
    /// that could not are different facts, and the caller is the one
    /// who has to tell them apart.
    pub fn install(hwnd: HWND, modifiers: u8, key: u32) -> Result<Combo, String> {
        unsafe {
            if RegisterHotKey(hwnd, ID, modifiers as u32, key) == 0 {
                let code = GetLastError();
                return Err(format!("the hotkey could not be registered ({code})"));
            }
        }
        Ok(Combo { hwnd })
    }
}

impl Drop for Combo {
    fn drop(&mut self) {
        unsafe {
            UnregisterHotKey(self.hwnd, ID);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window::Window;

    /// The platform's F12, pinned: the number is the platform's own
    /// fact, and this test is the only place the tree names it.
    const VK_F12: u32 = 0x7B;

    /// A combo is registered and given back.
    ///
    /// The platform answers for the registration: the same combination
    /// cannot be taken twice by one process, so taking it, dropping it
    /// and taking it again is the whole life of the guard, proved.
    #[test]
    fn a_combo_is_registered_and_given_back() {
        let window = Window::new("rgxy-test").expect("the window was made");
        let hwnd = window.hwnd();
        // A combination no program on a test machine reaches for: the
        // test only asks the platform to hold it for a moment. The
        // modifier bits are the settings' own consts, which are the
        // platform's own numbers, pinned in their own test.
        let modifiers = rgy_launcher::settings::mods::CONTROL | rgy_launcher::settings::mods::SHIFT;
        let combo = Combo::install(hwnd, modifiers, VK_F12).expect("the combo was registered");
        drop(combo);
        let again = Combo::install(hwnd, modifiers, VK_F12);
        assert!(again.is_ok(), "the combination was given back");
    }
}
