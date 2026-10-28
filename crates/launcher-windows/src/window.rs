// SPDX-License-Identifier: Apache-2.0

//! The window: a compact overlay that appears over whatever is on screen.
//!
//! It is a plain Win32 popup — no window manager, no toolkit, no runtime —
//! because the launcher has to be *there* in a few milliseconds after the
//! key, and every layer between the key and the pixels is a layer that can
//! be slow.
//!
//! # What it looks like
//!
//! | | |
//! |---|---|
//! | shape | a rounded rectangle, 12 px of radius |
//! | backdrop | the Win11 host backdrop (acrylic), and a near-black tint over it |
//! | shadow | the system's own drop shadow |
//! | type | Segoe UI Variable Text, and a lighter weight for the query |
//!
//! # How it behaves
//!
//! The window never takes the focus away from the window underneath for
//! longer than it is on screen, and it is always above: `WS_EX_TOPMOST`
//! and `WS_EX_TOOLWANDOW`, so it is not in the taskbar and not in the
//! alt-tab list. The keyboard focus it *does* take is the price of a
//! search box, and it gives it back the moment it is hidden.

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_void;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU16, Ordering};

use rgy_launcher::{Action, Layout, Mode, Preview, Row};

use crate::paint::{self, Fonts, Icons};
use crate::win::*;

/// The window's class name. One window, one class, one registration.
const CLASS_NAME: &[u16] = &[
    b'r' as u16,
    b'g' as u16,
    b'y' as u16,
    b'x' as u16,
    b'y' as u16,
    b'W' as u16,
    b'i' as u16,
    b'n' as u16,
    b'd' as u16,
    b'o' as u16,
    b'w' as u16,
    0,
];

/// The message the hotkey posts, so the window wakes up and shows itself.
pub const WM_TOGGLE: UINT = WM_APP + 1;

/// The timer the debounce rides on.
const TIMER_TICK: usize = 1;

/// How often the window ticks while it is up.
///
/// A sixth of a second: long enough that a burst of typing is one search,
/// short enough that the answer feels like it came with the typing.
const TICK_MS: u32 = 120;

/// A key, named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A character the user typed.
    Char(char),
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Enter,
    /// Ctrl+Enter: open the folder that holds the result.
    CtrlEnter,
    /// Ctrl+C: copy the path.
    CtrlC,
    /// Ctrl+P: preview.
    CtrlP,
    /// Ctrl+K: the actions menu.
    CtrlK,
    /// Ctrl+W: delete the word.
    CtrlW,
    /// Ctrl+U: clear the query.
    CtrlU,
    Esc,
    Tab,
    Other,
}

/// What the window says to the world.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A key was pressed.
    Key(Key),
    /// The window's size changed.
    Resize,
    /// The hotkey fired: show the window, or hide it if it is already up.
    Toggle,
    /// The window was closed.
    Quit,
    /// The clock ticked: the host decides whether the typing has settled.
    Tick,
}

/// What the window draws.
///
/// It owns its strings rather than borrowing them: the window's procedure
/// is called by the system, not by the host, and a borrow that outlived
/// the call would be a lie about who owns what.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct View {
    pub query: String,
    pub placeholder: String,
    pub rows: Vec<Row>,
    pub selected: usize,
    pub top: usize,
    pub mode: Mode,
    pub menu: Vec<Action>,
    /// Which action of the menu is selected.
    pub menu_selected: usize,
    pub warnings: Vec<String>,
    /// The one line under the field: what is happening.
    pub status: String,
    /// The panel over the results, when there is one.
    pub preview: Option<Preview>,
}

/// What the host says the window should do about the event it was given.
///
/// The window owns itself, and the host owns the state: the two talk
/// through this, and the window does what it is told. A host that reached
/// into the window instead would have to borrow it while the window is
/// running the loop that calls it — and the borrow checker is right about
/// that one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Control {
    /// Nothing: the window has done what the event asked.
    None,
    /// Show the window, with this view, at this size.
    Show(View, i32, i32),
    /// Hide the window. The process stays.
    Hide,
    /// Repaint, with this view, at this size.
    Paint(View, i32, i32),
    /// End the loop.
    Quit,
}

