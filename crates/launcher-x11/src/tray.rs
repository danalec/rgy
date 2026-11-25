// SPDX-License-Identifier: Apache-2.0

//! The XEmbed system-tray icon.
//!
//! The notification area on X11 is a **selection** (`_NET_SYSTEM_TRAY_S<n>`)
//! owned by the shell, not a window the launcher draws into. A client asks
//! the owner to dock a small window of its own, and the shell embeds that
//! window and draws it. So the tray here is an X11 window the tree owns:
//! the tree computes the magnifier's pixels and paints them, the tree
//! decides what a click means, and Xlib is only the carrier.
//!
//! The mark is the same magnifying glass the Windows tray draws, from
//! [`crate::mark`]: one set of bytes, two platforms, one search box.
//!
//! # A click is a Show, never a Toggle
//!
//! The icon's left button answers [`Event::Show`] and never
//! [`Event::Toggle`], and a double click is two such answers. A toggle
//! would show the window on the first click and hide it again on the
//! second — the user asked the icon to bring the window up, not to play
//! with it. The **hotkey** owns the toggle; the **icon** owns the show.
//!
//! # The menu is a machine the user can dismiss
//!
//! The right button opens a small self-drawn popup: an override-redirect
//! window with three rows, a pointer grab, and a bounded local event loop.
//! There is no toolkit and no menu widget; the tree draws the rows with
//! [`crate::paint::text`] and reads the choice out of the pointer's `y`.
//!
//! # `no_std`
//!
//! This module is compiled only on `target_os = "linux"` (see `lib.rs`),
//! and it uses `alloc`, `core` and the [`crate::x`] seam. The pure
//! decisions — which command an item means, which row a `y` names — are
//! ordinary functions with ordinary tests.

use alloc::format;
use alloc::string::{String, ToString};
use core::ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_void};
use core::sync::atomic::{AtomicU64, Ordering};

use rgy_launcher::text::Text;

use crate::hotkey::Spin;
use crate::types::Event;
use crate::{mark, runtime, x};

/// The icon's XID, so the free [`on_event`] can recognise its events.
///
/// `Tray::new` stores it once; `Drop` clears it. An atomic is the honest
/// word for one number shared between the window pump and the tray, and
/// it is the only word that needs no `std` mutex.
static TRAY_WINDOW: AtomicU64 = AtomicU64::new(0);

/// The popup menu's XID while it is open, `0` when it is closed.
///
/// [`on_event`] is a free function and cannot reach the `Tray` value, so
/// it records the menu here; `Drop` asks this too, in case the process
/// goes away while a menu is still up.
static TRAY_MENU: AtomicU64 = AtomicU64::new(0);

/// The three labels of the popup, in the session's language.
///
/// [`Tray::new`] copies them out of the [`Text`] it was handed, so the
/// free `on_event` — which has no language of its own — still draws the
/// words the user reads. A `Spin` keeps the three pointers without `std`.
static MENU_LABELS: Spin<[&'static str; 3]> = Spin::new(["Search", "Settings", "Quit"]);

/// What the menu's items do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Open the window — the same thing the hotkey does, but a Show.
    Search,
    /// Open the window with the settings panel open.
    Settings,
    /// End the program.
    Quit,
}

/// The event a menu command means.
///
/// A pure mapping, kept separate from the window machinery so that the
/// three answers can be asserted without opening a display.
pub fn menu_event(command: Command) -> Event {
    match command {
        Command::Search => Event::Show,
        Command::Settings => Event::Settings,
        Command::Quit => Event::Quit,
    }
}

/// Which of the three 30-pixel rows a pointer `y` names, clamped.
///
/// The first row starts at `y = 1`; a click above the menu or below it
/// still names a row the user can see, rather than an index that walks
/// off the end of the array. Factored out of the menu loop so the rule
/// is testable on the host that has no X server.
fn row_of(y: i32) -> usize {
    let row = (y - 1) / 30;
    if row < 0 {
        0
    } else if row > 2 {
        2
    } else {
        row as usize
    }
}

/// The icon the launcher leaves in the system tray.
pub struct Tray {
    /// The XEmbed window the shell embeds: the icon's own XID.
    icon: u64,
    /// The tip the shell shows when the pointer rests on the icon.
    tip: String,
    /// The X connection the icon and its menu live on.
    display: *mut c_void,
    /// The popup menu's XID, `0` when it is closed.
    menu: u64,
}

