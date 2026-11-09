// SPDX-License-Identifier: Apache-2.0

//! The window: a plain X11 override-redirect popup drawn into an
//! off-screen pixmap, with no toolkit and no window manager of its own.
//!
//! The **driver is shared with Windows**. `bins/rgxy/src/gui.rs` reaches
//! the platform through a module alias, so this module exports the very
//! same `Window`, `Event`, `View`, `Control`, `Compositor`, `Awareness` and
//! `Key` that `rgy-launcher-windows` does. A Linux port that invented a
//! second vocabulary would need a second driver.
//!
//! The event loop is a single [`poll`] over **three** file descriptors:
//! the X connection (`XConnectionNumber`), the read end of a self-pipe
//! (`runtime::wake_read_fd`) that another thread wakes with one byte, and
//! the single-instance listening socket (`sole::listen_fd`). A 120 ms
//! timeout *is* the clock while the window is up; there is no OS timer to
//! arm. Xlib buffers events, so `XPending`/`XNextEvent` are drained after
//! every poll, even on a tick.
//!
//! Everything Xlib touches is declared once in [`crate::x`]; the rest is
//! `no_std` + `alloc`. The only `unsafe` here is the FFI itself.
//!
//! [`poll`]: crate::x::poll

// The dispatch keeps the event's type, its window and its payload as
// three separate gates, in that order: a collapsed `if` would read as
// one condition where the reader needs to see the order the loop takes.
// The same shape is used by the two wake checks in `pump`.
#![allow(clippy::collapsible_if)]
// Every FFI call spells Xlib's own `c_int` / `c_uint` / `c_ulong` alias at
// the boundary. On LP64 Linux those aliases resolve to the same primitive
// width as the plain integers being cast, so a few casts are intentionally
// redundant; spelling the platform's type at the call is the house style in
// `x.rs`'s wrappers, and clearer than trimming them to the lint.
#![allow(clippy::unnecessary_cast)]

use alloc::string::{String, ToString as _};
use alloc::vec::Vec;
use core::ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_void};

use rgy_launcher::layout::SCALE_PERCENT;
use rgy_launcher::text::{Lang, Text};
use rgy_launcher::theme::Theme;

pub use crate::types::{Awareness, Compositor, Control, Event, Key, View};

use crate::decode;
use crate::geometry;
use crate::keys as k;
use crate::runtime;
use crate::x as xlib;

/// The clock's period, in milliseconds, while the window is visible.
const TICK_MS: i32 = 120;

/// The `poll` timeout while the latency bars are easing: one frame of the
/// animation, at about sixty a second.
const ANIMATION_MS: i32 = 16;

/// Where the window sits by default, in thousandths of the work area.
///
/// Centred across, a little under a fifth of the way down — the same
/// upper-third seat the Windows launcher opens on.
const DEFAULT_ANCHOR: (u32, u32) = (500, 180);

/// The launcher's window on X11.
///
/// The handles are the machine's: an X `Display`, the popup `Window`, a
/// `GC`, and the off-screen `Pixmap` the frame is drawn into before one
/// `XCopyArea` puts it on the window. The `model` is the size the driver
/// asked for; `width`/`height` are the device pixels actually used (clipped
/// to the work area by [`geometry::spot_on`]). Everything the driver draws
/// lives in `view`.
pub struct Window {
    /// The X connection, opened by [`Window::new`] and owned by this thread.
    display: *mut c_void,
    /// The launcher's own override-redirect popup.
    win: u64,
    /// The root window, parent of the popup and of the off-screen pixmap.
    /// The default screen number, for `XDisplayWidth`/`XDisplayHeight`.
    screen: c_int,
    /// The graphics context used for every drawing call.
    gc: *mut c_void,
    /// The frame's pixels, computed in client memory by [`crate::render`].
    renderer: crate::render::Renderer,
    /// The `XImage` the finished frame is handed to the server in.
    image: *mut c_void,
    /// The `malloc` buffer behind `image`: Xlib's own allocator, because an
    /// image's buffer is the library's to free.
    buffer: *mut u32,
    /// The buffer's stride, in pixels.
    image_w: u32,
    /// The buffer's height, in pixels.
    image_h: u32,
    /// The window's width in device pixels.
    width: i32,
    /// The window's height in device pixels.
    height: i32,
    /// The size the driver last asked for, `(width, height)`.
    model: (i32, i32),
    /// Where the window sits, in thousandths of the work area.
    anchor: (u32, u32),
    /// The language the window's own words are read in.
    lang: Lang,
    /// The frame the driver wants on the screen.
    view: View,
    /// The colours the panel wears (kept in step with `view.theme`).
    colours: Theme,
    /// How opaque the panel is (kept in step with `view.alpha`).
    alpha: u32,
    /// Whether the window is mapped.
    visible: bool,
    /// The tray icon, when the notification area gave us one.
    tray: Option<crate::tray::Tray>,
    /// Why the tray icon is missing, when it is.
    tray_error: Option<String>,
    /// The read end of the self-pipe the loop polls.
    wake_read: c_int,
    /// The write end other threads write a byte to.
    wake_write: c_int,
    /// Whether the poll timeout should act as a ticking clock. Kept equal
    /// to `visible`; read by [`Window::pump`].
    clock_running: bool,
    /// `WM_PROTOCOLS`, for the standard close message.
    wm_protocols: u64,
    /// `WM_DELETE_WINDOW`, a request to close.
    wm_delete: u64,
    /// `_RGY_SHOW`, a second launcher asking this one to show itself.
    rgy_show: u64,
    /// `_RGY_WAKE`, a private wake message (the pipe's other route).
    rgy_wake: u64,
}

