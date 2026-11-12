// SPDX-License-Identifier: Apache-2.0

//! What the launcher does with the selected result, and with the
//! clipboard.
//!
//! Four actions, and each one is a platform call the tree does not have
//! yet: open a file with its own program, open the folder that holds it,
//! put a path on the clipboard, and take the clipboard's text back.
//! They live here and not in the model because they *are* the machine:
//! the model decides which one the user asked for, and this carries it
//! out.
//!
//! The words these actions fail with are the user's, not the
//! programmer's: they land in the line under the launcher's field, and
//! that line speaks one language. [`could_not_open`] fills the
//! vocabulary's template with the path and the reason, exactly as the
//! Windows backend does.
//!
//! # Opening a folder is not `/select`
//!
//! Explorer's `/select,<path>` opens the folder **and** selects the
//! file. X11 file managers have no portable equivalent, so [`reveal`]
//! opens the containing folder with `xdg-open`. That is the honest
//! difference: the user sees the folder, not a highlighted file.
//!
//! # The clipboard is a selection, not a buffer
//!
//! On X11 the clipboard is an X **selection** named `CLIPBOARD`. A copy
//! takes ownership of the selection and answers `SelectionRequest`
//! events from the window pump; a paste asks the current owner to
//! convert the selection onto a property of our own window and reads
//! that property back. Both directions are bounded and never block for
//! ever, and the tree owns the bytes it serves.
//!
//! # `no_std`
//!
//! `alloc`, `core` and the [`crate::x`] seam. The pure decisions —
//! [`parent_of`], [`reveal_args`], [`could_not_open`] — are ordinary
//! functions with ordinary tests; the X11 bodies are Linux-only.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::{c_char, c_int, c_ulong, c_void};

use rgy_launcher::text::{Lang, Text};

use crate::hotkey::Spin;
use crate::{runtime, x};

/// The bytes the launcher serves to `CLIPBOARD` requestors.
///
/// A spinlock like the hotkey's: one `Vec<u8>`, taken for a section
/// that copies it and never blocks. `copy_path` writes it and
/// [`serve_selection_request`] reads it.
static CLIPBOARD: Spin<Vec<u8>> = Spin::new(Vec::new());

/// What the clipboard holds.
///
/// The answer is a **kind** and not a sentence: the caller turns the
/// kind into the word the user reads, in the language the user reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Clipboard {
    /// Text, and the text.
    Text(String),
    /// Something that is not text: a bitmap, a list of files.
    NotText,
    /// Another program is holding the clipboard, or there is no owner.
    Busy,
}

/// Open a path with the platform's default program.
///
/// `xdg-open` is the desktop's own association: the answer is the
/// user's file manager or handler, not a guess of ours. The language is
/// the one the user reads — the sentence the call fails with is the
/// vocabulary's.
pub fn open(path: &str, lang: Lang) -> Result<(), String> {
    match spawn(&["xdg-open", path]) {
        Ok(()) => Ok(()),
        Err(reason) => Err(could_not_open(path, &reason, lang)),
    }
}

/// The argv of the reveal command, for a path and its parent folder.
///
/// `["xdg-open", parent]`: X11 file managers have no portable
/// `/select`, so the containing folder is what can be opened. `path` is
/// in the signature because [`reveal`] computes the parent from it and
/// the two arguments belong together; the folder is what the command
/// names.
pub fn reveal_args<'a>(path: &'a str, parent: &'a str) -> Vec<&'a str> {
    let _ = path;
    alloc::vec!["xdg-open", parent]
}

/// Open the folder that holds a path.
///
/// The honest difference from Explorer's `/select,<path>`: no file is
/// highlighted, because X11 has no portable way to ask for one. The
/// folder opens, and the user sees the file in it.
pub fn reveal(path: &str, lang: Lang) -> Result<(), String> {
    let parent = parent_of(path);
    let args = reveal_args(path, parent);
    match spawn(&args) {
        Ok(()) => Ok(()),
        Err(reason) => Err(could_not_open(path, &reason, lang)),
    }
}