/// The state the window's procedure reads.
struct State {
    view: View,
    /// The geometry the last paint used, so a resize knows what changed.
    painted: (i32, i32),
    /// Whether the platform really gave the window its blur.
    ///
    /// The blur is behind the window, so the painting has to leave the
    /// client area alone where there is one — and where there is not,
    /// the window has to paint its own near-black.
    backdrop: bool,
    /// The icons, one per kind, resolved the first time they are needed.
    icons: Icons,
    /// The window's fonts, made once and deleted with the window.
    fonts: Fonts,
}

/// The window.
pub struct Window {
    hwnd: HWND,
    /// The state, as a pointer and not as a box.
    ///
    /// The window procedure reaches the state through the window's own
    /// user data, and this is the same pointer: one path to the object,
    /// one provenance, and no borrow that outlives the call that made
    /// it. A box here would be a second path, and two live `&mut` to the
    /// same object are what the compiler is free to assume cannot
    /// happen.
    state: NonNull<State>,
    width: i32,
    height: i32,
}

impl Window {
    /// Build the window, hidden.
    ///
    /// It is not shown until [`Window::show`]: a window that appears and
    /// disappears at start-up is a window the user did not ask for.
    pub fn new(title: &str) -> Result<Window, String> {
        register_class()?;
        let mut title16: Vec<u16> = title.encode_utf16().collect();
        title16.push(0);

        // The state is a heap allocation and the window's user data
        // holds the pointer to it: the window procedure reaches the
        // state there, and the window reaches it through the same
        // pointer.
        let raw: *mut State = Box::into_raw(Box::new(State {
            view: View {
                placeholder: "Buscar arquivos, apps ou comandos…".to_string(),
                ..View::default()
            },
            painted: (0, 0),
            backdrop: false,
            icons: Icons::new(),
            fonts: Fonts::new(),
        }));
        let state = match NonNull::new(raw) {
            Some(state) => state,
            None => {
                // The allocation is ours and nobody else has it.
                // SAFETY: the pointer came from `Box::into_raw` and was
                // never given to anyone else.
                unsafe {
                    drop(Box::from_raw(raw));
                }
                return Err("the window's state could not be allocated".to_string());
            }
        };

        unsafe {
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                CLASS_NAME.as_ptr(),
                title16.as_ptr(),
                WS_POPUP,
                0,
                0,
                600,
                80,
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                GetModuleHandleW(core::ptr::null()),
                state.as_ptr().cast::<c_void>(),
            );
            if hwnd.is_null() {
                // The state was ours and nobody else has it now.
                drop(Box::from_raw(state.as_ptr()));
                return Err("the window could not be created".to_string());
            }
            SetWindowLongPtrW(hwnd, GWL_USERDATA, state.as_ptr() as isize);
            let mut w = Window {
                hwnd,
                state,
                width: 600,
                height: 80,
            };
            // The backdrop says what the painting has to do: with one, the
            // client area is left alone for the blur to show through.
            let backdrop = w.apply_backdrop();
            w.state_mut().backdrop = backdrop;
            w.apply_region();
            Ok(w)
        }
    }

    /// The state, read.
    ///
    /// The pointer is the window's own: it is the one the window put in
    /// its user data, and the allocation lives as long as the window
    /// does.
    fn state(&self) -> &State {
        // SAFETY: the pointer is the one this window put in its own user
        // data, and the allocation is alive for as long as the window is.
        unsafe { &*self.state.as_ptr() }
    }

    /// The state, written.
    fn state_mut(&mut self) -> &mut State {
        // SAFETY: the pointer is the one this window put in its own user
        // data, and the allocation is alive for as long as the window is;
        // `&mut self` says that the caller is the only one who can reach
        // the window, and so the only one who can reach the state.
        unsafe { &mut *self.state.as_ptr() }
    }

    /// Start the clock.
    ///
    /// The window ticks while it is up and stops when it is hidden: a
    /// hidden window that ticks is a process that never sleeps.
    pub fn start_clock(&self) {
        unsafe {
            SetTimer(self.hwnd, TIMER_TICK, TICK_MS, core::ptr::null_mut());
        }
    }

    /// Stop the clock.
    pub fn stop_clock(&self) {
        unsafe {
            KillTimer(self.hwnd, TIMER_TICK);
        }
    }

    /// The window's handle, for the hook to post to.
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// How tall the work area is, in pixels.
    ///
    /// The host needs it to know how many rows the window can show: the
    /// model sizes the window from the rows, and the rows have to fit
    /// between the top of the work area and the bottom of the screen.
    /// The work area and not the screen, because the taskbar is not room
    /// for rows.
    ///
    /// The answer is never zero: a host that read zero would show a
    /// single row.
    pub fn work_area_height(&self) -> u32 {
        let work = self.target_work_area();
        let height = work.bottom - work.top;
        if height > 0 {
            return height as u32;
        }
        // The platform would not say, and the screen is the answer that
        // is always there.
        unsafe { GetSystemMetrics(SM_CYSCREEN).max(0) as u32 }
    }

    /// What the window draws.
    pub fn view(&self) -> &View {
        &self.state().view
    }

    /// What the window draws, changed.
    pub fn set_view(&mut self, view: View) {
        self.state_mut().view = view;
    }

    /// Show the window, over everything, without raising what is under it.
    ///
    /// The window is shown with `SW_SHOWNOACTIVATE` so the window
    /// underneath is not raised or restored — the desktop the user was
    /// looking at is still there, untouched — and the focus is taken
    /// afterwards, because a search box that cannot be typed into is not a
    /// search box.
    pub fn show(&mut self) {
        unsafe {
            self.place();
            ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            SetForegroundWindow(self.hwnd);
            SetFocus(self.hwnd);
            UpdateWindow(self.hwnd);
            self.start_clock();
        }
    }

    /// Hide the window. The process stays.
    pub fn hide(&self) {
        unsafe {
            ShowWindow(self.hwnd, SW_HIDE);
        }
        self.stop_clock();
    }

    /// Whether the window is on the screen.
    pub fn is_visible(&self) -> bool {
        unsafe { IsWindowVisible(self.hwnd) != 0 }
    }

    /// Repaint the window.
    pub fn repaint(&self) {
        unsafe {
            InvalidateRect(self.hwnd, core::ptr::null_mut(), 0);
            UpdateWindow(self.hwnd);
        }
    }

    /// Ask for the window to show itself, from another thread.
    ///
    /// The hotkey's hook runs on the thread that installed it, so this is
    /// rarely a cross-thread call — but the hook must not touch the window
    /// directly, so it posts and lets the message loop do the work.
    pub fn post_toggle(&self) {
        unsafe {
            PostMessageW(self.hwnd, WM_TOGGLE, 0, 0);
        }
    }

    /// Put the window where the user is looking.
    ///
    /// The monitor the cursor is on is the monitor the user is looking
    /// at, and the window is centred on that monitor's work area and
    /// sits in the upper third of it.
    fn place(&mut self) {
        let spot = spot_on(self.target_work_area(), self.width, self.height);
        // The spot may have cut the window, and the region is the
        // window's shape: it has to follow.
        self.height = spot.height;
        self.apply_region();
        unsafe {
            SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                spot.x,
                spot.y,
                self.width,
                self.height,
                SWP_NOACTIVATE,
            );
        }
    }

    /// The work area the window lives in.
    ///
    /// While the window is hidden it is the work area of the monitor the
    /// cursor is on: the launcher opens where the user is looking, and
    /// the user is where the pointer is. Once the window is up it is the
    /// work area of the monitor the window is on, because a window that
    /// moved to another screen while the user is typing in it would be a
    /// window the user cannot reach.
    fn target_work_area(&self) -> RECT {
        if self.is_visible() {
            own_work_area(self.hwnd)
        } else {
            work_area()
        }
    }

    /// Resize the window to fit what it has to show.
    pub fn resize(&mut self, width: i32, height: i32) {
        // The window is never taller than the room the work area has: the
        // model sizes it from the rows, and the rows have to fit between
        // the top of the area and the bottom of the screen.
        let spot = spot_on(self.target_work_area(), width, height);
        if width == self.width && spot.height == self.height {
            return;
        }
        self.width = width;
        self.height = spot.height;
        self.apply_region();
        unsafe {
            // The window stays where it is: it opened on the monitor the
            // user was looking at, and a window that moved to another
            // screen while the user is typing in it would be a window the
            // user cannot reach.
            SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                0,
                0,
                self.width,
                self.height,
                SWP_NOMOVE | SWP_NOACTIVATE,
            );
        }
    }

    /// The acrylic backdrop, when the platform has one.
    ///
    /// Win11's host backdrop is the blur behind the window, and the
    /// accent's gradient colour is the tint over it: 60% black, which is
    /// what "almost black, slightly transparent" is made of. On a platform
    /// without it the window paints its own near-black, and the only
    /// difference the user sees is that nothing moves behind it.
    ///
    /// The answer is whether the backdrop is really there. The call is
    /// undocumented and the platform is free to refuse it, and a window
    /// that believes in a blur that is not there is a window that paints
    /// nothing at all and shows whatever the screen held before.
    fn apply_backdrop(&self) -> bool {
        unsafe {
            // The rounded corners, when the platform rounds them itself.
            let pref: i32 = DWMWCP_ROUND;
            DwmSetWindowAttribute(
                self.hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                (&pref as *const i32).cast::<c_void>(),
                core::mem::size_of::<i32>() as u32,
            );

            let user32 = GetModuleHandleW(wide("user32.dll").as_ptr());
            if user32.is_null() {
                return false;
            }
            let name = b"SetWindowCompositionAttribute\0";
            let addr = GetProcAddress(user32, name.as_ptr());
            if addr.is_null() {
                return false;
            }
            let set: SetWindowCompositionAttributeFn =
                core::mem::transmute::<*mut c_void, SetWindowCompositionAttributeFn>(addr);
            // 0x99000000 is ABGR: 60% alpha, black. The tint over the blur.
            // The host backdrop is the Win11 one; the blur behind is the
            // Win10 one. The tint is 60% black either way, which is what
            // "almost black, slightly transparent" is made of. The first
            // policy the platform takes is the one it has.
            for state in [ACCENT_ENABLE_HOSTBACKDROP, ACCENT_ENABLE_BLURBEHIND] {
                let mut accent = AccentPolicy {
                    accent_state: state,
                    accent_flags: ACCENT_FLAG_DRAW_ALL_BORDERS,
                    gradient_color: 0x9900_0000,
                    animation_id: 0,
                };
                let mut data = WindowCompositionAttributeData {
                    attribute: WCA_ACCENT_POLICY,
                    data: (&mut accent as *mut AccentPolicy).cast::<c_void>(),
                    size_of_data: core::mem::size_of::<AccentPolicy>(),
                };
                if set(self.hwnd, &mut data) != 0 {
                    return true;
                }
            }
            false
        }
    }

    /// The rounded corners.
    ///
    /// A region, and not the corner preference: the preference rounds the
    /// window's *frame*, and the backdrop the accent painted is square
    /// behind it. A region clips both, so the blur follows the corner.
    fn apply_region(&self) {
        let radius = Layout::default().radius as i32;
        unsafe {
            let region = CreateRoundRectRgn(
                0,
                0,
                self.width + 1,
                self.height + 1,
                radius * 2,
                radius * 2,
            );
            if !region.is_null() {
                // The rule is the opposite of the usual GDI one: when the
                // call succeeds the system takes the region and it must
                // not be deleted, and when the call fails the region is
                // still ours and has to be given back.
                //
                // The answer is the *previous* region, so a first call
                // that succeeds answers zero as well — there was no
                // previous region. The last error is what tells the two
                // apart, and it is cleared before the call so that what
                // is read after it is this call's and not an older one.
                //
                // A resize makes a new region, and the old one is
                // already gone: the system took it.
                SetLastError(ERROR_SUCCESS);
                let previous = SetWindowRgn(self.hwnd, region, 1);
                if previous == 0 && GetLastError() != ERROR_SUCCESS {
                    // The call failed: the region is still ours.
                    DeleteObject(region);
                }
            }
        }
    }

    /// Run the message loop, until the window is closed.
    ///
    /// The events go to the host, and the host answers with what the
    /// window should do about them. The loop is the only place the window's
    /// thread lives, so it is also the only place the host's work happens:
    /// the answer to a keystroke is a query, and the query is the host's.
    ///
    /// The host answers with a [`Control`], and the window does it. The
    /// window is not lent to the host — the host is called *by* the
    /// window — so this is the only way the host can say "show yourself"
    /// without the two of them borrowing each other.
    pub fn pump(&mut self, mut on_event: impl FnMut(Event) -> Control) -> Result<(), String> {
        let mut msg = MSG {
            hwnd: core::ptr::null_mut(),
            message: 0,
            wParam: 0,
            lParam: 0,
            time: 0,
            pt: POINT { x: 0, y: 0 },
        };
        loop {
            unsafe {
                let got = GetMessageW(&mut msg, core::ptr::null_mut(), 0, 0);
                if got <= 0 {
                    break;
                }
                let control = self.dispatch(&msg, &mut on_event);
                match control {
                    Control::None => {}
                    Control::Show(view, width, height) => {
                        self.state_mut().view = view;
                        self.resize(width, height);
                        self.show();
                    }
                    Control::Hide => self.hide(),
                    Control::Paint(view, width, height) => {
                        self.state_mut().view = view;
                        self.resize(width, height);
                        self.repaint();
                    }
                    Control::Quit => break,
                }
            }
        }
        Ok(())
    }

    /// Turn one message into one control.
    ///
    /// A message the window handles itself — the paint, the resize — is
    /// given to the default procedure as well, and a message it does not
    /// handle at all goes straight there. The keys are the exception: a
    /// key the host did not want is translated and dispatched, because
    /// that is what turns a key press into the character it types.
    fn dispatch(&mut self, msg: &MSG, on_event: &mut impl FnMut(Event) -> Control) -> Control {
        unsafe {
            if msg.message == WM_TOGGLE {
                return on_event(Event::Toggle);
            }
            if (msg.message == WM_KEYDOWN || msg.message == WM_SYSKEYDOWN)
                && let Some(key) = translate_key(msg)
            {
                return on_event(Event::Key(key));
            }
            if msg.message == WM_CHAR
                && let Some(key) = translate_char(msg)
            {
                return on_event(Event::Key(key));
            }
            if msg.message == WM_SIZE {
                return on_event(Event::Resize);
            }
            if msg.message == WM_TIMER {
                return on_event(Event::Tick);
            }
            if msg.message == WM_CLOSE {
                return on_event(Event::Quit);
            }
            TranslateMessage(msg);
            DispatchMessageW(msg);
        }
        Control::None
    }
}