impl Window {
    /// Open the display and build the (still hidden) window.
    ///
    /// `XOpenDisplay(null)` reads `DISPLAY`; a null answer is an error.
    /// `XInitThreads` is called (its result ignored) and the default screen,
    /// root, and screen size are read. A non-blocking, close-on-exec
    /// self-pipe is made with `pipe2`. The window is an `InputOutput`
    /// override-redirect popup at `CopyFromParent` depth with a null visual,
    /// a black background pixel and a key/button/expose/structure event
    /// mask. Its `_NET_WM_WINDOW_TYPE` is a utility and its
    /// `_NET_WM_STATE` is above and skip-taskbar, both set as format-32
    /// `XA_ATOM` properties. `XStoreName` carries a NUL-terminated copy of
    /// `title`. A GC and an off-screen pixmap are made, the machine state
    /// is installed in [`runtime`], and the tray is built — a missing tray
    /// is *not* fatal, its reason is kept in `tray_error`.
    ///
    /// The window is **not** mapped here: the compositor answer on X11 is
    /// always `Accepted` (a window always gets its drawable), and the first
    /// frame is drawn by the driver through `set_view`/`resize`/`show`.
    pub fn new(title: &str) -> Result<Window, String> {
        // `XInitThreads` has to be the FIRST Xlib call, before the display
        // is opened. The launcher never calls Xlib from another thread —
        // the worker wakes the loop through a pipe — but the library still
        // wants its threading set up before it builds the connection.
        unsafe {
            let _ = xlib::XInitThreads();
        }
        // SAFETY: every Xlib call below is made on the display opened here,
        // on this thread, before any worker starts.
        let display = unsafe { xlib::XOpenDisplay(core::ptr::null()) };
        if display.is_null() {
            return Err(String::from("the X display could not be opened"));
        }
        let screen = unsafe { xlib::XDefaultScreen(display) };
        let root = unsafe { xlib::XDefaultRootWindow(display) } as u64;
        let screen_w = unsafe { xlib::XDisplayWidth(display, screen) };
        let screen_h = unsafe { xlib::XDisplayHeight(display, screen) };

        // A byte in this pipe is a Wake. O_NONBLOCK keeps a worker from
        // stalling on a full pipe and O_CLOEXEC closes both ends on exec.
        // `pipe2` takes `O_*` flags on both kernels; `SOCK_CLOEXEC` is for
        // `socket()`/`accept4()` instead and has a DIFFERENT value on
        // FreeBSD, so the socket flag must not be used here.
        let mut fds: [c_int; 2] = [-1, -1];
        let flags = (xlib::O_NONBLOCK as c_int) | (xlib::O_CLOEXEC as c_int);
        let pipe_rc = unsafe { xlib::pipe2(fds.as_mut_ptr(), flags) };
        if pipe_rc != 0 {
            unsafe {
                xlib::XCloseDisplay(display);
            }
            return Err(String::from("the wake pipe could not be created"));
        }
        let (wake_read, wake_write) = (fds[0], fds[1]);

        // The event mask is a `long`; the masks in `crate::keys` are plain
        // i64 integers, so they combine without a single cast.
        let mut attrs: xlib::XSetWindowAttributes = unsafe { core::mem::zeroed() };
        attrs.override_redirect = 1;
        attrs.background_pixel = 0;
        attrs.event_mask = k::KeyPressMask
            | k::KeyReleaseMask
            | k::ExposureMask
            | k::StructureNotifyMask
            | k::ButtonPressMask;
        let value_mask = (xlib::CWOverrideRedirect as c_ulong)
            | (xlib::CWBackPixel as c_ulong)
            | (xlib::CWEventMask as c_ulong);

        let win_w = 600u32.min(screen_w.max(1) as u32).max(1);
        let win_h = 400u32.min(screen_h.max(1) as u32).max(1);
        let win = unsafe {
            xlib::XCreateWindow(
                display,
                root,
                0 as c_int,
                0 as c_int,
                win_w as c_uint,
                win_h as c_uint,
                0 as c_uint,
                xlib::CopyFromParent as c_int,
                xlib::InputOutput as c_int,
                core::ptr::null_mut(),
                value_mask,
                &mut attrs,
            )
        } as u64;
        if win == 0 {
            unsafe {
                xlib::XCloseDisplay(display);
                xlib::close(wake_read);
                xlib::close(wake_write);
            }
            return Err(String::from("the launcher window could not be created"));
        }

        // The window manager hints: a utility window, kept above the rest
        // and out of the taskbar. Both are format-32 atom lists.
        let a_type = intern(display, b"_NET_WM_WINDOW_TYPE\0");
        let a_type_utility = intern(display, b"_NET_WM_WINDOW_TYPE_UTILITY\0");
        let a_state = intern(display, b"_NET_WM_STATE\0");
        let a_above = intern(display, b"_NET_WM_STATE_ABOVE\0");
        let a_skip = intern(display, b"_NET_WM_STATE_SKIP_TASKBAR\0");
        let wm_protocols = intern(display, b"WM_PROTOCOLS\0");
        let wm_delete = intern(display, b"WM_DELETE_WINDOW\0");
        let rgy_show = intern(display, b"_RGY_SHOW\0");
        let rgy_wake = intern(display, b"_RGY_WAKE\0");
        set_atoms(display, win, a_type, &[a_type_utility as c_ulong]);
        set_atoms(
            display,
            win,
            a_state,
            &[a_above as c_ulong, a_skip as c_ulong],
        );
        set_atoms(display, win, wm_protocols, &[wm_delete as c_ulong]);

        // XStoreName wants a NUL-terminated byte string, so the title is
        // copied into one rather than passed borrowed.
        let mut name: Vec<u8> = Vec::with_capacity(title.len() + 1);
        name.extend_from_slice(title.as_bytes());
        name.push(0);
        unsafe {
            xlib::XStoreName(display, win, name.as_ptr() as *const c_char);
        }

        let gc = unsafe { xlib::XCreateGC(display, win, 0, core::ptr::null_mut()) };
        if gc.is_null() {
            unsafe {
                xlib::XDestroyWindow(display, win);
                xlib::XCloseDisplay(display);
                xlib::close(wake_read);
                xlib::close(wake_write);
            }
            return Err(String::from("the graphics context could not be created"));
        }

        // The frame's image: a buffer from Xlib's own allocator (an image's
        // buffer is the library's to free) and the XImage that carries it.
        // It is allocated once and grown only when a frame outgrows it.
        let depth = unsafe { xlib::XDefaultDepth(display, screen) }.max(24) as c_uint;
        let visual = unsafe { xlib::XDefaultVisual(display, screen) };
        let cap_w = win_w.max(64);
        let cap_h = win_h.max(64);
        let buffer = unsafe { xlib::malloc((cap_w as usize * cap_h as usize) * 4) }.cast::<u32>();
        let image = if buffer.is_null() {
            core::ptr::null_mut()
        } else {
            unsafe {
                xlib::XCreateImage(
                    display,
                    visual,
                    depth,
                    xlib::ZPixmap,
                    0,
                    buffer.cast::<c_char>(),
                    cap_w,
                    cap_h,
                    32,
                    (cap_w * 4) as c_int,
                )
            }
        };
        if image.is_null() {
            unsafe {
                if !buffer.is_null() {
                    xlib::free(buffer.cast::<c_void>());
                }
                xlib::XFreeGC(display, gc);
                xlib::XDestroyWindow(display, win);
                xlib::XCloseDisplay(display);
                xlib::close(wake_read);
                xlib::close(wake_write);
            }
            return Err(String::from("the frame image could not be created"));
        }

        // The one launcher's machine state, shared with workers through
        // atomics. The single-instance socket is deliberately NOT stored
        // here — `pump` asks `sole::listen_fd()` directly.
        runtime::install(display, root, win, wake_read, wake_write);

        let text = Text::for_lang(Lang::English);
        let (tray, tray_error) = match crate::tray::Tray::new(text) {
            Ok(icon) => (Some(icon), None),
            Err(reason) => (None, Some(reason)),
        };
        let view = View {
            placeholder: text.placeholder.to_string(),
            ..View::default()
        };

        // The panel's opacity is a compositor property, and the default view
        // is already the one the window opens with.
        apply_opacity(display, win, view.alpha);
        unsafe {
            xlib::XFlush(display);
        }
        Ok(Window {
            display,
            win,
            screen,
            gc,
            renderer: crate::render::Renderer::new(win_w, win_h),
            image,
            buffer,
            image_w: cap_w,
            image_h: cap_h,
            width: win_w as i32,
            height: win_h as i32,
            model: (0, 0),
            anchor: DEFAULT_ANCHOR,
            lang: Lang::English,
            view,
            colours: Theme::default(),
            alpha: 255,
            visible: false,
            tray,
            tray_error,
            wake_read,
            wake_write,
            clock_running: false,
            wm_protocols,
            wm_delete,
            rgy_show,
            rgy_wake,
        })
    }

