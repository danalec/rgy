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
//! The words themselves live in the vocabulary — one table, one column
//! per language — and every call that can fail is told which language to
//! fail in. The clipboard is the exception: it answers with a **kind**,
//! and the caller turns the kind into a word, because the kind is what
//! the caller acts on and the word is what the user reads.
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

use rgy_launcher::text::{Lang, Text};

use crate::win::*;

/// Open a path with the platform's default program.
///
/// `ShellExecuteW` with the `open` verb: the same call a file manager
/// makes, so the answer is the user's own association and not a guess.
///
/// The language is the one the user reads: the sentence the call fails
/// with is the vocabulary's, and the vocabulary has a column per
/// language.
pub fn open(path: &str, lang: Lang) -> Result<(), String> {
    shell(path, "open", lang)
}

/// Open the folder that holds a path, with the file selected in it.
///
/// The `explore` verb is what selects the file; a launcher that opened
/// the folder without the file would make the user hunt for the one they
/// picked.
pub fn reveal(path: &str, lang: Lang) -> Result<(), String> {
    shell(path, "explore", lang)
}

/// One shell call.
fn shell(path: &str, verb: &str, lang: Lang) -> Result<(), String> {
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
            return Err(could_not_open(path, r as isize, lang));
        }
    }
    Ok(())
}

/// The sentence the launcher says when the platform refused to open a path.
///
/// The word is a template with two holes in it, and the holes are filled
/// in the order they come: the path first and the platform's own code
/// second. The template is split instead of searched for its marks
/// because a path can hold the mark itself, and a path that does would
/// put the sentence out of shape.
fn could_not_open(path: &str, code: isize, lang: Lang) -> String {
    let mut parts = Text::for_lang(lang).could_not_open.split("{}");
    let head = parts.next().unwrap_or_default();
    let middle = parts.next().unwrap_or_default();
    let tail = parts.next().unwrap_or_default();
    format!("{head}{path}{middle}{code}{tail}")
}

/// Put a path on the clipboard.
///
/// The clipboard is a global, and it is taken with the three calls the
/// platform documents: open it, put the data in it, close it. The data
/// is a piece of global memory the clipboard takes over when it is given
/// to `SetClipboardData`.
///
/// Every failure says which one it was, in the language the user reads:
/// a refusal that does not say why is a refusal the user cannot act on.
pub fn copy_path(path: &str, lang: Lang) -> Result<(), String> {
    let words = Text::for_lang(lang);
    unsafe {
        if OpenClipboard(core::ptr::null_mut()) == 0 {
            return Err(words.clipboard_is_busy.to_string());
        }
        let result = (|| {
            if EmptyClipboard() == 0 {
                return Err(words.clipboard_not_emptied.to_string());
            }
            let bytes: Vec<u16> = path.encode_utf16().chain(core::iter::once(0)).collect();
            let size = bytes.len() * 2;
            let handle = GlobalAlloc(GMEM_MOVEABLE, size);
            if handle.is_null() {
                return Err(words.clipboard_not_reserved.to_string());
            }
            let ptr = GlobalLock(handle);
            if ptr.is_null() {
                GlobalFree(handle);
                return Err(words.clipboard_not_locked.to_string());
            }
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.cast::<u16>(), bytes.len());
            GlobalUnlock(handle);
            // The clipboard owns the memory now.
            if SetClipboardData(CF_UNICODETEXT, handle).is_null() {
                GlobalFree(handle);
                return Err(words.path_not_copied.to_string());
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

const SW_SHOWNORMAL: i32 = 1;
const CF_UNICODETEXT: u32 = 13;
const GMEM_MOVEABLE: u32 = 0x0002;

#[cfg(test)]
mod tests {
    use super::*;

    /// A busy clipboard says so, in the language it was asked for.
    ///
    /// The clipboard is shared, and another program holding it is the
    /// failure a test can ask for on purpose. The sentence is the
    /// vocabulary's, and the vocabulary has a column per language: a
    /// refusal that says "busy" in English to a user who reads
    /// Portuguese is a refusal the user cannot act on.
    #[test]
    fn a_busy_clipboard_says_it_in_the_language_it_was_asked_for() {
        extern crate std;
        use std::sync::mpsc;

        // The clipboard is opened with a **window** as its owner, and the
        // owner is what makes it busy: a clipboard opened without one
        // belongs to the whole task, and the task that owns it is
        // allowed to open it again.
        let window = crate::window::Window::new("rgxy-test").expect("the window was made");
        let owner = window.hwnd() as usize;
        let (held_tx, held_rx) = mpsc::channel::<bool>();
        let (free_tx, free_rx) = mpsc::channel::<()>();
        let holder = std::thread::spawn(move || unsafe {
            let opened = OpenClipboard(owner as HWND) != 0;
            let _ = held_tx.send(opened);
            if opened {
                // The clipboard is held until the test is done with it:
                // a clipboard left open is a clipboard the machine does
                // not have.
                let _ = free_rx.recv();
                CloseClipboard();
            }
        });
        let _ = held_rx.recv().unwrap_or(false);
        let english = copy_path("C:/a.txt", Lang::English);
        let portuguese = copy_path("C:/a.txt", Lang::Portuguese);
        // The clipboard goes back before the assertions: a failure here
        // must not leave the machine without one.
        let _ = free_tx.send(());
        let _ = holder.join();

        assert_eq!(
            english.expect_err("the clipboard was busy"),
            Text::for_lang(Lang::English).clipboard_is_busy
        );
        assert_eq!(
            portuguese.expect_err("the clipboard was busy"),
            Text::for_lang(Lang::Portuguese).clipboard_is_busy
        );
    }

    /// The sentence the launcher refuses with names the path and the
    /// platform's own code.
    ///
    /// The two holes of the template are filled in the order they come,
    /// and the path is one of them: a refusal that does not say what it
    /// could not open is a refusal the user cannot act on.
    #[test]
    fn the_refusal_names_the_path_and_the_code() {
        let reason = could_not_open("C:/a.txt", 2, Lang::English);
        assert_eq!(reason, "C:/a.txt could not be opened (2)", "{reason}");

        let reason = could_not_open("C:/a.txt", 2, Lang::Portuguese);
        assert_eq!(reason, "C:/a.txt não pôde ser aberto (2)", "{reason}");
    }
}