impl Tray {
    /// Dock an icon in the session's system tray.
    ///
    /// `runtime::display()` is the one X connection the window owns; a
    /// null one is not a tray the launcher can build. The tray selection
    /// is `_NET_SYSTEM_TRAY_S<screen>` and is asked of `XDefaultScreen`:
    /// this launcher is a single-screen client, and screen `0` is the
    /// screen a session tray serves — so the exact name is
    /// `_NET_SYSTEM_TRAY_S0` on the sessions this crate is built for.
    ///
    /// A session with **no tray owner** is not an error that stops the
    /// launcher: the hotkey is the face in the meantime. The reason is
    /// returned so the caller can keep it in `Window::tray_error`.
    ///
    /// The atoms are interned as they are needed: the tray selection and
    /// its opcode, `_XEMBED_INFO`, `_NET_WM_NAME` and `UTF8_STRING`.
    /// `_XEMBED` and `WM_DELETE_WINDOW` belong to the fuller protocol;
    /// this launcher only docks and is destroyed with the process, so it
    /// does not send them.
    pub fn new(text: &Text) -> Result<Tray, String> {
        let display = runtime::display();
        if display.is_null() {
            return Err("the X display is not open".to_string());
        }
        let root = runtime::root();

        let screen = unsafe { x::XDefaultScreen(display) };
        let selection_name = format!("_NET_SYSTEM_TRAY_S{screen}\0");
        let tray_selection = intern(display, selection_name.as_bytes());
        let owner = unsafe { x::XGetSelectionOwner(display, tray_selection) };
        if owner == 0 {
            // The launcher still runs: the caller keeps this reason and
            // the hotkey remains the way to open the window.
            return Err("the system tray has no owner".to_string());
        }

        let opcode = intern(display, b"_NET_SYSTEM_TRAY_OPCODE\0");
        let xembed_info = intern(display, b"_XEMBED_INFO\0");

        // A 24x24 window on the root, with the parent's depth, visual
        // and attributes. The shell reparents it into the tray.
        let icon = unsafe { x::XCreateSimpleWindow(display, root, 0, 0, 24, 24, 0, 0, 0) };
        if icon == 0 {
            return Err("the tray icon window could not be made".to_string());
        }

        // `_XEMBED_INFO` is (version, flags) as two 32-bit `long`s:
        // version 0 and `XEMBED_MAPPED` (bit 0). The property's own atom
        // is its type, as the XEmbed protocol says.
        let info: [c_long; 2] = [0, 1 << 0];
        unsafe {
            x::XChangeProperty(
                display,
                icon,
                xembed_info,
                xembed_info,
                32,
                x::PropModeReplace,
                info.as_ptr().cast::<u8>(),
                2,
            );
            // The masks are `i64` and `c_long` is `i64` on this LP64
            // ABI, so the OR needs no cast to fit `XSelectInput`.
            x::XSelectInput(
                display,
                icon,
                x::ButtonPressMask | x::ExposureMask | x::StructureNotifyMask,
            );
        }

        // SYSTEM_TRAY_REQUEST_DOCK: one client message, straight to the
        // selection owner, with the icon's XID in the third `long`.
        let data: [c_long; 5] = [x::CurrentTime as c_long, 0, icon as c_long, 0, 0];
        let mut ev = x::XEvent::default();
        x::set_client_message(&mut ev, owner, opcode, 32, &data);
        unsafe {
            x::XSendEvent(display, owner, 0, 0, &mut ev);
            x::XFlush(display);
        }

        TRAY_WINDOW.store(icon, Ordering::Release);
        draw_icon(display, icon);
        write_tip(display, icon, text.tray_tip);
        *MENU_LABELS.lock().get() = [text.tray_search, text.tray_settings, text.tray_quit];

        Ok(Tray {
            icon,
            tip: text.tray_tip.to_string(),
            display,
            menu: 0,
        })
    }

    /// Change the tip the shell shows when the pointer rests on the icon.
    ///
    /// The tip is a word the user reads, and the words are the product's.
    /// Setting `_NET_WM_NAME` with `UTF8_STRING` on the icon is best
    /// effort: a tray that ignores it still shows the window's own name,
    /// and the stored tip is what the launcher answered with either way.
    pub fn set_tip(&mut self, tip: &str) {
        self.tip = tip.to_string();
        write_tip(self.display, self.icon, &self.tip);
    }

    /// The icon's XID.
    ///
    /// The window pump recognises the icon's events through the module's
    /// own static, but the XID is the tray's answer to "which window is
    /// my face" — a question a reader of the interface can ask.
    #[allow(dead_code)]
    pub fn window(&self) -> u64 {
        self.icon
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        // A menu still open when the tray goes away is destroyed too:
        // Quit is one of its items, so the tray can be dropped while the
        // loop that opened the menu is still unwinding.
        let open = TRAY_MENU.swap(0, Ordering::AcqRel);
        let menu = if self.menu != 0 { self.menu } else { open };
        if !self.display.is_null() {
            unsafe {
                if menu != 0 {
                    x::XDestroyWindow(self.display, menu);
                }
                let icon = self.window();
                if icon != 0 {
                    x::XDestroyWindow(self.display, icon);
                }
                x::XFlush(self.display);
            }
        }
        TRAY_WINDOW.store(0, Ordering::Release);
    }
}