    /// Read the window's own words in `lang`, and tell the tray icon too.
    ///
    /// The empty field's placeholder and the icon's tip are the two words
    /// the window speaks itself; the host owns the rows and the query.
    pub fn with_lang(mut self, lang: Lang) -> Window {
        self.lang = lang;
        let text = Text::for_lang(lang);
        self.view.placeholder = text.placeholder.to_string();
        if let Some(tray) = self.tray.as_mut() {
            tray.set_tip(text.tray_tip);
        }
        self
    }

    /// Paint in another theme.
    ///
    /// The theme is the whole palette at once and the alpha goes with it.
    /// The panel is repainted at once if the window is already up, so a
    /// theme the user chose is on the screen the moment the arrow leaves
    /// the key. (The view itself is the authority on colours — the next
    /// `set_view` carries them — so these two fields are kept in step.)
    pub fn set_colours(&mut self, theme: Theme, alpha: u32) {
        // Wearing the same theme again costs nothing: the comparison is a
        // few words and the repaint it would cause is not. This is also
        // where the stored colours are read, kept in step with the view.
        let changed = self.colours != theme || self.alpha != alpha;
        if self.alpha != alpha {
            apply_opacity(self.display, self.win, alpha);
        }
        self.colours = theme;
        self.alpha = alpha;
        if changed && self.visible {
            self.repaint();
        }
    }

    /// Move the window to another place on its work area.
    ///
    /// The numbers are permille; `geometry::clamp_anchor` keeps an anchor
    /// the screen cannot honour from pushing the window off it, and the
    /// window is re-placed at once.
    pub fn set_anchor(&mut self, x: u32, y: u32) {
        self.anchor = geometry::clamp_anchor(x, y);
        self.place();
    }

    /// The window's token, for the hotkey's guards.
    ///
    /// X11 has no `HWND`: this is the numeric window id cast to a pointer,
    /// a real, non-null token whose *value* the hotkey guards ignore.
    pub fn hwnd(&self) -> *mut c_void {
        self.win as usize as *mut c_void
    }

    /// What the compositor said about the window's own picture.
    ///
    /// X11 always gives a window its drawable, so the answer is always
    /// `Accepted`; the variants exist so the Windows driver's question has
    /// one answer on both hosts.
    pub fn compositor(&self) -> Compositor {
        Compositor::Accepted
    }

    /// What the process knows about the screen's scale.
    ///
    /// X11 reports pixels, not a scale, and the launcher draws one unit
    /// per pixel — the `PerMonitor` answer that says nothing is stretched.
    pub fn awareness(&self) -> Awareness {
        Awareness::PerMonitor
    }

    /// Why the notification area has no icon, when it has none.
    ///
    /// The launcher works without the icon (the hotkey does not go through
    /// it), so the reason is not shown to the user — it is the answer to a
    /// reader's "why is there no icon?".
    pub fn tray_error(&self) -> Option<&str> {
        self.tray_error.as_deref()
    }

    /// How much room there is below the anchor, in the model's units.
    ///
    /// The host sizes the window from `rows_that_fit` of this number: on
    /// X11 the screen's device pixels are the model's units at 100%.
    pub fn work_area_height(&self) -> u32 {
        let h = if self.display.is_null() {
            0
        } else {
            unsafe { xlib::XDisplayHeight(self.display, self.screen) }.max(0) as u32
        };
        geometry::rows_height(h, SCALE_PERCENT, self.anchor.1)
    }

    /// What the window draws.
    pub fn view(&self) -> &View {
        &self.view
    }

    /// Change what the window draws.
    ///
    /// The view carries its own theme and anchor: `install` wears them and
    /// re-places the window if the seat moved. A visible window is
    /// repainted at once (the driver calls this before the first `show`,
    /// when it is still hidden, so that costs nothing). The pump's
    /// `Control::Paint` path uses `install` directly so *it* owns the
    /// repaint decision and can skip a frame nobody can see.
    pub fn set_view(&mut self, view: View) {
        self.install(view);
        if self.visible {
            self.repaint();
        }
    }

