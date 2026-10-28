// SPDX-License-Identifier: Apache-2.0

//! What the launcher does with the selected result, and with the
//! clipboard.
//!
//! Four actions, and each one is a platform call the tree does not have
//! yet: open the file with its own program, open the folder that holds
//! it, put its path on the clipboard, and take the clipboard's text back.
//!
//! They live here and not in the model because they *are* the machine:
//! the model decides which one the user asked for, and this is what
//! carries it out.
//!
//! The words these actions fail with are the user's, not the programmer's:
//! they land in the line under the launcher's field, and that line speaks
//! one language. The code around them speaks another; the messages are
//! for the person reading the window.
//!
//! The clipboard is the one resource that is **shared**: it belongs to
//! whoever opens it until it is closed, and a launcher that opens it and
//! never gives it back takes it away from every other program on the
//! machine. So both directions — the copy and the paste — open it, do
//! their work, and close it, and they live in the same file because they
//! are the same three calls in two directions.

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
            return Err(format!("{path} não pôde ser aberto ({})", r as isize));
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
            return Err("a área de transferência está ocupada".to_string());
        }
        let result = (|| {
            if EmptyClipboard() == 0 {
                return Err("a área de transferência não pôde ser esvaziada".to_string());
            }
            let bytes: Vec<u16> = path.encode_utf16().chain(core::iter::once(0)).collect();
            let size = bytes.len() * 2;
            let handle = GlobalAlloc(GMEM_MOVEABLE, size);
            if handle.is_null() {
                return Err("a memória da área de transferência não pôde ser reservada".to_string());
            }
            let ptr = GlobalLock(handle);
            if ptr.is_null() {
                GlobalFree(handle);
                return Err("a memória da área de transferência não pôde ser bloqueada".to_string());
            }
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.cast::<u16>(), bytes.len());
            GlobalUnlock(handle);
            // The clipboard owns the memory now.
            if SetClipboardData(CF_UNICODETEXT, handle).is_null() {
                GlobalFree(handle);
                return Err("o caminho não pôde ir para a área de transferência".to_string());
            }
            Ok(())
        })();
        CloseClipboard();
        result
    }
}

/// What the clipboard holds.
///
/// The answer is a **kind** and not a sentence: the user has to be told
/// which of the two things went wrong in the language the user reads,
/// and the seam does not know that language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Clipboard {
    /// Text, and the text.
    Text(String),
    /// Something that is not text: a bitmap, a list of files.
    NotText,
    /// Another program is holding the clipboard.
    Busy,
}

/// The text on the clipboard.
///
/// The same three calls the copy uses, in the other direction: open it,
/// read it, close it.
///
/// The text is not always there. The clipboard can hold a bitmap, a list
/// of files, or nothing at all — and each of those is an answer, not a
/// failure: the query stays as it is and the user is told which of them
/// it was.
pub fn paste_text() -> Clipboard {
    unsafe {
        if OpenClipboard(core::ptr::null_mut()) == 0 {
            return Clipboard::Busy;
        }
        let result = (|| {
            let handle = GetClipboardData(CF_UNICODETEXT);
            if handle.is_null() {
                return Clipboard::NotText;
            }
            let ptr = GlobalLock(handle);
            if ptr.is_null() {
                return Clipboard::NotText;
            }
            // The size is a **bound**, not the length: the text is
            // NUL-terminated, and a bound that is not a length is what
            // keeps a clipboard another program wrote from being read
            // past its end.
            let units = GlobalSize(handle) / 2;
            let slice = core::slice::from_raw_parts(ptr.cast::<u16>(), units);
            let end = slice.iter().position(|&unit| unit == 0).unwrap_or(units);
            let text = String::from_utf16_lossy(&slice[..end]);
            GlobalUnlock(handle);
            Clipboard::Text(text)
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
    fn GetClipboardData(uFormat: u32) -> HANDLE;
    fn GlobalSize(hMem: HANDLE) -> usize;
    fn GlobalAlloc(uFlags: u32, dwBytes: usize) -> HANDLE;
    fn GlobalLock(hMem: HANDLE) -> *mut c_void;
    fn GlobalUnlock(hMem: HANDLE) -> i32;
    fn GlobalFree(hMem: HANDLE) -> HANDLE;
}

use core::ffi::c_void;

const SW_SHOWNORMAL: i32 = 1;
const CF_UNICODETEXT: u32 = 13;
const GMEM_MOVEABLE: u32 = 0x0002;
