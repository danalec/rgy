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
//! | backdrop | the window's own: a 32-bit premultiplied bitmap at ninety per cent opacity, so a tenth of the desktop shows through the near-black |
//! | shadow | the system's own drop shadow |
//! | type | Segoe UI Variable Text — Segoe UI where the platform has no variable face — and a heavier weight for the query |
//!
//! The backdrop is the window's own because the platform was asked and it
//! has none to give here: the Win11 host backdrop comes back untinted and
//! sharp, the blur-behind accent comes back ninety-six per cent opaque and
//! breaks the text GDI draws, and `DwmEnableBlurBehindWindow` paints a
//! white void. A window that believes in a blur that is not there is a
//! window that shows the screen it was opened on, so the panel carries its
//! own alpha — and the system's own shadow is left where it was.
//!
//! # How it behaves
//!
//! The window never takes the focus away from the window underneath for
//! longer than it is on screen, and it is always above: `WS_EX_TOPMOST`
//! and `WS_EX_TOOLWANDOW`, so it is not in the taskbar and not in the
//! alt-tab list. The keyboard focus it *does* take is the price of a
//! search box, and it gives it back the moment it is hidden.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_void;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, Ordering};

use rgy_launcher::latency::Latency;
use rgy_launcher::layout::{BASE_DPI, SCALE_PERCENT, device_pixels, model_pixels};
use rgy_launcher::text::{Lang, Text};
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
    /// Ctrl+V: paste the clipboard's text into the query.
    CtrlV,
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
    /// The search window was asked for: show it, and leave it up.
    ///
    /// The icon in the notification area asks with this and not with a
    /// toggle. A double click is two clicks, and a toggle would show the
    /// window and hide it again — a window that flickers is a window that
    /// is not there — and the shell sends the same click twice when the
    /// user double-clicks.
    Show,
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
    /// Where the caret is, counted in characters from the start of the
    /// query.
    ///
    /// The window used to draw the caret after the last character, always,
    /// and a caret that is always at the end is a decoration: the model
    /// moves it with the arrow keys, and a paste lands where it is.
    pub caret: usize,
    /// The line the empty field shows.
    ///
    /// The word is the vocabulary's, and the window fills it with the
    /// word of the language it speaks: the host owns the rows and the
    /// query, and the words are the window's.
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
    /// How long the answers take.
    ///
    /// The figures are the model's: the meter lives in the model and the
    /// window draws what the model measured. An empty figure is not a
    /// measurement — before the first answer there is nothing to show,
    /// and a window that says "0 ms" then is a window that reports a
    /// measurement it never took.
    pub latency: Latency,
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

/// What the desktop window manager said about the window's own picture.
///
/// A layered window is composited by the compositor, and the compositor
/// has the last word: a picture it refuses leaves the window with nothing
/// to show, and a window with nothing to show is a launcher the user
/// cannot see.
///
/// The answer is asked while the window is still hidden, because there is
/// no second chance to find out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compositor {
    /// Nothing was handed to it yet.
    Unknown,
    /// It took the picture.
    Accepted,
    /// It threw the picture away.
    Refused,
}

/// What the process knows about the screen's scale.
///
/// A process that does not know it has its windows virtualized: the
/// platform stretches the picture, and every scale the launcher works out
/// is the system's instead of the monitor's. The launcher still works --
/// it is the picture that is stretched -- and the answer is kept so that
/// the line under the field can say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Awareness {
    /// The process knows the scale of the monitor the window is on.
    PerMonitor,
    /// The process knows the system's scale, and the platform stretches
    /// the window when the monitor's is another.
    System,
    /// The process does not know the scale at all.
    Unaware,
}

/// The state the window's procedure reads.
struct State {
    view: View,
    /// The language the window is speaking.
    ///
    /// The words the window draws come from the vocabulary, and the
    /// vocabulary has a column per language: the window holds the
    /// language it was given and asks the table for the word it needs.
    lang: Lang,
    /// The geometry the last paint used, so a resize knows what changed.
    painted: (i32, i32),
    /// Whether the platform really gave the window its blur.
    ///
    /// The blur is behind the window, so the painting has to leave the
    /// client area alone where there is one — and where there is not,
    /// the window has to paint its own near-black.
    backdrop: bool,
    /// What the compositor said about the window's own picture.
    ///
    /// The window paints its own alpha and the compositor is the one that
    /// puts it on the screen: a picture it refuses leaves the window with
    /// nothing to show, and a window with nothing to show is a launcher
    /// the user cannot see.
    compositor: Compositor,
    /// What the process knows about the screen's scale.
    awareness: Awareness,
    /// The window's own pixels, when the platform gave no backdrop.
    ///
    /// The shadow is still the system's own: the window is created
    /// without the layered style and made layered afterwards, and the
    /// ring around it measures the same as it did when the window was
    /// opaque.
    layer: Option<Layer>,
    /// Whether the window hid itself.
    ///
    /// A window that hides itself makes the application inactive, and an
    /// inactive application says goodbye again -- and answering that would
    /// open the window the user just closed. The flag tells the two
    /// apart: the window says goodbye, or the user says it.
    hiding: bool,
    /// The user went somewhere else and the window has to get out of the
    /// way.
    ///
    /// The answer is given by the message pump and not by the window
    /// procedure: the message can arrive while the host is inside a
    /// platform call -- opening a file changes the activation -- and
    /// answering it from there would ask the host for the state it is
    /// already using.
    focus_lost: bool,
    /// The icons, one per kind, resolved the first time they are needed.
    icons: Icons,
    /// The window's fonts, made once and deleted with the window.
    fonts: Fonts,
    /// The scale of the monitor the window is on, in per cent.
    ///
    /// The picture is drawn in device pixels and the drawing is in the
    /// model's units: the scale is what tells the two apart.
    scale: u32,
    /// The half of a character the platform is sending in two pieces.
    ///
    /// A character outside the Basic Multilingual Plane is two UTF-16
    /// code units, and the platform sends them as two messages. The
    /// window keeps the first half until the second one arrives: a
    /// surrogate is not a scalar value, and a window that reads each
    /// message on its own drops the character — the field would not
    /// move while the user typed an emoji.
    pending_char: Option<u16>,
    /// Why the notification area has no icon, when it has none.
    ///
    /// The launcher works without the icon — the hotkey does not go through
    /// the notification area — so a launcher that could not have the icon
    /// is not a launcher that could not start. But it IS a launcher with no
    /// face, and the reason is kept so that the question can be asked
    /// instead of guessed at.
    tray_error: Option<String>,
    /// The mouse's message the shell sent, waiting for the pump.
    ///
    /// The shell **sends** the notification area's clicks to the window's
    /// procedure: the message is not posted, so it never comes out of the
    /// message queue and the pump never sees it. The procedure is the only
    /// place it arrives, so the procedure keeps it and the pump asks for it.
    ///
    /// The mouse's own message is in the low word of the word the shell
    /// sent — the icon's place is in the high word — and the word is taken
    /// out before it is read.
    tray_click: Option<UINT>,
}

/// The window's own pixels, for a window that paints its own alpha.
///
/// A layered window is composited by itself: the desktop window manager
/// does not paint behind it, and the window's own bitmap carries the
/// alpha that the compositor uses. The bitmap is the panel and nothing
/// else — the rounded corners are the region's own, and the shadow is
/// the system's own — and the alpha of every pixel is worked out after
/// the drawing, because GDI does not write the alpha channel of a
/// 32-bit bitmap.
///
/// The platform's own backdrops are better when there are any — they
/// come with the platform's blur — but on this host there are none, and
/// a window that believes in a blur that is not there is a window that
/// shows the screen it was opened on.
struct Layer {
    /// The context the panel is drawn into.
    dc: HDC,
    /// The bitmap that context holds.
    bitmap: HANDLE,
    /// The bitmap the context held before this one.
    ///
    /// A bitmap that is selected into a context cannot be deleted, and a
    /// null handle does not deselect it: the context goes back to this
    /// one before the panel's own bitmap is given back, or the panel
    /// leaks -- with its pixels -- every time the window is closed.
    previous: HANDLE,
    /// The bitmap's pixels, in the order the platform stores them.
    bits: *mut u8,
    /// The panel's width.
    width: i32,
    /// The panel's height.
    height: i32,
}