    /// Store a frame without painting it.
    ///
    /// The placeholder is overwritten with the window's own language, and
    /// the theme, alpha and anchor are worn from the view. Painting is the
    /// caller's: `set_view` repaints, `apply` decides for itself.
    fn install(&mut self, mut view: View) {
        view.placeholder = Text::for_lang(self.lang).placeholder.to_string();
        self.colours = view.theme;
        if self.alpha != view.alpha {
            self.alpha = view.alpha;
            apply_opacity(self.display, self.win, self.alpha);
        }
        let anchor = geometry::clamp_anchor(view.anchor.0, view.anchor.1);
        if anchor != self.anchor {
            self.anchor = anchor;
            self.place();
        }
        self.view = view;
    }

    /// Resize the window to `(width, height)` and repaint it.
    ///
    /// The public setter, used by the driver before the first `show`. The
    /// pump's `Control::Paint` calls `resize_inner(.., false)` instead so a
    /// frame that has not changed is not painted sixteen times a second.
    pub fn resize(&mut self, width: i32, height: i32) {
        self.resize_inner(width, height, true);
    }

    /// The geometry of a resize, with repainting left to the caller.
    fn resize_inner(&mut self, width: i32, height: i32, repaint: bool) {
        self.model = (width, height);
        let spot = geometry::spot_on(self.work_area(), width, height, self.anchor);
        let w = spot.width.max(1);
        let h = spot.height.max(1);
        if self.width == w && self.height == h {
            if repaint && self.visible {
                self.repaint();
            }
            return;
        }
        self.width = w;
        self.height = h;
        // The frame is computed at the window's size, which the renderer
        // learns on the next repaint.
        unsafe {
            xlib::XResizeWindow(self.display, self.win, w as c_uint, h as c_uint);
        }
        if repaint {
            self.repaint();
        }
    }

    /// The desktop's work area: the screen minus its panels.
    ///
    /// `_NET_WORKAREA` is what a desktop with a panel publishes, and asking
    /// on each placement (rather than caching) keeps a panel the user moved
    /// where it now is. A desktop that says nothing answers with the screen,
    /// which is what the launcher assumed before.
    fn work_area(&self) -> geometry::WorkArea {
        let (sw, sh) = self.screen_size();
        work_area_of_screen(
            work_area(self.display, self.root_window(), self.screen),
            sw,
            sh,
        )
    }

    /// The root window this display was opened on.
    fn root_window(&self) -> u64 {
        if self.display.is_null() {
            return 0;
        }
        // SAFETY: the display is live for the window's whole life.
        unsafe { xlib::XDefaultRootWindow(self.display) }
    }
    /// The screen's device size, in pixels.
    fn screen_size(&self) -> (i32, i32) {
        if self.display.is_null() {
            return (0, 0);
        }
        unsafe {
            (
                xlib::XDisplayWidth(self.display, self.screen),
                xlib::XDisplayHeight(self.display, self.screen),
            )
        }
    }

    /// Put the window at its seat on the work area.
    fn place(&mut self) {
        let spot = geometry::spot_on(self.work_area(), self.width, self.height, self.anchor);
        unsafe {
            xlib::XMoveResizeWindow(
                self.display,
                self.win,
                spot.x as c_int,
                spot.y as c_int,
                spot.width.max(1) as c_uint,
                spot.height.max(1) as c_uint,
            );
        }
    }

    /// Map the window, take the keyboard, start the clock, paint.
    pub fn show(&mut self) {
        self.place();
        unsafe {
            xlib::XMapRaised(self.display, self.win);
            xlib::XSetInputFocus(
                self.display,
                self.win,
                xlib::RevertToParent as c_int,
                xlib::CurrentTime as c_ulong,
            );
        }
        self.visible = true;
        self.clock_running = true;
        self.repaint();
    }

    /// Unmap the window and stop the clock. The process stays.
    pub fn hide(&mut self) {
        unsafe {
            xlib::XUnmapWindow(self.display, self.win);
        }
        self.visible = false;
        self.clock_running = false;
    }

    /// Whether the window is mapped.
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Whether the window is up and has a real X id.
    pub fn is_on_the_screen(&self) -> bool {
        self.visible && self.win != 0
    }

    /// Compute the frame in client memory, then hand it to the server.
    ///
    /// The pixels are the tree's own (see [`crate::render`]); the server is
    /// asked for one thing, `XPutImage`, and that is the only server-side
    /// call a frame makes. A frame is never seen half-finished because the
    /// buffer is complete before it is sent.
    pub fn repaint(&mut self) {
        if self.display.is_null() || self.gc.is_null() || self.image.is_null() {
            return;
        }
        let w = self.width.max(1) as u32;
        let h = self.height.max(1) as u32;
        self.renderer.resize(w, h);
        let (pw, ph) = self.renderer.draw(&self.view, self.lang);
        if pw == 0 || ph == 0 {
            return;
        }
        if pw > self.image_w || ph > self.image_h {
            self.grow_image(pw, ph);
            if self.buffer.is_null() || self.image.is_null() {
                return;
            }
        }
        // The surface is straight ARGB with no padding; the image is 24-bit
        // with a stride. One row at a time, because the two widths differ.
        let stride = self.image_w as usize;
        let surface = &self.renderer.surface;
        // SAFETY: the buffer holds `image_w * image_h` u32s, and `pw`/`ph`
        // are inside those bounds after the growth above.
        let out =
            unsafe { core::slice::from_raw_parts_mut(self.buffer, stride * self.image_h as usize) };
        for row in 0..ph as usize {
            let from = row * pw as usize;
            let to = row * stride;
            for col in 0..pw as usize {
                out[to + col] = crate::raster::premultiplied_to_x11(surface.pixels[from + col]);
            }
        }
        // SAFETY: the image, the window and the GC are the window's own live
        // handles, and the region is inside the image.
        unsafe {
            xlib::set_image_size(self.image, pw, ph);
            xlib::XPutImage(
                self.display,
                self.win,
                self.gc,
                self.image,
                0,
                0,
                0,
                0,
                pw,
                ph,
            );
            xlib::XFlush(self.display);
        }
    }