/// The folder that holds a path.
///
/// Strip everything after the last separator, keeping a leading `/`
/// when the path is at the root. Both `/` and the Windows-style `\` are
/// separators: the wire carries UTF-8 strings and the daemon may hand
/// over a path spelled either way. A path with no separator at all is
/// a name in the current folder, whose spelling here is `.`.
pub fn parent_of(path: &str) -> &str {
    match path.rfind(['/', '\\']) {
        None => ".",
        Some(0) => &path[..1],
        Some(at) => &path[..at],
    }
}

/// The sentence the launcher says when the platform refused to open a
/// path.
///
/// The word is a template with two `{}` holes, filled in the order they
/// come: the path first, the reason second. It is split instead of
/// searched for its marks because a path can hold the mark itself, and
/// a path that did would put the sentence out of shape. Exactly the
/// rule `launcher-windows/src/actions.rs` follows.
pub fn could_not_open(path: &str, reason: &str, lang: Lang) -> String {
    let mut parts = Text::for_lang(lang).could_not_open.split("{}");
    let head = parts.next().unwrap_or_default();
    let middle = parts.next().unwrap_or_default();
    let tail = parts.next().unwrap_or_default();
    format!("{head}{path}{middle}{reason}{tail}")
}

/// Run a program without making the launcher its parent.
///
/// A **double fork**: child 1 forks child 2 and exits at once, so the
/// grandchild is reparented to init and the launcher never accumulates
/// a zombie. There is deliberately no `signal(SIGCHLD, SIG_IGN)` — the
/// double fork is the whole answer, and changing a process-wide signal
/// handler for one `xdg-open` would be a side effect on every other
/// child the launcher may have. The parent waits for child 1 alone.
///
/// `execvp` wants NUL-terminated C strings and a NULL-terminated array
/// of pointers; `Vec<Vec<u8>>` keeps them alive until the child has
/// replaced its image (or exited), and the pointer array lives on the
/// parent's stack across the `waitpid`.
///
/// SAFETY is confined to this function's block: the pointers are into
/// our own `cstrings`, which outlive every call below, and in the
/// forked children the same memory is copied by the kernel.
fn spawn(argv: &[&str]) -> Result<(), String> {
    if argv.is_empty() {
        return Err("there is nothing to run".to_string());
    }
    let cstrings: Vec<Vec<u8>> = argv
        .iter()
        .map(|arg| {
            let mut bytes = Vec::with_capacity(arg.len() + 1);
            bytes.extend_from_slice(arg.as_bytes());
            bytes.push(0);
            bytes
        })
        .collect();
    let mut ptrs: Vec<*const c_char> = cstrings
        .iter()
        .map(|bytes| bytes.as_ptr().cast::<c_char>())
        .collect();
    ptrs.push(core::ptr::null());

    unsafe {
        let child1 = x::fork();
        if child1 < 0 {
            return Err("fork failed".to_string());
        }
        if child1 == 0 {
            let child2 = x::fork();
            if child2 == 0 {
                x::setsid();
                x::execvp(ptrs[0], ptrs.as_ptr());
                // `execvp` only returns when it failed.
                x::_exit(127);
            }
            x::_exit(0);
        }
        let mut status: c_int = 0;
        x::waitpid(child1, &mut status, 0);
    }
    Ok(())
}

/// Put a path on the `CLIPBOARD` selection.
///
/// The bytes are stored first, then the launcher takes ownership of the
/// selection on its own window. If `XGetSelectionOwner` afterwards does
/// not name that window, another client won the race and the answer is
/// `clipboard_is_busy` in the language the user reads. The window pump
/// forwards `SelectionRequest` events to [`serve_selection_request`].
pub fn copy_path(path: &str, lang: Lang) -> Result<(), String> {
    let display = runtime::display();
    if display.is_null() {
        return Err(Text::for_lang(lang).clipboard_is_busy.to_string());
    }
    *CLIPBOARD.lock().get() = path.as_bytes().to_vec();
    let selection = intern(display, b"CLIPBOARD\0");
    // `UTF8_STRING` is interned for the requests we will answer; the
    // target the requestor chose is the type we put on its property.
    intern(display, b"UTF8_STRING\0");
    let owner = runtime::window();
    unsafe {
        x::XSetSelectionOwner(display, selection, owner, x::CurrentTime);
        x::XFlush(display);
        if x::XGetSelectionOwner(display, selection) != owner {
            return Err(Text::for_lang(lang).clipboard_is_busy.to_string());
        }
    }
    Ok(())
}