/// The panel's own alpha: ninety per cent.
///
/// Almost black and slightly transparent: a tenth of the desktop shows
/// through, and the panel keeps its colour where the desktop is dark.
/// It is the alpha that lets the desktop through where the panel draws
/// nothing — the rounded corners are the region's own, and the shadow is
/// the system's own, and the alpha is what is left of the panel.
const PANEL_ALPHA: u8 = 230;

/// How far a pixel may be from the panel's own colour and still be the
/// panel's own colour.
///
/// The text is drawn with antialiasing, and its faintest edges are a
/// step away from the panel's colour. They are the panel's colour, and
/// they keep the panel's alpha: a text edge that is suddenly opaque
/// draws a dark line around every letter.
const PANEL_TOLERANCE: i32 = 8;

/// The window composites itself: its own pixels carry their own alpha.
const WS_EX_LAYERED: u32 = 0x0008_0000;

/// The window's own styles.
const GWL_EXSTYLE: i32 = -20;

/// The alpha of the window's own pixels is the alpha of the window.
const AC_SRC_ALPHA: u8 = 1;

/// The window is put on the screen with the alpha of its own pixels.
const ULW_ALPHA: u32 = 0x0000_0002;

impl Layer {
    /// A layer for a panel of this size.
    ///
    /// The bitmap is the panel's own size: the rounded corners are the
    /// region's, and the shadow is the system's, and the window's own
    /// pixels are only the panel.
    fn new(width: i32, height: i32) -> Option<Layer> {
        unsafe {
            let screen = GetDC(core::ptr::null_mut());
            if screen.is_null() {
                return None;
            }
            let dc = CreateCompatibleDC(screen);
            ReleaseDC(core::ptr::null_mut(), screen);
            if dc.is_null() {
                return None;
            }
            let (bitmap, bits, previous) = Layer::bitmap(dc, width, height);
            if bitmap.is_null() {
                DeleteDC(dc);
                return None;
            }
            Some(Layer {
                dc,
                bitmap,
                previous,
                bits,
                width,
                height,
            })
        }
    }

    /// The bitmap the layer's context holds, for a panel of this size.
    ///
    /// The height is negative so that the first row of the bitmap is the
    /// top of the panel: a bitmap that grows upwards would put the
    /// panel's own picture upside down.
    ///
    /// The handle that was in the context comes back with it: the panel's
    /// own bitmap cannot be deleted while it is selected, so the context
    /// has to go back to what it held before.
    fn bitmap(dc: HDC, width: i32, height: i32) -> (HANDLE, *mut u8, HANDLE) {
        let info = Layer::info(width, height);
        let mut bits: *mut c_void = core::ptr::null_mut();
        let bitmap = unsafe {
            CreateDIBSection(
                dc,
                &info,
                DIB_RGB_COLORS,
                &mut bits,
                core::ptr::null_mut(),
                0,
            )
        };
        let mut previous = core::ptr::null_mut();
        if !bitmap.is_null() {
            previous = unsafe { SelectObject(dc, bitmap) };
        }
        (bitmap, bits.cast::<u8>(), previous)
    }

    /// The bitmap's own information: the header, and the room for the
    /// colours that a 32-bit bitmap does not need.
    fn info(width: i32, height: i32) -> BitmapInfo {
        BitmapInfo {
            header: BitmapInfoHeader {
                size: core::mem::size_of::<BitmapInfoHeader>() as u32,
                width,
                height: -height,
                planes: 1,
                bits: 32,
                compression: BI_RGB,
                image_size: 0,
                x_pixels_per_meter: 0,
                y_pixels_per_meter: 0,
                colors_used: 0,
                colors_important: 0,
            },
            colors: [0; 1],
        }
    }

    /// Give the layer a panel of another size.
    ///
    /// The bitmap is made again: the platform's bitmaps are of one size,
    /// and a launcher that is resized is a launcher that is drawn
    /// again.
    fn resize(&mut self, width: i32, height: i32) {
        if width == self.width && height == self.height {
            return;
        }
        unsafe {
            let (bitmap, bits, previous) = Layer::bitmap(self.dc, width, height);
            if bitmap.is_null() || previous.is_null() {
                // The context did not take the new bitmap: the panel keeps
                // the one it has.
                if !bitmap.is_null() {
                    DeleteObject(bitmap);
                }
                return;
            }
            // The old bitmap is given back only after the new one is in
            // the context: a bitmap that is still selected cannot be
            // deleted. The context held the old panel's bitmap, so
            // putting the new one in deselected it -- and the old one can
            // go back now.
            let old = core::mem::replace(&mut self.bitmap, bitmap);
            DeleteObject(old);
            self.bits = bits;
        }
        self.width = width;
        self.height = height;
    }

    /// Put the window on the screen.
    ///
    /// The panel is drawn into the layer's own context, the alpha is
    /// worked out from what was drawn, and the whole thing is handed to
    /// the compositor: that is what a layered window is.
    ///
    /// The answer is the compositor's own: it took the picture or it
    /// threw it away, and a window whose picture was thrown away shows
    /// nothing at all. False says that.
    ///
    /// The picture is the window's own, and the words on it are the
    /// window's: a struct would be a struct that only this function and
    /// its caller ever see.
    #[allow(clippy::too_many_arguments)]
    fn present(
        &mut self,
        hwnd: HWND,
        view: &View,
        painted: &mut (i32, i32),
        icons: &mut Icons,
        fonts: &Fonts,
        scale: u32,
        lang: Lang,
    ) -> bool {
        unsafe {
            // The panel's own near-black is what the desktop shows
            // through.
            paint::paint(self.dc, view, painted, false, icons, fonts, scale, lang);
            // The alpha is worked out after the drawing: GDI writes the
            // colour of a pixel and never its alpha.
            self.compose();
            let screen = GetDC(core::ptr::null_mut());
            if screen.is_null() {
                return false;
            }
            let mut rect = RECT::default();
            GetWindowRect(hwnd, &mut rect);
            let dest = POINT {
                x: rect.left,
                y: rect.top,
            };
            let size = Size {
                width: self.width,
                height: self.height,
            };
            let source = POINT { x: 0, y: 0 };
            let blend = BlendFunction {
                operation: 0,
                flags: 0,
                constant_alpha: 255,
                alpha_format: AC_SRC_ALPHA,
            };
            let took = UpdateLayeredWindow(
                hwnd, screen, &dest, &size, self.dc, &source, 0, &blend, ULW_ALPHA,
            ) != 0;
            ReleaseDC(core::ptr::null_mut(), screen);
            took
        }
    }

    /// The alpha the window's own pixels carry.
    ///
    /// The panel's own near-black is nearly opaque and the desktop shows
    /// through the rest of it; what the panel draws on it — the border,
    /// the query, the rows — is opaque, so the text keeps its edges.
    fn compose(&self) {
        let width = self.width;
        let height = self.height;
        let stride = (width * 4) as usize;
        unsafe {
            for y in 0..height {
                for x in 0..width {
                    let at = (y as usize) * stride + (x as usize) * 4;
                    let pixel = self.bits.add(at);
                    if is_the_panel(*pixel, *pixel.add(1), *pixel.add(2)) {
                        // The panel's own colour: nearly opaque, and the
                        // colour it carries is the colour it has with
                        // that alpha in it.
                        *pixel = premultiplied(paint::colour::BLUE, PANEL_ALPHA);
                        *pixel.add(1) = premultiplied(paint::colour::GREEN, PANEL_ALPHA);
                        *pixel.add(2) = premultiplied(paint::colour::RED, PANEL_ALPHA);
                        *pixel.add(3) = PANEL_ALPHA;
                    } else {
                        // What is drawn on the panel is opaque.
                        *pixel.add(3) = 255;
                    }
                }
            }
        }
    }
}

impl Drop for Layer {
    fn drop(&mut self) {
        unsafe {
            // The context goes back to the bitmap it held before the
            // panel's own: a bitmap that is selected cannot be deleted,
            // and a null handle does not deselect it -- the panel's
            // bitmap and its pixels would leak with it.
            if !self.previous.is_null() {
                SelectObject(self.dc, self.previous);
            }
            DeleteObject(self.bitmap);
            DeleteDC(self.dc);
        }
    }
}