    /// Grow the frame buffer, and rebuild the image around it.
    ///
    /// `XDestroyImage` is a macro in Xlib and not a symbol, so the image is
    /// given back with `XFree` when it has to change size at all — and it
    /// changes size only when a frame outgrows the buffer, which is a resize
    /// and not a keystroke.
    fn grow_image(&mut self, width: u32, height: u32) {
        if self.display.is_null() {
            return;
        }
        let w = width.max(self.image_w).max(64);
        let h = height.max(self.image_h).max(64);
        // SAFETY: the handles are this window's, and the old buffer goes
        // back to the allocator that made it.
        unsafe {
            if !self.image.is_null() {
                xlib::XFree(self.image);
                self.image = core::ptr::null_mut();
            }
            if !self.buffer.is_null() {
                xlib::free(self.buffer.cast::<c_void>());
                self.buffer = core::ptr::null_mut();
            }
            let bytes = (w as usize).saturating_mul(h as usize).saturating_mul(4);
            let buffer = xlib::malloc(bytes).cast::<u32>();
            if buffer.is_null() {
                return;
            }
            let depth = xlib::XDefaultDepth(self.display, self.screen).max(24) as c_uint;
            let visual = xlib::XDefaultVisual(self.display, self.screen);
            let image = xlib::XCreateImage(
                self.display,
                visual,
                depth,
                xlib::ZPixmap,
                0,
                buffer.cast::<c_char>(),
                w,
                h,
                32,
                (w * 4) as c_int,
            );
            if image.is_null() {
                xlib::free(buffer.cast::<c_void>());
                return;
            }
            self.buffer = buffer;
            self.image = image;
            self.image_w = w;
            self.image_h = h;
        }
    }

    /// Start the clock.
    ///
    /// On X11 there is **no timer to arm**: the clock is the `poll` timeout
    /// in `pump`, which ticks for as long as the window is visible. This
    /// entry point exists for parity with the Windows twin; `show`/`hide`
    /// move the `clock_running` marker, and `pump` reads it.
    pub fn start_clock(&self) {
        // Intentionally empty: the poll timeout is the clock.
    }

    /// Stop the clock. See [`Window::start_clock`].
    pub fn stop_clock(&self) {
        // Intentionally empty: the timeout stops with the visibility.
    }

    /// Ask for a toggle from another thread.
    ///
    /// On X11 the hotkey is delivered *inside* the pump: a passive root
    /// grab produces a `KeyPress` that `hotkey::on_key_press` turns into a
    /// `Toggle` on the loop's own thread. There is no posted message to
    /// synthesize, and the self-pipe carries only `Wake` bytes, so this
    /// compatibility entry point is honestly a no-op.
    pub fn post_toggle(&self) {
        // Intentionally empty: the hotkey does not post on X11.
    }

    /// The main loop: `poll` three fds, dispatch X events, answer controls.
    ///
    /// While visible the timeout is 120 ms and a timeout delivers
    /// `Event::Tick`; hidden, the timeout is infinite. `XPending` is drained
    /// after every poll (Xlib buffers events). Each X event is turned into
    /// an `Option<Event>`, delivered to `on_event`, and the returned
    /// `Control` is applied. `poll` returning < 0 (EINTR) just waits again.
    /// A `Control::Quit` ends the loop with `Ok(())`.
    pub fn pump(&mut self, mut on_event: impl FnMut(Event) -> Control) -> Result<(), String> {
        let x_fd = unsafe { xlib::XConnectionNumber(self.display) };
        let wake_fd = runtime::wake_read_fd();
        let poll_in = xlib::POLLIN as core::ffi::c_short;
        loop {
            let sole_fd = crate::sole::listen_fd();
            // A negative fd is ignored by poll, so skipped fds are simply
            // parked at -1 and the array is always three entries long.
            let mut fds = [
                xlib::pollfd {
                    fd: if x_fd >= 0 { x_fd } else { -1 },
                    events: poll_in,
                    revents: 0,
                },
                xlib::pollfd {
                    fd: if wake_fd >= 0 { wake_fd } else { -1 },
                    events: poll_in,
                    revents: 0,
                },
                xlib::pollfd {
                    fd: if sole_fd >= 0 { sole_fd } else { -1 },
                    events: poll_in,
                    revents: 0,
                },
            ];
            let timeout = tick_timeout(
                self.visible && self.clock_running,
                self.renderer.is_animating(),
            );
            // SAFETY: `fds` is three contiguous pollfds that outlive the
            // call; poll only reads `fd`/`events` and writes `revents`.
            let rc = unsafe { xlib::poll(fds.as_mut_ptr(), fds.len(), timeout as c_int) };
            if rc < 0 {
                // EINTR: a signal is no reason to leave the loop.
                continue;
            }
            if rc == 0 && self.visible {
                // A clock tick: the host decides whether typing settled.
                if !self.apply(on_event(Event::Tick)) {
                    return Ok(());
                }
            }

            // Drain the X connection even on a tick — a buffered KeyPress
            // must not wait a whole clock to reach the field.
            let mut quit = false;
            while unsafe { xlib::XPending(self.display) } > 0 {
                let mut ev = blank_event();
                // SAFETY: the display is live and `ev` is a writable XEvent.
                unsafe { xlib::XNextEvent(self.display, &mut ev) };
                if !self.dispatch(&mut ev, &mut on_event) {
                    quit = true;
                    break;
                }
            }
            if quit {
                return Ok(());
            }

            // The self-pipe: drain every byte and deliver one Wake.
            if (fds[1].revents & poll_in) != 0 {
                let mut buf = [0u8; 32];
                let mut woken = false;
                loop {
                    // SAFETY: `buf` is writable; read on the non-blocking
                    // pipe returns <= 0 on EAGAIN.
                    let n = unsafe { xlib::read(wake_fd, buf.as_mut_ptr() as *mut c_void, 32) };
                    if n <= 0 {
                        break;
                    }
                    woken = true;
                }
                if woken && !self.apply(on_event(Event::Wake)) {
                    return Ok(());
                }
            }

            // The single-instance socket: a second launcher asked to show.
            if (fds[2].revents & poll_in) != 0 && crate::sole::accept_show() {
                if !self.apply(on_event(Event::Show)) {
                    return Ok(());
                }
            }

            // The bars ease towards the meter's newest sample one frame at a
            // time, so while anything still moves the loop paints.
            if self.visible && self.renderer.is_animating() {
                self.repaint();
            }
        }
    }