/// Build a `SelectionNotify` event by filling `XEvent.data` directly.
///
/// Xlib writes the union whole, so the words are placed where its own
/// `XSelectionEvent` has them: type at word 0, serial at 1, send_event
/// at 2, the display pointer at 3, requestor at 4, selection at 5,
/// target at 6, property at 7 and time at 9 (word 8 is the same time
/// on the real layout — both are set so either reader agrees). The
/// display comes from the runtime: `serve_selection_request` has no
/// display argument of its own.
pub(crate) fn selection_notify_event(
    requestor: u64,
    selection: u64,
    target: u64,
    property: u64,
    time: u64,
) -> x::XEvent {
    let mut ev = x::XEvent::default();
    ev.data[0] = x::SelectionNotify as c_ulong;
    ev.data[1] = 0;
    ev.data[2] = 1;
    ev.data[3] = runtime::display() as usize as c_ulong;
    ev.data[4] = requestor;
    ev.data[5] = selection;
    ev.data[6] = target;
    ev.data[7] = property;
    ev.data[8] = time;
    ev.data[9] = time;
    ev
}

/// Answer one `SelectionRequest` for the clipboard we own.
///
/// The property is the request's own when it named one, otherwise the
/// target. A `UTF8_STRING`, `XA_STRING` or `TEXT` target gets the bytes
/// via `XChangeProperty` and a notification that names the property;
/// any other target — and any request while we do not own the
/// selection — is refused with a zero property. `XSendEvent` delivers
/// the answer to the requestor directly.
pub(crate) fn serve_selection_request(ev: &x::XEvent) {
    let display = runtime::display();
    if display.is_null() {
        return;
    }
    let selection = intern(display, b"CLIPBOARD\0");
    let utf8 = intern(display, b"UTF8_STRING\0");
    let text = intern(display, b"TEXT\0");

    let requestor = x::ev_selection_requestor(ev);
    let asked = x::ev_selection_selection(ev);
    let target = x::ev_selection_target(ev);
    let requested_property = x::ev_selection_property(ev);
    let time = x::ev_selection_time(ev);
    let property = if requested_property != 0 {
        requested_property
    } else {
        target
    };

    let owned = unsafe { x::XGetSelectionOwner(display, selection) } == runtime::window();
    let mut notice = selection_notify_event(requestor, asked, target, property, time);

    if owned && (target == utf8 || target == x::XA_STRING || target == text) {
        let bytes = CLIPBOARD.lock().get().clone();
        unsafe {
            x::XChangeProperty(
                display,
                requestor,
                property,
                target,
                8,
                x::PropModeReplace,
                bytes.as_ptr(),
                bytes.len() as c_int,
            );
        }
        notice.data[7] = property;
    } else {
        notice.data[7] = 0;
    }

    unsafe {
        x::XSendEvent(display, requestor, 0, 0, &mut notice);
        x::XFlush(display);
    }
}

/// Read the clipboard's text back.
///
/// `XConvertSelection` asks the owner to put a UTF-8 answer on our
/// private `_RGY_CLIP` property; the wait is bounded to 200 short
/// `poll`s on the X connection's fd and only `SelectionNotify` events
/// with that property are acted on — every other event is ignored, so
/// the nested loop cannot swallow the pump's own traffic.
///
/// A selection with **no owner** is `Busy`: nobody can answer a
/// conversion request that has no destination. A conversion that
/// produced no bytes, or no notification at all within the bound, is
/// `NotText`: the query stays as it is and the user is told.
pub fn paste_text() -> Clipboard {
    let display = runtime::display();
    if display.is_null() {
        return Clipboard::Busy;
    }
    let selection = intern(display, b"CLIPBOARD\0");
    let utf8 = intern(display, b"UTF8_STRING\0");
    let clip_prop = intern(display, b"_RGY_CLIP\0");
    let win = runtime::window();

    let owner = unsafe { x::XGetSelectionOwner(display, selection) };
    if owner == 0 {
        return Clipboard::Busy;
    }
    unsafe {
        x::XConvertSelection(display, selection, utf8, clip_prop, win, x::CurrentTime);
        x::XFlush(display);
    }

    let fd = unsafe { x::XConnectionNumber(display) };
    for _ in 0..200 {
        let mut pfd = x::pollfd {
            fd,
            events: x::POLLIN,
            revents: 0,
        };
        let ready = unsafe { x::poll(&mut pfd, 1, 20) };
        if ready <= 0 {
            continue;
        }
        while unsafe { x::XPending(display) } > 0 {
            let mut next = x::XEvent::default();
            unsafe {
                x::XNextEvent(display, &mut next);
            }
            if x::ev_type(&next) == x::SelectionNotify && x::ev_notify_property(&next) == clip_prop
            {
                return read_property(display, win, clip_prop);
            }
            // Ignore every other event: this loop waits for one answer.
        }
    }
    Clipboard::NotText
}