/// How far down the work area the window sits, as a fraction of it.
///
/// The upper third: the eye is there, and the middle of the screen is
/// where the user's hands are.
const VERTICAL_FRACTION: f32 = 0.18;

/// The window's place on a work area.
struct Spot {
    /// The left edge.
    x: i32,
    /// The top edge.
    y: i32,
    /// The height the window may have: it is never taller than the room
    /// it has.
    height: i32,
}

/// Where the window goes on a work area.
///
/// The window is centred on the area and sits in the upper third of it,
/// measured from the area's own top: the taskbar is not part of the
/// area, and neither is whatever is above it.
///
/// The window never falls outside the area. One that is taller than the
/// area is pinned to the top and cut to the area's height, and one that
/// is taller than the room below the offset is cut to that room.
fn spot_on(work: RECT, width: i32, height: i32) -> Spot {
    let area_w = work.right - work.left;
    let area_h = work.bottom - work.top;
    // A window wider than the area starts at its left edge: centred, it
    // would hang over the edge of the screen.
    let x = work.left + (area_w - width).max(0) / 2;
    // The offset is where the window starts, and the room below it is
    // what the window has to fit in.
    let offset = (area_h as f32 * VERTICAL_FRACTION) as i32;
    if height >= area_h {
        // Taller than the area: the window starts at the top and is cut.
        return Spot {
            x,
            y: work.top,
            height: area_h,
        };
    }
    let room = (area_h - offset).max(0);
    Spot {
        x,
        y: work.top + offset,
        height: height.min(room),
    }
}