    /// Turn one X event into an optional launcher event and apply it.
    ///
    /// Returns false when a `Control::Quit` came back, which ends the loop.
    fn dispatch(
        &mut self,
        ev: &mut xlib::XEvent,
        on_event: &mut impl FnMut(Event) -> Control,
    ) -> bool {
        let t = xlib::ev_type(ev) as i32;
        if t == k::Expose {
            // A paint request is answered here, not by the driver: the
            // frame is already in the pixmap, so one copy is enough.
            self.repaint();
            return true;
        }
        if t == k::SelectionRequest {
            crate::actions::serve_selection_request(ev);
            return true;
        }
        if t == k::MappingNotify {
            // The keyboard map changed: the hotkey re-reads keycodes; the
            // field sees no key from this.
            return true;
        }
        if t == k::ClientMessage {
            return self.on_client_message(ev, on_event);
        }
        if t == k::KeyPress {
            return self.on_key_press(ev, on_event);
        }
        if t == k::ButtonPress || t == k::ButtonRelease {
            // The tray's icon and its menu live on their own X window, and
            // XNextEvent returns their events too.
            if t == k::ButtonPress {
                if let Some(event) = crate::tray::on_event(ev) {
                    return self.deliver(event, on_event);
                }
            }
            return true;
        }
        // The structural events are a pure type -> event decision.
        if let Some(event) = event_of_type(t) {
            if xlib::ev_window(ev) as u64 == self.win {
                return self.deliver(event, on_event);
            }
        }
        true
    }

    /// A `ClientMessage`: the tray, a close, or a private show/wake atom.
    fn on_client_message(
        &mut self,
        ev: &mut xlib::XEvent,
        on_event: &mut impl FnMut(Event) -> Control,
    ) -> bool {
        // The tray's own menu also speaks in client messages.
        if let Some(event) = crate::tray::on_event(ev) {
            return self.deliver(event, on_event);
        }
        let mt = xlib::ev_message_type(ev) as u64;
        if mt == self.wm_delete {
            return self.deliver(Event::Quit, on_event);
        }
        // The standard close routes through WM_PROTOCOLS + data[0].
        if mt == self.wm_protocols && xlib::ev_client_data(ev, 0) as u64 == self.wm_delete {
            return self.deliver(Event::Quit, on_event);
        }
        if mt == self.rgy_show {
            return self.deliver(Event::Show, on_event);
        }
        if mt == self.rgy_wake {
            return self.deliver(Event::Wake, on_event);
        }
        true
    }

    /// A `KeyPress`: the hotkey first, then the field, then a replay.
    fn on_key_press(
        &mut self,
        ev: &xlib::XEvent,
        on_event: &mut impl FnMut(Event) -> Control,
    ) -> bool {
        // Every key press goes to the global hotkey first. Some(Toggle)
        // means the key fired the hotkey and must NOT reach the field.
        if let Some(event) = crate::hotkey::on_key_press(ev) {
            return self.deliver(event, on_event);
        }
        if xlib::ev_window(ev) as u64 != self.win {
            // A key from a passive root grab: replay it so the window that
            // really has the focus receives it, and never feed the field.
            unsafe {
                xlib::XAllowEvents(
                    self.display,
                    xlib::ReplayKeyboard as c_int,
                    xlib::ev_time(ev) as c_ulong,
                );
            }
            return true;
        }
        // Our window. `XLookupKeysym(event, 0)` is the UNSHIFTED level:
        // with Shift held it still answers `XK_period` for the key that
        // spells `>`. So the character comes from `XLookupString`, which
        // applies Shift, Caps Lock and AltGr; the keysym still decides
        // the special keys and every Ctrl combination.
        unsafe {
            let state = xlib::ev_state(ev);
            let ctrl = decode::ctrl_of(state);
            let alt = state & (xlib::Mod1Mask | xlib::Mod4Mask) != 0;
            let mut keysym = xlib::XLookupKeysym(ev, 0 as c_int) as u64;
            if keysym == 0 {
                keysym = xlib::XKeycodeToKeysym(
                    self.display,
                    xlib::ev_keycode(ev) as xlib::KeyCode,
                    0 as c_int,
                ) as u64;
            }
            let mut text = [0u8; 32];
            let mut ignored: xlib::KeySym = 0;
            let written = xlib::XLookupString(
                ev,
                text.as_mut_ptr().cast::<c_char>(),
                text.len() as c_int,
                &mut ignored,
                core::ptr::null_mut(),
            );
            let text = if written > 0 {
                &text[..(written as usize).min(text.len())]
            } else {
                &[][..]
            };
            let key = decode::typed_key(keysym, ctrl, alt, text);
            if let Some(k) = key {
                return self.deliver(Event::Key(k), on_event);
            }
        }
        true
    }

    /// Deliver one event to the driver and apply its control.
    fn deliver(&mut self, event: Event, on_event: &mut impl FnMut(Event) -> Control) -> bool {
        self.apply(on_event(event))
    }

    /// Do what a `Control` says. False when the window is done (Quit).
    ///
    /// `Paint` compares the old view and the old model to the incoming
    /// frame first: a frame that is already on the screen is not painted
    /// again, and a frame nobody can see is not painted at all. This is the
    /// no-news repaint guard the Windows driver measured on 2026-11-04.
    fn apply(&mut self, control: Control) -> bool {
        match control {
            Control::None => true,
            Control::Show(view, width, height) => {
                self.install(view);
                self.resize_inner(width, height, false);
                self.show();
                true
            }
            Control::Hide => {
                self.hide();
                true
            }
            Control::Paint(view, width, height) => {
                let same =
                    same_frame(&self.view, &view, width, height) && self.model == (width, height);
                self.install(view);
                self.resize_inner(width, height, false);
                if !same && self.visible {
                    self.repaint();
                }
                true
            }
            Control::Quit => false,
        }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: every handle below was made by new() on this display and
        // is still owned here. The image is given back with `XFree` (its
        // `XDestroyImage` is a macro, not a symbol) and the buffer behind
        // it with the allocator that made it.
        unsafe {
            if !self.display.is_null() {
                if !self.image.is_null() {
                    xlib::XFree(self.image);
                    self.image = core::ptr::null_mut();
                }
                if !self.buffer.is_null() {
                    xlib::free(self.buffer.cast::<c_void>());
                    self.buffer = core::ptr::null_mut();
                }
                if !self.gc.is_null() {
                    xlib::XFreeGC(self.display, self.gc);
                }
                if self.win != 0 {
                    xlib::XDestroyWindow(self.display, self.win);
                }
                xlib::XCloseDisplay(self.display);
            }
        }
        if self.wake_read >= 0 {
            unsafe { xlib::close(self.wake_read) };
        }
        if self.wake_write >= 0 {
            unsafe { xlib::close(self.wake_write) };
        }
        runtime::clear();
        crate::hotkey::reset();
        // Dropping `self.tray` (an Option<Tray>) runs Tray::drop, which
        // gives its own icon window back to X.
    }
}