/// Read the converted property off our window and free Xlib's buffer.
fn read_property(display: *mut c_void, win: u64, prop: u64) -> Clipboard {
    let mut format_out: c_int = 0;
    let mut type_out: x::Atom = 0;
    let mut nitems: c_ulong = 0;
    let mut after: c_ulong = 0;
    let mut data: *mut u8 = core::ptr::null_mut();
    unsafe {
        x::XGetWindowProperty(
            display,
            win,
            prop,
            0,
            0x1fff_ffff,
            0,
            0,
            &mut format_out,
            &mut type_out,
            &mut nitems,
            &mut after,
            &mut data,
        );
        if data.is_null() || nitems == 0 {
            if !data.is_null() {
                x::XFree(data.cast::<c_void>());
            }
            x::XDeleteProperty(display, win, prop);
            return Clipboard::NotText;
        }
        let bytes = core::slice::from_raw_parts(data, nitems as usize).to_vec();
        x::XFree(data.cast::<c_void>());
        x::XDeleteProperty(display, win, prop);
        Clipboard::Text(String::from_utf8_lossy(&bytes).into_owned())
    }
}

/// Intern a name that carries its own trailing NUL.
fn intern(display: *mut c_void, name: &[u8]) -> x::Atom {
    unsafe { x::XInternAtom(display, name.as_ptr().cast::<c_char>(), 0) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path's parent is the folder, with a leading `/` kept at the
    /// root and Windows-style `\` treated as a separator too.
    #[test]
    fn parent_of_strips_the_last_separator() {
        assert_eq!(parent_of("/a/b/c.txt"), "/a/b");
        assert_eq!(parent_of("/c.txt"), "/");
        assert_eq!(parent_of("C:\\a\\b.txt"), "C:\\a");
        assert_eq!(parent_of("report.txt"), ".");
        assert_eq!(parent_of("a/b"), "a");
    }

    /// The reveal command names the containing folder and nothing but
    /// `xdg-open` and that folder.
    #[test]
    fn reveal_args_opens_the_containing_folder() {
        let args = reveal_args("/a/b/c.txt", "/a/b");
        assert_eq!(args, alloc::vec!["xdg-open", "/a/b"]);
        assert!(args.contains(&"/a/b"), "the parent is in the argv");
        assert_eq!(args.len(), 2);
    }

    /// The refusal names the path and the reason, in both languages —
    /// the same sentence the Windows backend says, out of the same
    /// vocabulary.
    #[test]
    fn could_not_open_fills_the_template_in_both_languages() {
        assert_eq!(
            could_not_open("C:/a.txt", "2", Lang::English),
            "C:/a.txt could not be opened (2)"
        );
        assert_eq!(
            could_not_open("C:/a.txt", "2", Lang::Portuguese),
            "C:/a.txt não pôde ser aberto (2)"
        );
    }

    /// A path that holds the template's own mark does not put the
    /// sentence out of shape: split, not replace, is the rule.
    #[test]
    fn a_path_with_the_mark_keeps_the_sentences_shape() {
        let reason = could_not_open("weird{}name", "why", Lang::English);
        assert_eq!(reason, "weird{}name could not be opened (why)");
    }
}