/// The work area of the monitor the cursor is on.
///
/// The area is the part of the monitor the taskbar and the other bars
/// did not take, and a window that lives outside it is a window under
/// the taskbar.
///
/// The cursor is the monitor the user is looking at: the launcher opens
/// where the user is, and the user is where the pointer is. A launcher
/// that opens on the wrong screen is worse than no launcher.
fn work_area() -> RECT {
    unsafe {
        let mut point = POINT { x: 0, y: 0 };
        if GetCursorPos(&mut point) == 0 {
            return whole_screen();
        }
        work_area_of(MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST))
    }
}

/// The work area of the monitor the window is on.
fn own_work_area(hwnd: HWND) -> RECT {
    unsafe { work_area_of(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST)) }
}

/// The work area of a monitor.
fn work_area_of(monitor: HMONITOR) -> RECT {
    unsafe {
        if monitor.is_null() {
            return whole_screen();
        }
        let mut info = MONITORINFO {
            cbSize: core::mem::size_of::<MONITORINFO>() as u32,
            rcMonitor: RECT::default(),
            rcWork: RECT::default(),
            dwFlags: 0,
        };
        if GetMonitorInfoW(monitor, &mut info) == 0 {
            return whole_screen();
        }
        info.rcWork
    }
}

/// The whole screen, when the platform will not say where the work is.
///
/// The screen is bigger than the work area, and a window that is a
/// little too tall is pinned to the top and cut, while an area of zero
/// would put the window in the corner and make the host show a single
/// row.
fn whole_screen() -> RECT {
    unsafe {
        RECT {
            left: 0,
            top: 0,
            right: GetSystemMetrics(SM_CXSCREEN),
            bottom: GetSystemMetrics(SM_CYSCREEN),
        }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.hwnd);
            // The window is gone and nothing points at the state any
            // more: the user data went with the window. The allocation
            // is ours to give back.
            drop(Box::from_raw(self.state.as_ptr()));
        }
    }
}

