// SPDX-License-Identifier: Apache-2.0

//! What the launcher does with the selected result.
//!
//! Three actions, and each one is a platform call the tree does not have
//! yet: open the file with its own program, open the folder that holds
//! it, and put its path on the clipboard.
//!
//! They live here and not in the model because they *are* the machine:
//! the model decides which one the user asked for, and this is what
//! carries it out.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::win::*;

/// Open a path with the platform's default program.
///
/// `ShellExecuteW` with the `open` verb: the same call a file manager
/// makes, so the answer is the user's own association and not a guess.
pub fn open(path: &str) -> Result<(), String> {
    shell(path, "open")
}

/// Open the folder that holds a path, with the file selected in it.
///
/// The `explore` verb is what selects the file; a launcher that opened
/// the folder without the file would make the user hunt for the one they
/// picked.
pub fn reveal(path: &str) -> Result<(), String> {
    shell(path, "explore")
}

/// One shell call.
fn shell(path: &str, verb: &str) -> Result<(), String> {
    unsafe {
        let verb16: Vec<u16> = verb.encode_utf16().chain(core::iter::once(0)).collect();
        let path16: Vec<u16> = path.encode_utf16().chain(core::iter::once(0)).collect();
        let r = ShellExecuteW(
            core::ptr::null_mut(),
            verb16.as_ptr(),
            path16.as_ptr(),
            core::ptr::null(),
            core::ptr::null(),
            SW_SHOWNORMAL,
        );
        // `ShellExecuteW` returns a value above 32 on success; anything
        // at or below it is an error code.
        if r as isize <= 32 {
            return Err(format!("{path} could not be opened ({})", r as isize));
        }
    }
    Ok(())
}

/// Put a path on the clipboard.
///
/// The clipboard is a global, and it is taken with the three calls the
/// platform documents: open it, put the data in it, close it. The data
/// is a piece of global memory the clipboard takes over when it is given
/// to `SetClipboardData`.
pub fn copy_path(path: &str) -> Result<(), String> {
    unsafe {
        if OpenClipboard(core::ptr::null_mut()) == 0 {
            return Err("the clipboard is busy".to_string());
        }
        let result = (|| {
            if EmptyClipboard() == 0 {
                return Err("the clipboard could not be emptied".to_string());
            }
            let bytes: Vec<u16> = path.encode_utf16().chain(core::iter::once(0)).collect();
            let size = bytes.len() * 2;
            let handle = GlobalAlloc(GMEM_MOVEABLE, size);
            if handle.is_null() {
                return Err("the clipboard memory could not be had".to_string());
            }
            let ptr = GlobalLock(handle);
            if ptr.is_null() {
                GlobalFree(handle);
                return Err("the clipboard memory could not be locked".to_string());
            }
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.cast::<u16>(), bytes.len());
            GlobalUnlock(handle);
            // The clipboard owns the memory now.
            if SetClipboardData(CF_UNICODETEXT, handle).is_null() {
                GlobalFree(handle);
                return Err("the path could not be put on the clipboard".to_string());
            }
            Ok(())
        })();
        CloseClipboard();
        result
    }
}

// `ShellExecuteW` is the one call here that is not in a library the Rust
// toolchain links by default: it lives in the shell library, and a launch
// that cannot open a file is not a launch.
#[link(name = "shell32")]
unsafe extern "system" {
    fn ShellExecuteW(
        hwnd: HWND,
        lpOperation: *const u16,
        lpFile: *const u16,
        lpParameters: *const u16,
        lpDirectory: *const u16,
        nShowCmd: i32,
    ) -> isize;
    fn OpenClipboard(hWndNewOwner: HWND) -> i32;
    fn EmptyClipboard() -> i32;
    fn CloseClipboard() -> i32;
    fn SetClipboardData(uFormat: u32, hMem: HANDLE) -> HANDLE;
    fn GlobalAlloc(uFlags: u32, dwBytes: usize) -> HANDLE;
    fn GlobalLock(hMem: HANDLE) -> *mut c_void;
    fn GlobalUnlock(hMem: HANDLE) -> i32;
    fn GlobalFree(hMem: HANDLE) -> HANDLE;
}

use core::ffi::c_void;

const SW_SHOWNORMAL: i32 = 1;
const CF_UNICODETEXT: u32 = 13;
const GMEM_MOVEABLE: u32 = 0x0002;