/// Read an event that belongs to the tray icon.
///
/// Called by the window pump for every event. Only a `ButtonPress` on
/// [`TRAY_WINDOW`] is ours: the left button is a Show, and the right
/// button opens the popup and answers with the item the user picked (or
/// nothing, when the menu was dismissed).
pub(crate) fn on_event(ev: &x::XEvent) -> Option<Event> {
    if x::ev_type(ev) != x::ButtonPress {
        return None;
    }
    if x::ev_window(ev) != TRAY_WINDOW.load(Ordering::Acquire) {
        return None;
    }
    match x::ev_button(ev) {
        1 => Some(Event::Show),
        3 => open_menu().map(menu_event),
        _ => None,
    }
}

/// Open the popup at the pointer and return the item the user chose.
///
/// The window is override-redirect so no window manager decorates it,
/// 180x90 so three 30-pixel rows fit, and a pointer grab so a click
/// anywhere — inside or outside — reaches this loop. The loop is
/// bounded at fifty events: the grab guarantees the user's next click
/// or key arrives, and a bound is the promise that a menu is a machine
/// that ends.
fn open_menu() -> Option<Command> {
    let display = runtime::display();
    if display.is_null() {
        return None;
    }
    let root = runtime::root();

    // Where the pointer is, so the menu opens under it.
    let mut root_ret: x::Window = 0;
    let mut child: x::Window = 0;
    let mut root_x: c_int = 0;
    let mut root_y: c_int = 0;
    let mut win_x: c_int = 0;
    let mut win_y: c_int = 0;
    let mut mask: c_uint = 0;
    let asked = unsafe {
        x::XQueryPointer(
            display,
            root,
            &mut root_ret,
            &mut child,
            &mut root_x,
            &mut root_y,
            &mut win_x,
            &mut win_y,
            &mut mask,
        )
    };
    let (px, py) = if asked != 0 { (root_x, root_y) } else { (0, 0) };

    let width: c_uint = 180;
    let height: c_uint = 90;
    let mut attrs = x::XSetWindowAttributes {
        background_pixel: 0x20_20_20,
        override_redirect: 1,
        event_mask: x::ButtonPressMask | x::KeyPressMask | x::ExposureMask,
        ..x::XSetWindowAttributes::default()
    };
    let menu = unsafe {
        x::XCreateWindow(
            display,
            root,
            px,
            py,
            width,
            height,
            0,
            x::CopyFromParent,
            x::InputOutput,
            core::ptr::null_mut(),
            x::CWBackPixel | x::CWOverrideRedirect | x::CWEventMask,
            &mut attrs,
        )
    };
    if menu == 0 {
        return None;
    }
    TRAY_MENU.store(menu, Ordering::Release);

    unsafe {
        x::XSelectInput(
            display,
            menu,
            x::ButtonPressMask | x::KeyPressMask | x::ExposureMask,
        );
        x::XMapRaised(display, menu);
        x::XGrabPointer(
            display,
            menu,
            0,
            x::ButtonPressMask as c_uint,
            x::GrabModeAsync,
            x::GrabModeAsync,
            x::None,
            x::None,
            x::CurrentTime,
        );
        x::XFlush(display);
    }

    let gc = unsafe { x::XCreateGC(display, menu, 0, core::ptr::null_mut()) };
    if !gc.is_null() {
        let labels = *MENU_LABELS.lock().get();
        // The mark's accent, in the red-first order Xlib's pixel wants.
        let (r, g, b) = mark::ACCENT;
        let rgb = (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
        let accent = rgb as c_ulong;
        unsafe {
            for row in 0..3i32 {
                x::XSetForeground(display, gc, 0x30_30_30);
                x::XFillRectangle(display, menu, gc, 0, row * 30, width, 30);
            }
            // A small accent bar is the highlight: the tree draws the
            // rows, `XFillRectangle` draws the selection, and no toolkit
            // is asked what a menu looks like.
            x::XSetForeground(display, gc, accent);
            x::XFillRectangle(display, menu, gc, 0, 0, 3, height);
            for (index, label) in labels.iter().copied().enumerate() {
                crate::paint::text(
                    display,
                    menu,
                    gc,
                    12,
                    index as i32 * 30 + 20,
                    label,
                    0x00ff_ffff,
                    1,
                );
            }
            x::XFreeGC(display, gc);
        }
    }

    let mut chosen = None;
    for _ in 0..50 {
        let mut next = x::XEvent::default();
        unsafe {
            x::XNextEvent(display, &mut next);
        }
        match x::ev_type(&next) {
            x::ButtonPress => {
                // A click outside the menu is a dismissal; a click
                // inside it picks the row under the pointer.
                if x::ev_window(&next) == menu {
                    chosen = match row_of(x::ev_y(&next)) {
                        0 => Some(Command::Search),
                        1 => Some(Command::Settings),
                        _ => Some(Command::Quit),
                    };
                }
                break;
            }
            x::KeyPress => {
                // Escape dismisses — and so does any other key. A menu
                // that ignores a key the user pressed is a menu that
                // traps them; a dismissal is the safe answer.
                break;
            }
            // Expose, configure and everything else are ignored: this
            // loop waits for a choice, not for a repaint.
            _ => {}
        }
    }

    unsafe {
        x::XUngrabPointer(display, x::CurrentTime);
        x::XDestroyWindow(display, menu);
        x::XFlush(display);
    }
    TRAY_MENU.store(0, Ordering::Release);
    chosen
}

/// Paint the magnifier into the icon window, one pixel at a time.
///
/// [`mark::magnifier`] returns **BGRA**, top row first; Xlib's
/// `XSetForeground` wants a `0xRRGGBB` pixel, so the channels are turned
/// around here, at the boundary, and nowhere else. 24 * 24 = 576 calls
/// of `XSetForeground` + `XFillRectangle` is small, one-shot work at
/// start-up, and it is the honest way to put the tree's own antialiased
/// bytes into a drawable without assuming a visual or an image format.
fn draw_icon(display: *mut c_void, icon: u64) {
    let pixels = mark::magnifier(24);
    let gc = unsafe { x::XCreateGC(display, icon, 0, core::ptr::null_mut()) };
    if gc.is_null() {
        return;
    }
    unsafe {
        for row in 0..pixels.size {
            for column in 0..pixels.size {
                let at = (row as usize * pixels.size as usize + column as usize) * 4;
                if pixels.colour[at + 3] == 0 {
                    continue;
                }
                let b = pixels.colour[at] as c_ulong;
                let g = pixels.colour[at + 1] as c_ulong;
                let r = pixels.colour[at + 2] as c_ulong;
                x::XSetForeground(display, gc, (r << 16) | (g << 8) | b);
                x::XFillRectangle(display, icon, gc, column, row, 1, 1);
            }
        }
        x::XFreeGC(display, gc);
    }
}

/// Put the tip on the icon as `_NET_WM_NAME` / `UTF8_STRING`.
///
/// Best effort and flushed: a tray that does not read it loses only a
/// tooltip, not the launcher.
fn write_tip(display: *mut c_void, icon: u64, tip: &str) {
    if display.is_null() || icon == 0 {
        return;
    }
    let net_wm_name = intern(display, b"_NET_WM_NAME\0");
    let utf8 = intern(display, b"UTF8_STRING\0");
    unsafe {
        x::XChangeProperty(
            display,
            icon,
            net_wm_name,
            utf8,
            8,
            x::PropModeReplace,
            tip.as_ptr(),
            tip.len() as c_int,
        );
        x::XFlush(display);
    }
}

/// Intern a name that carries its own trailing NUL.
///
/// `only_if_exists = 0` creates the atom when the session has not got
/// one yet, which is what a selection owner needs for its own name.
fn intern(display: *mut c_void, name: &[u8]) -> x::Atom {
    unsafe { x::XInternAtom(display, name.as_ptr().cast::<c_char>(), 0) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three items mean three different things, and none of them is
    /// a Toggle: the icon shows, it does not play with the window.
    #[test]
    fn menu_event_maps_all_three_commands() {
        assert_eq!(menu_event(Command::Search), Event::Show);
        assert_eq!(menu_event(Command::Settings), Event::Settings);
        assert_eq!(menu_event(Command::Quit), Event::Quit);
        assert_ne!(menu_event(Command::Search), Event::Toggle);
    }

    /// A row is 30 pixels tall, the first starts at `y = 1`, and a `y`
    /// above or below the menu still names one of the three rows.
    #[test]
    fn row_of_selects_and_clamps() {
        assert_eq!(row_of(1), 0);
        assert_eq!(row_of(30), 0);
        assert_eq!(row_of(31), 1);
        assert_eq!(row_of(60), 1);
        assert_eq!(row_of(61), 2);
        assert_eq!(row_of(90), 2);
        assert_eq!(row_of(1000), 2, "a click below the menu clamps high");
        assert_eq!(row_of(0), 0, "a click above the menu clamps low");
        assert_eq!(row_of(-500), 0, "and so does a negative one");
    }
}