/// The window's procedure.
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: UINT,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            if let Some(state) = unsafe { state_of(hwnd) } {
                let mut ps = PAINTSTRUCT {
                    hdc: core::ptr::null_mut(),
                    fErase: 0,
                    rcPaint: RECT::default(),
                    fRestore: 0,
                    fIncUpdate: 0,
                    rgbReserved: [0; 32],
                };
                let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
                // SAFETY: the state lives in the window's own thread, the
                // window procedure runs on that thread, and no other
                // thread ever holds a pointer to it — the hotkey posts a
                // message and nothing more. The pointer is the one the
                // window put in its own user data, and the allocation
                // lives as long as the window does.
                unsafe {
                    let state = &mut *state;
                    paint::paint(
                        hdc,
                        &state.view,
                        &mut state.painted,
                        state.backdrop,
                        &mut state.icons,
                        &state.fonts,
                    );
                }
                unsafe { EndPaint(hwnd, &ps) };
            }
            0
        }
        // The backdrop is the system's: erasing the background here would
        // paint over the blur the accent policy asked for.
        WM_ERASEBKGND => 1,
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// The window's state, from the window's own user data.
///
/// The pointer is the only path to the state: the window holds the same
/// pointer, and both reach the object through it. The borrow is not
/// handed out here — a borrow that outlives the call that made it would
/// be a second way into the object — so what comes back is the pointer,
/// and the caller decides what to do with it.
unsafe fn state_of(hwnd: HWND) -> Option<*mut State> {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWL_USERDATA) } as *mut State;
    if ptr.is_null() { None } else { Some(ptr) }
}