/// Wake a window's loop from any thread.
///
/// `hwnd` is ignored on X11: a wake is **one byte in a pipe**, not a posted
/// window message, and `runtime::wake` writes it to the write end set by
/// `install`. The handle is kept in the signature because the worker that
/// asks the daemon is shared with the Windows build.
pub fn wake(hwnd: usize) {
    let _ = hwnd;
    let _ = runtime::wake();
}

/// The launcher window of this session, when there is one.
///
/// X11 has no `FindWindowW`: the one resident launcher publishes its own
/// window id in [`runtime`], and a second launcher asks it to show itself
/// (or reaches it over the session socket — both routes are fine). The
/// socket is an abstract name on Linux and a `/tmp` path on FreeBSD;
/// these two are the X11 route, kept for a second launcher that has a
/// display but no socket.
#[allow(dead_code)]
pub(crate) fn find_window() -> Option<u64> {
    let w = runtime::window();
    if w != 0 { Some(w) } else { None }
}

/// Ask another launcher's window to show itself.
///
/// A private `_RGY_SHOW` `ClientMessage` is sent to `w` with `XSendEvent`
/// and a flush. The receiving pump maps its message type to `Event::Show`.
/// `runtime::display()` is the connection a second instance opened; when it
/// is null there is nothing to send on.
#[allow(dead_code)]
pub(crate) fn ask_to_show(w: u64) {
    let display = runtime::display();
    if display.is_null() {
        return;
    }
    let atom = intern(display, b"_RGY_SHOW\0");
    let mut ev = blank_event();
    xlib::set_client_message(&mut ev, w, atom, 32, &[0 as c_long; 5]);
    // SAFETY: `display` is live, `w` is a window on it, and `ev` is a
    // fully built ClientMessage whose union we hand to XSendEvent.
    unsafe {
        xlib::XSendEvent(display, w, 0, 0, &mut ev);
        xlib::XFlush(display);
    }
}

/// Intern one atom from a NUL-terminated byte string.
fn intern(display: *mut c_void, name: &[u8]) -> u64 {
    // SAFETY: `name` is a static NUL-terminated string XInternAtom only
    // reads; `0` asks the server to create the atom if it is absent.
    unsafe { xlib::XInternAtom(display, name.as_ptr() as *const c_char, 0) as u64 }
}

/// Set a format-32 `XA_ATOM` property from a slice of `c_ulong` words.
///
/// A format-32 property is carried in memory as `long` words, so the atom
/// list is handed to Xlib as `[c_ulong; N]` cast to a byte pointer with
/// `nelements = N` — exactly the shape XChangeProperty expects.
fn set_atoms(display: *mut c_void, win: u64, property: u64, atoms: &[c_ulong]) {
    // SAFETY: the handles are live and `atoms` outlives the call; the
    // words are the exact in-memory form of a format-32 property.
    unsafe {
        xlib::XChangeProperty(
            display,
            win,
            property as c_ulong,
            xlib::XA_ATOM as c_ulong,
            32 as c_int,
            xlib::PropModeReplace as c_int,
            atoms.as_ptr() as *const u8,
            atoms.len() as c_int,
        );
    }
}

/// A zeroed `XEvent`, ready for `XNextEvent` or `set_client_message`.
fn blank_event() -> xlib::XEvent {
    // SAFETY: XEvent is a union of `c_ulong` words with no invariant; all
    // zero is a valid value and is overwritten before it is read.
    unsafe { core::mem::zeroed() }
}

/// The work area as `(left, top, right, bottom)` for a screen of `w x h`.
///
/// X11 gives the launcher no taskbar insets, so the work area is the whole
/// screen. Factored out so the pure geometry can be tested on this host.
fn work_area_of(w: i32, h: i32) -> (i32, i32, i32, i32) {
    (0, 0, w, h)
}

/// The compositor's window-opacity property.
///
/// X11 has no layered window: translucency is a property the **compositor**
/// reads, so this is the same setting the Windows backend composes itself,
/// asked of the desktop. Without a compositor nothing reads it, which is why
/// the panel stays opaque there.
const OPACITY_ATOM: &[u8] = b"_NET_WM_WINDOW_OPACITY\0";

/// The desktop's work area, which is the screen minus its panels.
const WORKAREA_ATOM: &[u8] = b"_NET_WORKAREA\0";

/// Ask the compositor for a panel opacity, or drop the property when the
/// panel is meant to be stone.
///
/// The value is a 32-bit fraction of `0xffff_ffff`, which is what the
/// freedesktop specification says and what every compositor reads; `alpha`
/// is the model's 0..=255, so the two are the same number scaled.
fn apply_opacity(display: *mut c_void, win: u64, alpha: u32) {
    if display.is_null() || win == 0 {
        return;
    }
    let atom = intern(display, OPACITY_ATOM);
    if atom == 0 {
        return;
    }
    // SAFETY: the handles are the window's own, and the property is the one
    // Xlib is asked to set on the launcher's window.
    unsafe {
        // A `View` that has not yet been given a frame has alpha 0; that
        // means "no setting yet", not "one 255th opaque". Both ends of the
        // range mean a solid panel: drop the property so the compositor
        // has nothing to blend.
        if alpha <= 1 || alpha >= 255 {
            xlib::XDeleteProperty(display, win, atom);
            return;
        }
        let value: u64 = (alpha as u64) * 0xffff_ffff / 255;
        xlib::XChangeProperty(
            display,
            win,
            atom,
            xlib::XA_CARDINAL,
            32,
            xlib::PropModeReplace,
            (&value as *const u64).cast::<u8>(),
            1,
        );
    }
}