/// Whether a pixel is the panel's own colour, or one step away from it.
///
/// The text's faintest edges are the panel's colour: they are the
/// panel, and they keep the panel's alpha.
fn is_the_panel(blue: u8, green: u8, red: u8) -> bool {
    let near = |value: u8, panel: u8| (value as i32 - panel as i32).abs() <= PANEL_TOLERANCE;
    near(blue, paint::colour::BLUE)
        && near(green, paint::colour::GREEN)
        && near(red, paint::colour::RED)
}

/// A colour with the alpha it carries already in it.
///
/// The compositor reads a layered window's bitmap as premultiplied: the
/// colour of a pixel that is half transparent is half as bright.
fn premultiplied(colour: u8, alpha: u8) -> u8 {
    ((colour as u32 * alpha as u32) / 255) as u8
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
    /// The window's width, in device pixels.
    width: i32,
    /// The window's height, in device pixels.
    height: i32,
    /// The size the model asked for, in the model's own units.
    ///
    /// The model speaks in the units of a 96-DPI screen and the window
    /// is in device pixels: the scale is what tells the two apart, and
    /// the model's size is kept so that the window can be sized again
    /// when the scale changes.
    model: (i32, i32),
    /// The scale of the monitor the window is on, in per cent.
    scale: u32,
    /// The icon in the notification area, when the shell took it.
    ///
    /// The window is a tool window on purpose — it is not in the taskbar
    /// and not in the alt-tab list — so the icon is the launcher's face
    /// while it waits. The shell can refuse it, and a launcher whose face
    /// was refused still answers the keyboard: the hotkey does not go
    /// through the notification area, and a launcher that refused to start
    /// because a tray was busy would be a worse answer than no icon.
    tray: Option<crate::tray::Tray>,
}

impl Window {
    /// Build the window, hidden.
    ///
    /// It is not shown until [`Window::show`]: a window that appears and
    /// disappears at start-up is a window the user did not ask for.
    pub fn new(title: &str) -> Result<Window, String> {
        // The process has to know the monitor's scale before the first
        // window exists: an awareness set after the window is on the
        // screen is not an awareness the window can use.
        //
        // The call can fail because somebody else set the awareness
        // already, and that is not an error to report: the platform's
        // own answer about the monitor is read anyway, and the window is
        // drawn at that scale.
        let awareness = unsafe {
            // The answer is acted upon: a process that does not know the
            // screen's scale has its window virtualized, and a virtualized
            // window is a stretched picture. The older call knows the
            // scale too, and it is the way out on a platform that does
            // not know the newer one.
            if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) != 0 {
                Awareness::PerMonitor
            } else if SetProcessDPIAware() != 0 {
                Awareness::System
            } else {
                Awareness::Unaware
            }
        };
        register_class()?;
        let mut title16: Vec<u16> = title.encode_utf16().collect();
        title16.push(0);

        // The state is a heap allocation and the window's user data
        // holds the pointer to it: the window procedure reaches the
        // state there, and the window reaches it through the same
        // pointer.
        let raw: *mut State = Box::into_raw(Box::new(State {
            view: View {
                placeholder: Text::for_lang(Lang::English).placeholder.to_string(),
                ..View::default()
            },
            lang: Lang::English,
            painted: (0, 0),
            backdrop: false,
            compositor: Compositor::Unknown,
            awareness,
            layer: None,
            hiding: false,
            focus_lost: false,
            icons: Icons::new(),
            fonts: Fonts::new(SCALE_PERCENT),
            scale: SCALE_PERCENT,
            pending_char: None,
            tray_error: None,
            tray_click: None,
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
                // Not layered: the backdrop is the desktop window
                // manager's, and it draws it in the window's own
                // background — where the window leaves it alone. A
                // layered window would be composited by itself, and the
                // window that the compositor puts on the screen is the
                // one the user is looking at.
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
                let code = GetLastError();
                return Err(format!("the window could not be created ({code})"));
            }
            SetWindowLongPtrW(hwnd, GWL_USERDATA, state.as_ptr() as isize);
            let mut w = Window {
                hwnd,
                state,
                width: 600,
                height: 80,
                model: (600, 80),
                scale: SCALE_PERCENT,
                tray: None,
            };
            // The icon goes into the notification area now that the window
            // exists: the shell's record holds the window's own handle, and
            // a record with no window behind it is a record of nothing.
            match crate::tray::Tray::new(hwnd, Text::for_lang(Lang::English)) {
                Ok(tray) => w.tray = Some(tray),
                Err(why) => w.state_mut().tray_error = Some(why),
            }
            // The window exists: the platform says what a unit is worth
            // on the monitor it is on, and the picture is drawn at that
            // size.
            let scale = scale_of(GetDpiForWindow(hwnd));
            w.scale = scale;
            w.state_mut().scale = scale;
            // The fonts are made again: a font is a size in device
            // pixels, and the provisional ones were made for a screen
            // that may not be the one the window is on.
            w.state_mut().fonts = Fonts::new(scale);
            let (width, height) = w.device_size();
            w.width = width;
            w.height = height;
            // And the window itself is sized at that scale: it was made
            // with the model's numbers, and the model does not know the
            // scale.
            w.resize(600, 80);
            // The backdrop says what the painting has to do: with one, the
            // client area is left alone for the blur to show through.
            let backdrop = w.apply_backdrop();
            w.state_mut().backdrop = backdrop;
            if !backdrop {
                // The platform gave the window no backdrop: the window
                // paints its own alpha, and the desktop shows through it.
                w.paint_its_own_alpha();
            }
            w.apply_region();
            // The compositor's answer, asked while the window is still
            // hidden: a window that is up and has no picture is a
            // launcher the user cannot see, and the window is never
            // hidden again once the user has asked for it.
            w.ask_the_compositor();
            Ok(w)
        }
    }

    /// The window, speaking another language.
    ///
    /// The window is built in English — the language the code is written
    /// in — and the product is not: the launcher was made with
    /// Portuguese words on purpose, and the words are in the vocabulary,
    /// one column per language. The window is told which column to read.
    ///
    /// The line the empty field shows changes with it: a window whose
    /// placeholder stayed in the other language would be a window the
    /// user reads in two at once.
    pub fn with_lang(mut self, lang: Lang) -> Window {
        self.state_mut().lang = lang;
        self.state_mut().view.placeholder = Text::for_lang(lang).placeholder.to_string();
        // The icon's tip is a word the user reads too, and the shell is
        // showing the old language's one until it is told otherwise.
        if let Some(tray) = self.tray.as_mut() {
            tray.set_tip(Text::for_lang(lang).tray_tip);
        }
        self
    }

    /// Why the notification area has no icon, when it has none.
    ///
    /// The launcher works without the icon — the hotkey does not go through
    /// the notification area — so the reason is not a failure the user is
    /// shown. It is the answer to "why is there no icon?", which is a
    /// question a reader of the tree can ask and a test can check.
    pub fn tray_error(&self) -> Option<&str> {
        self.state().tray_error.as_deref()
    }