/// The registered window class, once.
///
/// The atom is a word: zero is "not registered yet", and anything else is
/// the atom itself. The class is registered once per process — registering
/// the same class twice is an error on Windows, and the launcher has one
/// window — and the word is what the second caller reads instead of
/// registering again.
static CLASS_ATOM: AtomicU16 = AtomicU16::new(0);

/// Register the window class, once.
fn register_class() -> Result<u16, String> {
    let known = CLASS_ATOM.load(Ordering::Acquire);
    if known != 0 {
        return Ok(known);
    }
    let atom = unsafe {
        let wc = WNDCLASSEXW {
            cbSize: core::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW | CS_DROPSHADOW,
            lpfnWndProc: Some(wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: GetModuleHandleW(core::ptr::null()),
            hIcon: core::ptr::null_mut(),
            hCursor: core::ptr::null_mut(),
            // No background brush: the accent policy paints the backdrop,
            // and a brush here would paint over it.
            hbrBackground: core::ptr::null_mut(),
            lpszMenuName: core::ptr::null(),
            lpszClassName: CLASS_NAME.as_ptr(),
            hIconSm: core::ptr::null_mut(),
        };
        RegisterClassExW(&wc)
    };
    if atom == 0 {
        return Err("the window class could not be registered".to_string());
    }
    // The first one to register owns the class; whoever comes later takes
    // the atom it got, because a second registration of the same class is
    // an error and not a new class.
    let _ = CLASS_ATOM.compare_exchange(0, atom, Ordering::AcqRel, Ordering::Acquire);
    Ok(CLASS_ATOM.load(Ordering::Acquire))
}

/// The control keys, from the system's own view of the keyboard.
fn translate_key(msg: &MSG) -> Option<Key> {
    let ctrl = unsafe { GetKeyState(VK_CONTROL as i32) < 0 };
    let vk = msg.wParam as u32;
    Some(match vk {
        VK_BACK => Key::Backspace,
        VK_TAB => Key::Tab,
        VK_RETURN => {
            if ctrl {
                Key::CtrlEnter
            } else {
                Key::Enter
            }
        }
        VK_ESCAPE => Key::Esc,
        VK_DELETE => Key::Delete,
        VK_LEFT => Key::Left,
        VK_RIGHT => Key::Right,
        VK_UP => Key::Up,
        VK_DOWN => Key::Down,
        VK_HOME => Key::Home,
        VK_END => Key::End,
        VK_PRIOR => Key::PageUp,
        VK_NEXT => Key::PageDown,
        _ if ctrl && vk == b'C' as u32 => Key::CtrlC,
        _ if ctrl && vk == b'P' as u32 => Key::CtrlP,
        _ if ctrl && vk == b'K' as u32 => Key::CtrlK,
        _ if ctrl && vk == b'W' as u32 => Key::CtrlW,
        _ if ctrl && vk == b'U' as u32 => Key::CtrlU,
        _ => return None,
    })
}

/// The characters, from the system's own view of the keyboard.
///
/// `WM_CHAR` is the right message for text: it knows the keyboard layout,
/// and it is the only one that does.
fn translate_char(msg: &MSG) -> Option<Key> {
    let c = msg.wParam as u16;
    if c == 0 {
        return None;
    }
    // The control characters the system sends for the keys it already
    // reported as a key event: they would be typed twice.
    if c < 0x20 && c != 0x09 {
        return None;
    }
    char::from_u32(c as u32).map(Key::Char)
}

/// A UTF-16 string with a NUL, for the calls that take one.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(core::iter::once(0)).collect()
}