/// The desktop's work area, or the whole screen when it does not say.
///
/// `_NET_WORKAREA` is four cardinals — `x, y, width, height` — of the
/// region the desktop leaves for windows. A bare X server never sets it, so
/// the screen is the answer there.
fn work_area(display: *mut c_void, root: u64, screen: i32) -> (i32, i32, i32, i32) {
    if display.is_null() {
        return (0, 0, 0, 0);
    }
    // SAFETY: the display and root are live, every out-parameter is a live
    // local, and the returned property is freed once it is read.
    unsafe {
        let width = xlib::XDisplayWidth(display, screen);
        let height = xlib::XDisplayHeight(display, screen);
        let atom = intern(display, WORKAREA_ATOM);
        if atom == 0 {
            return (0, 0, width, height);
        }
        let mut actual_format: c_int = 0;
        let mut actual_type: xlib::Atom = 0;
        let mut nitems: c_ulong = 0;
        let mut bytes_after: c_ulong = 0;
        let mut data: *mut core::ffi::c_uchar = core::ptr::null_mut();
        let status = xlib::XGetWindowProperty(
            display,
            root,
            atom,
            0,
            4,
            0,
            xlib::XA_CARDINAL,
            &mut actual_format,
            &mut actual_type,
            &mut nitems,
            &mut bytes_after,
            &mut data,
        );
        if status != 0 || data.is_null() || nitems < 4 {
            if !data.is_null() {
                xlib::XFree(data.cast());
            }
            return (0, 0, width, height);
        }
        // A format-32 property arrives as an array of `c_ulong` words.
        let values = core::slice::from_raw_parts(data.cast::<c_ulong>(), 4);
        let area = (
            values[0] as i32,
            values[1] as i32,
            values[2] as i32,
            values[3] as i32,
        );
        xlib::XFree(data.cast());
        if area.2 <= 0 || area.3 <= 0 {
            (0, 0, width, height)
        } else {
            area
        }
    }
}

/// The work area the launcher settles on.
///
/// Kept apart from the read so the choice can be tested without a display:
/// reading is the platform's job and choosing is the model's.
fn work_area_of_screen(area: (i32, i32, i32, i32), w: i32, h: i32) -> (i32, i32, i32, i32) {
    if area.2 <= 0 || area.3 <= 0 {
        work_area_of(w, h)
    } else {
        area
    }
}
/// The `poll` timeout: the clock ticks at 120 ms while visible, and waits
/// forever when hidden.
fn tick_timeout(visible: bool, animating: bool) -> i32 {
    if !visible {
        -1
    } else if animating {
        // Fast enough that the ease reads as movement, and only while a
        // frame would actually differ.
        ANIMATION_MS
    } else {
        TICK_MS
    }
}

/// The pure message -> event decision: the event types that name a whole
/// launcher event without looking at any other field.
///
/// `ClientMessage`, `KeyPress` and `ButtonPress` need their atoms / keysyms
/// / tray and are handled in the pump; everything else is `None` here.
fn event_of_type(t: i32) -> Option<Event> {
    if t == k::DestroyNotify {
        Some(Event::Quit)
    } else if t == k::ConfigureNotify {
        Some(Event::Resize)
    } else {
        None
    }
}

/// Whether a `Paint` would draw the frame already on the screen.
///
/// The picture is a function of the view and the size. The view comparison
/// is the expensive, pure half; the caller adds the remembered model's
/// `(width, height)` comparison. A frame with no area was never drawn, so
/// repainting it is never a no-op.
fn same_frame(old: &View, new: &View, w: i32, h: i32) -> bool {
    if w <= 0 || h <= 0 {
        return false;
    }
    old == new
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The window opens centred across and in the upper third of the work
    /// area, the seat the Windows twin uses.
    #[test]
    fn default_anchor_is_the_upper_third() {
        assert_eq!(DEFAULT_ANCHOR, (500, 180));
    }

    /// A structural event type decides its launcher event; the ones that
    /// need more of the XEvent do not decide anything here.
    #[test]
    fn a_type_decides_its_event() {
        assert_eq!(event_of_type(crate::keys::DestroyNotify), Some(Event::Quit));
        assert_eq!(
            event_of_type(crate::keys::ConfigureNotify),
            Some(Event::Resize)
        );
        assert_eq!(event_of_type(crate::keys::Expose), None);
        assert_eq!(event_of_type(crate::keys::SelectionRequest), None);
    }

    /// The work area is the whole screen on X11.
    #[test]
    fn the_work_area_is_the_screen() {
        assert_eq!(work_area_of(1920, 1080), (0, 0, 1920, 1080));
        assert_eq!(work_area_of(800, 600), (0, 0, 800, 600));
    }

    /// A desktop that publishes `_NET_WORKAREA` gets it honoured, and one
    /// that publishes nothing gets the screen.
    #[test]
    fn the_desktops_area_wins_when_it_is_real() {
        assert_eq!(
            work_area_of_screen((10, 20, 900, 600), 1280, 800),
            (10, 20, 900, 600)
        );
        assert_eq!(
            work_area_of_screen((0, 0, 0, 0), 1280, 800),
            (0, 0, 1280, 800)
        );
        assert_eq!(
            work_area_of_screen((0, 0, -5, 20), 1920, 1080),
            (0, 0, 1920, 1080)
        );
    }

    /// A window placed on a short work area stays inside it.
    #[test]
    fn the_window_lands_inside_the_work_area() {
        let area = work_area_of_screen((0, 0, 1280, 500), 1280, 800);
        let spot = geometry::spot_on(area, 600, 89, (500, 180));
        assert!(spot.y >= 0, "never above the work area");
        assert!(spot.y + spot.height <= 500, "never below it");
    }

    /// The clock ticks only while the window is visible.
    #[test]
    fn the_clock_ticks_only_while_visible() {
        assert_eq!(tick_timeout(true, false), 120, "the slow clock");
        assert_eq!(tick_timeout(true, true), 16, "the animation's frame");
        assert_eq!(tick_timeout(false, true), -1, "hidden waits forever");
        assert_eq!(tick_timeout(false, false), -1);
    }

    /// A frame that has not changed is not painted again, and a frame with
    /// no area is never a no-op.
    #[test]
    fn an_unchanged_frame_is_not_painted_again() {
        let a = View::default();
        let b = View::default();
        assert!(same_frame(&a, &b, 600, 200));
        assert!(!same_frame(&a, &b, 0, 200));
        assert!(!same_frame(&a, &b, 600, 0));
    }
}