    /// Make the window composite itself.
    ///
    /// A window that paints its own alpha is composited by itself: the
    /// desktop window manager does not paint behind it, and what the
    /// user sees is the window's own pixels and the alpha they carry.
    ///
    /// The window is still hidden here: the question of a backdrop was
    /// asked while it was, and the answer is what makes the window
    /// layered or not.
    fn paint_its_own_alpha(&mut self) {
        // The layer comes first: a window that composites itself and has
        // no pixels of its own is a window the user cannot see.
        let Some(layer) = Layer::new(self.width, self.height) else {
            return;
        };
        self.state_mut().layer = Some(layer);
        unsafe {
            let style = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE) as u32;
            SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, (style | WS_EX_LAYERED) as isize);
        }
    }

    /// Ask the compositor whether it takes the window's own picture.
    ///
    /// The question is asked with a real picture, while the window is
    /// still hidden: the layer is painted once and handed over, and the
    /// answer is the one the compositor gives.
    ///
    /// When the answer is no, the window gives the layered surface up and
    /// paints itself into its own device context. The translucency is
    /// lost -- the desktop stops showing through the panel -- and the
    /// launcher is still there, which is the trade the window makes: a
    /// window the user can see without its translucency beats a window
    /// the user cannot see with it.
    fn ask_the_compositor(&mut self) {
        let hwnd = self.hwnd;
        let state = self.state_mut();
        let took = match state.layer.as_mut() {
            Some(layer) => layer.present(
                hwnd,
                &state.view,
                &mut state.painted,
                &mut state.icons,
                &state.fonts,
                state.scale,
                state.lang,
            ),
            // There is no layer: the window paints itself, and the
            // compositor has nothing to refuse.
            None => true,
        };
        state.compositor = if took {
            Compositor::Accepted
        } else {
            Compositor::Refused
        };
        if took {
            return;
        }
        // The surface goes, and the layered style with it: a window that
        // is layered and has no surface shows nothing at all. The window
        // is hidden here, and a window that has never been shown can be
        // un-layered -- the style is a promise the compositor keeps for
        // the windows that are already on the screen.
        state.layer = None;
        state.backdrop = false;
        unsafe {
            let style = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE) as u32;
            SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, (style & !WS_EX_LAYERED) as isize);
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

    /// How tall the work area is, in the model's own units.
    ///
    /// The host needs it to know how many rows the window can show: the
    /// model sizes the window from the rows, and the rows have to fit
    /// between the top of the work area and the bottom of the screen.
    /// The work area and not the screen, because the taskbar is not room
    /// for rows.
    ///
    /// The answer is in the model's units and not in device pixels: the
    /// rows are counted in them, and a screen that is twice as big in
    /// device pixels is not twice as many rows.
    ///
    /// The answer is never zero: a host that read zero would show a
    /// single row.
    pub fn work_area_height(&self) -> u32 {
        let work = self.target_work_area();
        let height = work.bottom - work.top;
        if height > 0 {
            return model_pixels(height as u32, self.scale);
        }
        // The platform would not say, and the screen is the answer that
        // is always there.
        unsafe { model_pixels(GetSystemMetrics(SM_CYSCREEN).max(0) as u32, self.scale) }
    }

    /// What the window draws.
    pub fn view(&self) -> &View {
        &self.state().view
    }

    /// What the window draws, changed.
    pub fn set_view(&mut self, view: View) {
        self.install(view);
    }

    /// The view the window draws, in the language the window speaks.
    ///
    /// The placeholder is the word of that language: the host owns the
    /// rows and the query, and a window that showed its placeholder in
    /// another language would be a window the user reads in two
    /// languages at once.
    fn install(&mut self, mut view: View) {
        view.placeholder = Text::for_lang(self.state().lang).placeholder.to_string();
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
        // A window that is up is not hiding: the goodbye it says when it
        // goes away belongs to this showing, not to the next one. And a
        // window that is up is not leaving either.
        self.state_mut().hiding = false;
        self.state_mut().focus_lost = false;
        unsafe {
            self.place();
            ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            SetForegroundWindow(self.hwnd);
            SetFocus(self.hwnd);
            // The window is invalidated before it is updated: a window
            // that composites itself has no backing store for the
            // system to mark as dirty, and a window that is never
            // painted is a window the user cannot see.
            InvalidateRect(self.hwnd, core::ptr::null_mut(), 0);
            UpdateWindow(self.hwnd);
            self.start_clock();
        }
    }

    /// Hide the window. The process stays.
    pub fn hide(&mut self) {
        // The window is the one that is going away, and it says so.
        self.state_mut().hiding = true;
        // A half character is not a character, and it does not survive
        // the session that typed it.
        self.state_mut().pending_char = None;
        // And it is not the user who left: the window is going, so the
        // answer to "the user left" is stale.
        self.state_mut().focus_lost = false;
        unsafe {
            ShowWindow(self.hwnd, SW_HIDE);
        }
        self.stop_clock();
    }

    /// Whether the window is on the screen.
    pub fn is_visible(&self) -> bool {
        unsafe { IsWindowVisible(self.hwnd) != 0 }
    }

    /// What the compositor said about the window's own picture.
    ///
    /// The answer is what tells the launcher that the screen is the one
    /// that is not cooperating: a window that is up and has no picture is
    /// a launcher the user cannot see, and a launcher that says nothing
    /// about it looks broken.
    pub fn compositor(&self) -> Compositor {
        self.state().compositor
    }

    /// What the process knows about the screen's scale.
    ///
    /// The answer is what tells the launcher that the picture it draws
    /// is going to be stretched: a window on a screen whose scale the
    /// process does not know is a window the platform draws at the
    /// system's scale and then stretches.
    pub fn awareness(&self) -> Awareness {
        self.state().awareness
    }

    /// Whether the window is on the screen.
    ///
    /// The three answers the platform gives: the window is visible, the
    /// compositor is not hiding it, and the bounds the compositor draws
    /// are the bounds the window has. A window that fails any of them is
    /// a window the user cannot see, and a launcher that is up and
    /// invisible looks broken -- so the launcher asks.
    ///
    /// This is the honest measurement on a host whose screen capture does
    /// not carry the windows that are put on top of it: a picture of the
    /// screen cannot answer this, and the platform can.
    pub fn is_on_the_screen(&self) -> bool {
        unsafe {
            if IsWindowVisible(self.hwnd) == 0 {
                return false;
            }
            // The compositor is not hiding it.
            let mut cloaked: i32 = 0;
            let asked = DwmGetWindowAttribute(
                self.hwnd,
                DWMWA_CLOAKED,
                (&mut cloaked as *mut i32).cast::<core::ffi::c_void>(),
                core::mem::size_of::<i32>() as u32,
            );
            if asked != 0 || cloaked != 0 {
                return false;
            }
            // And it is where it says it is.
            let mut drawn = RECT::default();
            let asked = DwmGetWindowAttribute(
                self.hwnd,
                DWMWA_EXTENDED_FRAME_BOUNDS,
                (&mut drawn as *mut RECT).cast::<core::ffi::c_void>(),
                core::mem::size_of::<RECT>() as u32,
            );
            if asked != 0 {
                return false;
            }
            let mut own = RECT::default();
            GetWindowRect(self.hwnd, &mut own);
            (drawn.left, drawn.top, drawn.right, drawn.bottom)
                == (own.left, own.top, own.right, own.bottom)
        }
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
        // window's shape: it has to follow. The layer is the panel's own
        // bitmap, and a panel of another size is a panel of another
        // bitmap.
        self.width = spot.width;
        self.height = spot.height;
        self.apply_region();
        let (width, height) = (self.width, self.height);
        if let Some(layer) = self.state_mut().layer.as_mut() {
            layer.resize(width, height);
        }
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
        // The window was made on the monitor the process started on and
        // moved to the monitor the user is looking at: a monitor has a
        // scale of its own, and the picture has to be drawn at it.
        self.follow_the_monitor();
    }

    /// Take the scale of the monitor the window is on.
    ///
    /// The window moves between monitors, and a monitor with another
    /// scale gives the window another scale: the fonts are made again —
    /// a font is a size in device pixels, and the size of the old
    /// monitor's is not the size of this one's — and the window is
    /// sized again at the new scale.
    fn follow_the_monitor(&mut self) {
        let dpi = unsafe { GetDpiForWindow(self.hwnd) };
        if dpi == 0 {
            // The platform would not say: the scale stays the one the
            // window was made with.
            return;
        }
        let scale = scale_of(dpi);
        if scale == self.scale {
            return;
        }
        self.scale = scale;
        self.state_mut().scale = scale;
        self.state_mut().fonts = Fonts::new(scale);
        // And the window is sized again, at the new scale.
        let (width, height) = self.model;
        self.resize(width, height);
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
    ///
    /// The numbers are the model's own — the units of a 96-DPI screen —
    /// and the window is in device pixels: the window multiplies them by
    /// its scale, and the picture is drawn at that size.
    pub fn resize(&mut self, width: i32, height: i32) {
        self.model = (width, height);
        let (width, height) = self.device_size();
        // The window is never taller than the room the work area has: the
        // model sizes it from the rows, and the rows have to fit between
        // the top of the area and the bottom of the screen.
        let spot = spot_on(self.target_work_area(), width, height);
        if spot.width == self.width && spot.height == self.height {
            return;
        }
        self.width = spot.width;
        self.height = spot.height;
        self.apply_region();
        // The layer is the panel's own bitmap: a panel of another size
        // is a panel of another bitmap.
        let (width, height) = (self.width, self.height);
        if let Some(layer) = self.state_mut().layer.as_mut() {
            layer.resize(width, height);
        }
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
                width,
                height,
                SWP_NOMOVE | SWP_NOACTIVATE,
            );
        }
    }

    /// The window's size in device pixels, at this screen's scale.
    ///
    /// The model's size is in the units of a 96-DPI screen and the
    /// window is in device pixels, and the scale is what tells the two
    /// apart.
    fn device_size(&self) -> (i32, i32) {
        (
            device_pixels(self.model.0.unsigned_abs(), self.scale) as i32,
            device_pixels(self.model.1.unsigned_abs(), self.scale) as i32,
        )
    }

    /// The acrylic backdrop, when the platform has one.
    ///
    /// Win11's host backdrop is the blur behind the window, and the
    /// accent's gradient colour is the tint over it: 60% black, which is
    /// what "almost black, slightly transparent" is made of. On a platform
    /// without it the window paints its own near-black — translucent, so
    /// the desktop shows through — and the only difference the user sees
    /// is that nothing moves behind it.
    ///
    /// The tint is asked for anyway: it costs nothing, and a platform
    /// that honours it is the platform the window was written for. It was
    /// measured on this one and it is ignored — the material comes back
    /// untinted — which is one of the reasons the window does not lean
    /// on it.
    ///
    /// The answer is whether the backdrop is really there, and the accent
    /// cannot answer it: the call is undocumented, the platform is free to
    /// accept it and still have no blur to give — and on the platform that
    /// took the accent away the window is swallowed whole, not blurred.
    /// The desktop window manager's own answer is about the documented
    /// backdrop, which is a different one, so it cannot confirm the
    /// accent either. What it can say is that there is no backdrop here.
    ///
    /// So the window believes the platform only when the platform says
    /// there is something, and otherwise takes the accent back and paints
    /// its own near-black. A window that believes in a blur that is not
    /// there is a window that paints nothing at all and shows whatever the
    /// screen held before — and a window that the user cannot see is not a
    /// launcher.
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
            // The frame is extended into the client area **before** the
            // accent is asked for: the accent is painted into the frame,
            // and a window whose frame stops at the client area's edge
            // has nowhere to paint it. The window then paints nothing
            // over its own background — that is what "letting the blur
            // through" means — and a window with a backdrop that never
            // arrived shows the screen it was opened on.
            let margins = MARGINS {
                cx_left_width: -1,
                cy_top_height: -1,
                cx_right_width: -1,
                cy_bottom_height: -1,
            };
            DwmExtendFrameIntoClientArea(self.hwnd, &margins);
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
            // "almost black, slightly transparent" is made of.
            //
            // Both are tried, and the one that is accepted is the one
            // the platform has — the answer is not a policy, it is what
            // the platform says it can do.
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
                    // The accent was accepted. That is not the same thing
                    // as a blur: the manager is asked what it gave.
                    if self.has_backdrop() {
                        return true;
                    }
                    // Accepted and nothing behind it: the other state is
                    // tried as well, because a platform may have the one
                    // and not the other — and the one it has is the one
                    // that counts.
                    continue;
                }
            }
            // The accent was accepted and there is no backdrop behind it.
            // The window takes both back: an accent with nothing to show
            // is a window whose own painting the manager throws away, and
            // the screen it was opened on takes its place.
            self.take_back_backdrop(set);
            false
        }
    }

    /// Take back what the attempt at a backdrop did.
    ///
    /// The accent off and the frame where it was. The frame covers the
    /// client area while the backdrop is there; without a backdrop the
    /// frame is a frame over a window that cannot paint.
    fn take_back_backdrop(&self, set: SetWindowCompositionAttributeFn) {
        unsafe {
            let margins = MARGINS {
                cx_left_width: 0,
                cy_top_height: 0,
                cx_right_width: 0,
                cy_bottom_height: 0,
            };
            DwmExtendFrameIntoClientArea(self.hwnd, &margins);
            let mut accent = AccentPolicy {
                accent_state: ACCENT_ENABLE_NONE,
                accent_flags: 0,
                gradient_color: 0,
                animation_id: 0,
            };
            let mut data = WindowCompositionAttributeData {
                attribute: WCA_ACCENT_POLICY,
                data: (&mut accent as *mut AccentPolicy).cast::<c_void>(),
                size_of_data: core::mem::size_of::<AccentPolicy>(),
            };
            set(self.hwnd, &mut data);
        }
    }

    /// Whether the desktop window manager really gave this window a
    /// backdrop to show.
    ///
    /// The question has an answer only on the platforms that have
    /// backdrops at all; everywhere else it is refused, and a refused
    /// question is a "no".
    fn has_backdrop(&self) -> bool {
        unsafe {
            let mut kind: i32 = DWMSBT_AUTO;
            let asked = DwmGetWindowAttribute(
                self.hwnd,
                DWMWA_SYSTEM_BACKDROP_TYPE,
                (&mut kind as *mut i32).cast::<c_void>(),
                core::mem::size_of::<i32>() as u32,
            );
            if asked != 0 {
                // The platform does not answer: it has no backdrops.
                return false;
            }
            backdrop_is_there(kind)
        }
    }

    /// The rounded corners.
    ///
    /// A region, and not the corner preference: the preference rounds the
    /// window's *frame*, and the backdrop the accent painted is square
    /// behind it. A region clips both, so the blur follows the corner.
    fn apply_region(&self) {
        let radius = Layout::default().scaled(self.scale).radius as i32;
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
                if got < 0 {
                    // The loop itself failed. The window is broken, and a
                    // launcher that leaves with the code of a clean run
                    // says nothing about it.
                    let code = GetLastError();
                    return Err(format!("the message loop failed ({code})"));
                }
                if got == 0 {
                    break;
                }
                let control = self.dispatch(&msg, &mut on_event);
                if !self.apply(control) {
                    break;
                }
                // The user went somewhere else: the window gets out of the
                // way, and the answer is given here, where the host is not
                // inside anything.
                if self.state_mut().focus_lost {
                    self.state_mut().focus_lost = false;
                    let control = on_event(Event::Toggle);
                    if !self.apply(control) {
                        break;
                    }
                }
                // The notification area: the shell sends its clicks to the
                // window's procedure, and the pump is where the answer is
                // given — the menu is a modal loop, and a modal loop
                // inside a window procedure is a modal loop inside a
                // window procedure.
                if let Some(mouse) = self.state_mut().tray_click.take() {
                    let control = self.tray_clicked(mouse, &mut on_event);
                    if !self.apply(control) {
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    /// Do what the control says. False when the window is done.
    fn apply(&mut self, control: Control) -> bool {
        match control {
            Control::None => true,
            Control::Show(view, width, height) => {
                self.install(view);
                self.resize(width, height);
                self.show();
                true
            }
            Control::Hide => {
                self.hide();
                true
            }
            Control::Paint(view, width, height) => {
                self.install(view);
                self.resize(width, height);
                self.repaint();
                true
            }
            Control::Quit => false,
        }
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
            // The notification area's clicks are NOT answered here: the
            // shell sends them to the window's procedure, and a message
            // that is sent never comes out of the queue. The procedure
            // keeps them and the pump answers them.
            // The shell was made again — it forgot every icon it was given,
            // and the launcher's face goes back into the notification area.
            // Without this the icon is there until the next restart of the
            // explorer, and the launcher that has no face is the launcher
            // the user cannot find.
            if let Some(tray) = self.tray.as_ref()
                && msg.message == tray.taskbar_created()
            {
                let _ = tray.restore();
                return Control::None;
            }
            if (msg.message == WM_KEYDOWN || msg.message == WM_SYSKEYDOWN)
                && let Some(key) = translate_key(msg, control_down())
            {
                return on_event(Event::Key(key));
            }
            if msg.message == WM_CHAR {
                // The platform sends a character outside the Basic
                // Multilingual Plane in two messages: the high
                // surrogate and then the low one. The window keeps the
                // first half until the second one arrives.
                let pending = self.state_mut().pending_char;
                let (key, pending) = translate_char(msg.wParam as u16, pending);
                self.state_mut().pending_char = pending;
                if let Some(key) = key {
                    return on_event(Event::Key(key));
                }
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

    /// What the user did with the icon in the notification area.
    ///
    /// The left button opens the window, and so does the double click:
    /// both ask for the window to be **shown**, and showing a window that
    /// is already up leaves it up. A toggle would be the wrong answer
    /// twice over — the shell sends the click again when the user
    /// double-clicks, and a toggle would open the window and close it.
    ///
    /// The right button opens the menu, and the menu says which of the two
    /// things the user wants: the window, or the end of the program.
    fn tray_clicked(
        &mut self,
        mouse: UINT,
        on_event: &mut impl FnMut(Event) -> Control,
    ) -> Control {
        if mouse == WM_LBUTTONUP || mouse == WM_LBUTTONDBLCLK {
            return on_event(Event::Show);
        }
        if mouse != WM_RBUTTONUP && mouse != WM_CONTEXTMENU {
            return Control::None;
        }
        let lang = self.state().lang;
        let Some(tray) = self.tray.as_ref() else {
            return Control::None;
        };
        match tray.menu(self.hwnd, Text::for_lang(lang)) {
            Some(crate::tray::Command::Search) => on_event(Event::Show),
            Some(crate::tray::Command::Quit) => on_event(Event::Quit),
            None => Control::None,
        }
    }
}

/// The scale of a monitor, in per cent.
///
/// The platform answers with the monitor's own DPI — 96 on a screen
/// that is not scaled, 192 on one that is twice as big — and the scale
/// is that number over the baseline.
fn scale_of(dpi: u32) -> u32 {
    if dpi == 0 {
        // The platform would not say, and a window with no scale is a
        // window at the baseline one.
        return SCALE_PERCENT;
    }
    dpi * SCALE_PERCENT / BASE_DPI
}

/// Whether the backdrop the desktop window manager named is one a
/// window can leave its background empty for.
///
/// `AUTO` is the system deciding, and "the system deciding" is not the
/// same thing as a blur; `NONE` is nothing at all. The other three are
/// the backdrops that are really there.
fn backdrop_is_there(kind: i32) -> bool {
    if kind == DWMSBT_NONE {
        // The manager said there is nothing behind the window.
        return false;
    }
    matches!(
        kind,
        DWMSBT_MAIN_WINDOW | DWMSBT_TRANSIENT_WINDOW | DWMSBT_TABBED_WINDOW
    )
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
    /// The width the window may have: it is never wider than the area.
    width: i32,
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
    // A window wider than the area is cut to the area: centred, it would
    // hang over the edge of the screen, and the window's own contract is
    // that it never falls outside it. The height is cut the same way
    // below, and the two cuts are the same cut.
    let width = width.min(area_w);
    let x = work.left + (area_w - width).max(0) / 2;
    // The offset is where the window starts, and the room below it is
    // what the window has to fit in.
    let offset = (area_h as f32 * VERTICAL_FRACTION) as i32;
    if height >= area_h {
        // Taller than the area: the window starts at the top and is cut.
        return Spot {
            x,
            y: work.top,
            width,
            height: area_h,
        };
    }
    let room = (area_h - offset).max(0);
    Spot {
        x,
        y: work.top + offset,
        width,
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
            // The painting is begun and ended whatever the window's state
            // is: a message that does not validate its update region is a
            // message the platform sends again, and again, forever.
            let mut ps = PAINTSTRUCT {
                hdc: core::ptr::null_mut(),
                fErase: 0,
                rcPaint: RECT::default(),
                fRestore: 0,
                fIncUpdate: 0,
                rgbReserved: [0; 32],
            };
            let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
            if let Some(state) = unsafe { state_of(hwnd) } {
                // SAFETY: the state lives in the window's own thread, the
                // window procedure runs on that thread, and no other
                // thread ever holds a pointer to it — the hotkey posts a
                // message and nothing more. The pointer is the one the
                // window put in its own user data, and the allocation
                // lives as long as the window does.
                unsafe {
                    let state = &mut *state;
                    match state.layer.as_mut() {
                        // The window paints its own alpha: the drawing
                        // goes into the layer's own bitmap, and the layer
                        // is what the compositor puts on the screen.
                        Some(layer) => {
                            if !layer.present(
                                hwnd,
                                &state.view,
                                &mut state.painted,
                                &mut state.icons,
                                &state.fonts,
                                state.scale,
                                state.lang,
                            ) {
                                // The compositor threw the picture away.
                                // The window keeps the one it had -- a
                                // window with an old picture is better
                                // than a window with none -- and the
                                // answer is remembered, so the launcher
                                // can say that the screen is the one that
                                // is not cooperating.
                                state.compositor = Compositor::Refused;
                            }
                        }
                        // The platform's own backdrop is behind this
                        // window, and the drawing leaves it alone.
                        None => {
                            paint::paint(
                                hdc,
                                &state.view,
                                &mut state.painted,
                                state.backdrop,
                                &mut state.icons,
                                &state.fonts,
                                state.scale,
                                state.lang,
                            );
                        }
                    }
                }
            }
            unsafe { EndPaint(hwnd, &ps) };
            0
        }
        // The backdrop is the system's: erasing the background here would
        // paint over the blur the accent policy asked for.
        WM_ERASEBKGND => 1,
        // The user went somewhere else: the launcher gets out of the way.
        // A window floating over the editor the user just clicked into is
        // a window the user has to close by hand.
        //
        // The platform **sends** this message to every top-level window
        // of the application when the activation state changes: it is not
        // posted, so it never comes out of the message queue and the pump
        // never sees it. The window procedure is the only place it
        // arrives — and the probe that proved it posted the message
        // instead, and the window hid, and the real message did nothing.
        //
        // So the window procedure raises the flag and the pump gives the
        // answer: the decision to hide belongs to the host, and the host
        // is asked where the host is not inside a platform call.
        WM_ACTIVATEAPP => {
            if wparam == 0
                && let Some(state) = unsafe { state_of(hwnd) }
            {
                // SAFETY: the state lives in the window's own thread,
                // the window procedure runs on that thread, and the
                // pointer is the one the window put in its own user
                // data.
                unsafe {
                    let state = &mut *state;
                    // The window hiding itself does not count: that is
                    // the launcher saying goodbye, and answering it
                    // would open the window the user just closed.
                    if state.hiding {
                        state.hiding = false;
                    } else {
                        state.focus_lost = true;
                    }
                }
            }
            0
        }
        // The shell **sends** the notification area's clicks to the
        // window's procedure: the message is not posted, so it never comes
        // out of the message queue and the pump never sees it. The window
        // procedure is the only place it arrives — and a click that the
        // procedure hands to the default procedure is a click that is
        // lost, which is an icon that does not answer when it is clicked.
        //
        // So the procedure keeps the mouse's message and wakes the pump,
        // and the pump answers it: the answer belongs to the host, and the
        // host is asked where the host is not inside a platform call.
        crate::tray::WM_TRAY => {
            if let Some(state) = unsafe { state_of(hwnd) } {
                unsafe {
                    (*state).tray_click = Some(crate::tray::mouse_message(lparam));
                }
                // The pump is asleep in the queue: the nudge is what wakes
                // it, so the answer is given now and not at the next key
                // press.
                unsafe { PostMessageW(hwnd, WM_NULL, 0, 0) };
            }
            0
        }
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
/// The flag is a word: false is "not registered yet", and true is the
/// class itself. The class is registered once per process -- registering
/// the same class twice is an error on Windows, and the launcher has one
/// window -- and the flag is what the second caller reads instead of
/// registering again.
static CLASS_REGISTERED: AtomicBool = AtomicBool::new(false);

/// Register the window class, once.
///
/// The window is made by the class's **name**, so the platform's handle
/// for the class -- the atom -- is not needed to make it, and the only
/// thing the registration has to get right is that the class is there.
///
/// Two callers that both find the class missing are two threads, and the
/// second registration is an error that says the class is already there.
/// That is not a failure: a class that is registered is the class this
/// caller wanted.
fn register_class() -> Result<(), String> {
    if CLASS_REGISTERED.load(Ordering::Acquire) {
        return Ok(());
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
        let code = unsafe { GetLastError() };
        if code != ERROR_CLASS_ALREADY_EXISTS {
            return Err(format!("the window class could not be registered ({code})"));
        }
        // The class is there: another thread registered it a moment ago.
    }
    CLASS_REGISTERED.store(true, Ordering::Release);
    Ok(())
}

/// Whether a Control key is down.
fn control_down() -> bool {
    unsafe { GetKeyState(VK_CONTROL as i32) < 0 }
}

/// The control keys, from the system's own view of the keyboard.
///
/// The Control state comes from the caller: the keyboard is read once,
/// here, and a translation that asked it again would be asking a
/// question whose answer can change between the two reads.
fn translate_key(msg: &MSG, ctrl: bool) -> Option<Key> {
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
        _ if ctrl && vk == b'V' as u32 => Key::CtrlV,
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
///
/// A character outside the Basic Multilingual Plane arrives in two
/// messages — the high surrogate and then the low one — and the two
/// halves are one character. The window keeps the first half until the
/// second one arrives, and a half whose other half never comes is
/// dropped: it is not a character, and a character nobody finished is a
/// character the field does not get.
///
/// The answer is the character and the half that is left waiting.
fn translate_char(code: u16, pending: Option<u16>) -> (Option<Key>, Option<u16>) {
    if code == 0 {
        return (None, pending);
    }
    // The control characters the system sends for the keys it already
    // reported as a key event: they would be typed twice. The tab is one
    // of them -- the key is already a key, and a tab inside a search box
    // is a character nobody typed.
    if code < 0x20 {
        return (None, None);
    }
    // The high surrogate: the first half of a character the platform
    // sends in two pieces.
    if (0xD800..=0xDBFF).contains(&code) {
        return (None, Some(code));
    }
    // The low surrogate: it completes the half the window kept.
    if (0xDC00..=0xDFFF).contains(&code) {
        let Some(high) = pending else {
            // A low surrogate with no half before it is not a character.
            return (None, None);
        };
        let together = 0x1_0000 + ((high as u32 - 0xD800) << 10) + (code as u32 - 0xDC00);
        return match char::from_u32(together) {
            Some(c) => (Some(Key::Char(c)), None),
            None => (None, None),
        };
    }
    // A whole character: it ends whatever half was waiting.
    (char::from_u32(code as u32).map(Key::Char), None)
}

/// A UTF-16 string with a NUL, for the calls that take one.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(core::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {

    /// A message with one key in it.
    fn key_message(vk: u32) -> MSG {
        MSG {
            hwnd: core::ptr::null_mut(),
            message: WM_KEYDOWN,
            wParam: vk as WPARAM,
            lParam: 0,
            time: 0,
            pt: POINT { x: 0, y: 0 },
        }
    }

    /// Ctrl+V is the paste, and only with the Control: the letter on its
    /// own is a letter, and a launcher that pasted the clipboard every
    /// time the user typed a "v" would be a launcher nobody could type
    /// in.
    #[test]
    fn the_paste_needs_the_control() {
        let msg = key_message(b'V' as u32);
        assert_eq!(translate_key(&msg, true), Some(Key::CtrlV));
        assert_eq!(translate_key(&msg, false), None);
    }

    /// Ctrl+C is the copy, and it is the same shape: the Control decides.
    #[test]
    fn the_copy_needs_the_control() {
        let msg = key_message(b'C' as u32);
        assert_eq!(translate_key(&msg, true), Some(Key::CtrlC));
        assert_eq!(translate_key(&msg, false), None);
    }

    /// The tab is a key, not a character.
    ///
    /// The key event already carries it, and the control character that
    /// comes with it would put a tab inside the query -- and Ctrl+I, which
    /// is the same control character, would do the same.
    #[test]
    fn the_tab_is_not_a_character() {
        assert_eq!(translate_char(0x09, None).0, None);
        // A letter is still a letter.
        assert_eq!(translate_char('a' as u16, None).0, Some(Key::Char('a')));
    }

    /// A character outside the Basic Multilingual Plane is two messages
    /// and one character.
    ///
    /// The platform sends it as a high surrogate and then a low one, and
    /// a window that reads each message on its own drops both: a
    /// surrogate is not a scalar value, and the field does not move
    /// while the user types an emoji.
    #[test]
    fn a_character_outside_the_bmp_is_one_character() {
        // U+1D54F is 𝕏, and the platform sends it as the high
        // surrogate 0xD835 and then the low one, 0xDD4F.
        let (high, low) = (0xD835u16, 0xDD4Fu16);
        assert_eq!(char::from_u32(0x1D54F), Some('\u{1D54F}'));
        // The first message: the half is kept, and it is not a
        // character yet.
        let (key, pending) = translate_char(high, None);
        assert_eq!(key, None);
        assert_eq!(pending, Some(high));
        // The second message: the two halves are one character.
        let (key, pending) = translate_char(low, pending);
        assert_eq!(key, Some(Key::Char('\u{1D54F}')));
        assert_eq!(pending, None, "and the half is gone");
    }

    /// A half that never gets its other half is dropped.
    ///
    /// The next character is a whole one: the half does not join it, and
    /// the field gets what the user typed.
    #[test]
    fn a_half_character_is_dropped() {
        let (key, pending) = translate_char(0xD835, None);
        assert_eq!(key, None);
        assert_eq!(pending, Some(0xD835));
        let (key, pending) = translate_char('a' as u16, pending);
        assert_eq!(key, Some(Key::Char('a')));
        assert_eq!(pending, None);
    }

    /// A low surrogate with no half before it is not a character.
    #[test]
    fn a_low_surrogate_alone_is_not_a_character() {
        let (key, pending) = translate_char(0xDD4F, None);
        assert_eq!(key, None);
        assert_eq!(pending, None);
    }

    /// So the window believes in a blur, the platform has to say there is
    /// one: the accent's own answer is not that answer.
    #[test]
    fn only_a_real_backdrop_is_a_backdrop() {
        assert!(
            !backdrop_is_there(DWMSBT_AUTO),
            "the system deciding is not a blur"
        );
        assert!(
            !backdrop_is_there(DWMSBT_NONE),
            "nothing behind the window is not a blur"
        );
        assert!(backdrop_is_there(DWMSBT_MAIN_WINDOW), "Mica is a backdrop");
        assert!(
            backdrop_is_there(DWMSBT_TRANSIENT_WINDOW),
            "Acrylic is a backdrop"
        );
        assert!(
            backdrop_is_there(DWMSBT_TABBED_WINDOW),
            "the tab's Mica is a backdrop"
        );
    }
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

    /// The window is the model's size at the screen's scale.
    ///
    /// The model asks for six hundred units and the window is in device
    /// pixels: a screen twice as big gets a window twice as big, and the
    /// text inside it is drawn twice as big.
    #[test]
    fn the_window_is_the_model_s_size_at_the_screen_s_scale() {
        assert_eq!(scale_of(96), SCALE_PERCENT);
        assert_eq!(scale_of(120), SCALE_PERCENT + SCALE_PERCENT / 4);
        assert_eq!(scale_of(144), SCALE_PERCENT + SCALE_PERCENT / 2);
        assert_eq!(scale_of(192), SCALE_PERCENT * 2);
        assert_eq!(device_pixels(600, scale_of(192)), 1200);
        assert_eq!(device_pixels(600, scale_of(96)), 600);
    }

    /// A screen that does not answer is the baseline one.
    #[test]
    fn a_screen_that_does_not_answer_is_the_baseline_one() {
        assert_eq!(scale_of(0), SCALE_PERCENT);
    }

    #[test]
    fn a_window_wider_than_the_work_area_starts_at_its_left_edge() {
        // Centred, it would hang over the edge of the screen.
        let work = area();
        let spot = spot_on(work, 2400, 484);
        assert_eq!(spot.x, work.left);
    }

    /// The compositor takes the window's own picture.
    ///
    /// A layered window whose picture the compositor refuses shows
    /// nothing at all, and the launcher would be a window the user
    /// cannot see. The question is asked before the window is ever
    /// shown, and this is the answer on the machine the test runs on.
    #[test]
    fn the_compositor_takes_the_window_picture() {
        let window = Window::new("rgxy-test").expect("the window was made");
        assert_eq!(window.compositor(), Compositor::Accepted);
    }

    /// The icon goes into the notification area.
    ///
    /// The window is a tool window on purpose: it is not in the taskbar and
    /// not in the alt-tab list, so the icon in the notification area is the
    /// launcher's face while it waits for the next Control. An icon that
    /// the shell refuses is a launcher with no face, and the refusal is
    /// said out loud here rather than swallowed: the launcher still works
    /// without the icon, and a launcher that is not there is a launcher the
    /// user cannot find.
    #[test]
    fn the_icon_goes_into_the_notification_area() {
        let window = Window::new("rgxy-test").expect("the window was made");
        assert!(
            window.tray_error().is_none(),
            "the notification area refused the icon: {:?}",
            window.tray_error()
        );
    }

    /// The icon's click asks for the window, and so does the double click.
    ///
    /// A double click on the notification area's icon is two clicks, and a
    /// toggle would show the window on the first and hide it on the
    /// second: a window that opens and closes again is a window that is
    /// not there. So the click asks for the window to be **shown**.
    #[test]
    fn the_icons_click_asks_for_the_window() {
        extern crate std;
        let mut window = Window::new("rgxy-test").expect("the window was made");
        let asked: std::cell::RefCell<Vec<Event>> = std::cell::RefCell::new(Vec::new());
        let mut answer = |event: Event| {
            asked.borrow_mut().push(event);
            Control::None
        };

        window.tray_clicked(WM_LBUTTONUP, &mut answer);
        assert_eq!(
            asked.borrow().as_slice(),
            &[Event::Show],
            "the click asks for the window"
        );

        asked.borrow_mut().clear();
        window.tray_clicked(WM_LBUTTONDBLCLK, &mut answer);
        assert_eq!(
            asked.borrow().as_slice(),
            &[Event::Show],
            "the double click asks too"
        );
    }

    /// Two windows at once are two windows.
    ///
    /// The class is registered once per process, and two threads that
    /// both find it missing are two threads that both try to register
    /// it: the second one is told the class is already there, and a
    /// class that is already registered is the class the caller wanted.
    /// The window is made by the class's name, so the atom is not needed.
    #[test]
    fn two_windows_at_once_are_two_windows() {
        extern crate std;
        let handles: Vec<_> = (0..8)
            .map(|_| std::thread::spawn(|| Window::new("rgxy-test").is_ok()))
            .collect();
        for handle in handles {
            assert!(
                handle.join().expect("the thread ended"),
                "the window was made"
            );
        }
    }

    /// The window is on the screen.
    ///
    /// The screen capture of this host does not carry the windows that
    /// are put on top of it -- not the ones of this process, and not even
    /// the ones the operating system drew -- so a picture of the screen
    /// cannot answer this. The platform can: a window that is visible,
    /// that the compositor is not cloaking, that is above everything, and
    /// whose bounds the compositor agrees with, is a window the user is
    /// looking at.
    ///
    /// The window is shown without the focus: the test does not take the
    /// keyboard away from whatever the user is doing.
    #[test]
    fn the_window_is_on_the_screen() {
        let window = Window::new("rgxy-test").expect("the window was made");
        let hwnd = window.hwnd();
        unsafe {
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        // The platform's own answer: the window is visible, the
        // compositor is not hiding it, and it is where it says it is.
        assert!(window.is_on_the_screen(), "the window is on the screen");

        // And it is above everything.
        let style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
        assert_ne!(style & WS_EX_LAYERED, 0, "the window paints its own alpha");
        assert_ne!(style & WS_EX_TOPMOST, 0, "the window is above everything");

        unsafe {
            ShowWindow(hwnd, SW_HIDE);
        }
    }

    /// The user going somewhere else is the window's own news.
    ///
    /// The platform **sends** `WM_ACTIVATEAPP` to every top-level window
    /// of the application when the activation state changes: it is not
    /// posted, so it never comes out of the message queue and the pump
    /// never sees it. The window procedure is the only place it arrives.
    ///
    /// The bug this pins: the pump used to look for the message, and the
    /// message the platform really sends never reached the pump — so the
    /// launcher stayed on the screen when the user clicked into their
    /// editor. A posted message is not the platform's message, and a
    /// probe that posts one proves nothing about the real one.
    #[test]
    fn the_user_going_somewhere_else_is_the_window_s_own_news() {
        let window = Window::new("rgxy-test").expect("the window was made");
        let hwnd = window.hwnd();
        assert!(!window.state().focus_lost, "the user has not gone anywhere");

        // The message the platform sends: the window procedure raises the
        // flag, and the pump gives the answer.
        unsafe {
            PostMessageW(hwnd, WM_ACTIVATEAPP, 0, 0);
        }
        // The message has to be pumped for the window procedure to see it:
        // a posted message waits in the queue.
        let mut msg = MSG {
            hwnd: core::ptr::null_mut(),
            message: 0,
            wParam: 0,
            lParam: 0,
            time: 0,
            pt: POINT { x: 0, y: 0 },
        };
        unsafe {
            let got = GetMessageW(&mut msg, core::ptr::null_mut(), 0, 0);
            assert!(got > 0, "the message arrived");
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        assert!(
            window.state().focus_lost,
            "the user went somewhere else and the window did not hear it"
        );
    }

    /// The window hiding itself is not the user going somewhere else.
    ///
    /// The platform says goodbye to the application when the window goes
    /// away, and answering that goodbye would open the window the user
    /// just closed.
    #[test]
    fn a_window_that_hides_itself_is_not_the_user_going_somewhere_else() {
        let mut window = Window::new("rgxy-test").expect("the window was made");
        let hwnd = window.hwnd();
        window.hide();
        assert!(window.state().hiding, "the window is saying goodbye");

        unsafe {
            PostMessageW(hwnd, WM_ACTIVATEAPP, 0, 0);
        }
        let mut msg = MSG {
            hwnd: core::ptr::null_mut(),
            message: 0,
            wParam: 0,
            lParam: 0,
            time: 0,
            pt: POINT { x: 0, y: 0 },
        };
        unsafe {
            let got = GetMessageW(&mut msg, core::ptr::null_mut(), 0, 0);
            assert!(got > 0, "the message arrived");
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        assert!(
            !window.state().focus_lost,
            "the window opened itself again after the user closed it"
        );
    }

    /// The window speaks English until it is told otherwise.
    ///
    /// The words are in the vocabulary, and the vocabulary's base column
    /// is English: a window that said nothing about its language would
    /// be a window whose words are a guess.
    #[test]
    fn the_window_speaks_english_by_default() {
        let window = Window::new("rgxy-test").expect("the window was made");
        assert_eq!(
            window.view().placeholder,
            Text::for_lang(Lang::English).placeholder
        );
    }

    /// The window speaks the language it is given.
    ///
    /// The launcher was made with Portuguese words on purpose, and the
    /// words are the product's: the window holds the language and asks
    /// the vocabulary for the word it needs.
    #[test]
    fn the_window_speaks_the_language_it_was_given() {
        let window = Window::new("rgxy-test")
            .expect("the window was made")
            .with_lang(Lang::Portuguese);
        assert_eq!(
            window.view().placeholder,
            Text::for_lang(Lang::Portuguese).placeholder
        );
        // And the two languages are not the same word.
        assert_ne!(
            window.view().placeholder,
            Text::for_lang(Lang::English).placeholder
        );
    }

    /// The view the host installs is in the window's language.
    ///
    /// The host owns the rows and the query, and the words are the
    /// window's: a view that carried the placeholder of another
    /// language would put two languages on one screen.
    #[test]
    fn the_view_the_host_installs_is_in_the_windows_language() {
        let mut window = Window::new("rgxy-test")
            .expect("the window was made")
            .with_lang(Lang::Portuguese);
        window.set_view(View {
            placeholder: Text::for_lang(Lang::English).placeholder.to_string(),
            ..View::default()
        });
        assert_eq!(
            window.view().placeholder,
            Text::for_lang(Lang::Portuguese).placeholder
        );
    }
}