unsafe extern "system" {
    fn GetKeyState(vKey: i32) -> i16;
    fn IsWindowVisible(hWnd: HWND) -> BOOL;
    fn InvalidateRect(hWnd: HWND, lpRect: *const RECT, bErase: BOOL) -> BOOL;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1920 by 1080 screen with a 40-pixel taskbar at the bottom.
    fn area() -> RECT {
        RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        }
    }

    #[test]
    fn the_window_is_centred_on_the_work_area() {
        let spot = spot_on(area(), 640, 484);
        assert_eq!(spot.x, 640, "the window is in the middle of the area");
    }

    #[test]
    fn the_window_sits_in_the_upper_third_of_the_work_area() {
        let spot = spot_on(area(), 640, 484);
        assert_eq!(spot.y, 187, "eighteen per cent down the area");
        assert_eq!(spot.height, 484, "the window keeps the height it asked for");
    }

    #[test]
    fn the_window_never_falls_outside_the_work_area() {
        let work = area();
        for height in [0, 100, 484, 1000, 1040, 2000] {
            let spot = spot_on(work, 640, height);
            assert!(spot.y >= work.top, "the window starts inside the area");
            assert!(
                spot.y + spot.height <= work.bottom,
                "the window ends inside the area"
            );
        }
    }

    #[test]
    fn a_window_taller_than_the_work_area_is_pinned_to_its_top() {
        let work = area();
        let spot = spot_on(work, 640, 2000);
        assert_eq!(spot.y, work.top, "the window starts at the top of the area");
        assert_eq!(
            spot.height,
            work.bottom - work.top,
            "and is cut to the height of the area"
        );
    }

    #[test]
    fn a_monitor_to_the_left_of_the_first_one_has_negative_coordinates() {
        // A monitor to the left of the primary one is at negative
        // coordinates, and the window has to be placed on that monitor
        // and not on the one the primary is.
        let work = RECT {
            left: -1920,
            top: 0,
            right: 0,
            bottom: 1040,
        };
        let spot = spot_on(work, 640, 484);
        assert_eq!(spot.x, -1280, "the window is centred on that monitor");
        assert_eq!(spot.y, 187);
    }

    #[test]
    fn a_window_wider_than_the_work_area_starts_at_its_left_edge() {
        // Centred, it would hang over the edge of the screen.
        let work = area();
        let spot = spot_on(work, 2400, 484);
        assert_eq!(spot.x, work.left);
    }
}
