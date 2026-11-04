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
use rgy_launcher::theme::Theme;
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

/// The message that asks the window to **show** itself, and not to toggle.
///
/// A second launcher posts this to the first one's window: the user
/// started the launcher again, and what they asked for is to see it. A
/// toggle would hide a window that was already up — the same reason the
/// notification area's click asks for a showing and not for a toggle.
pub const WM_SHOW: UINT = WM_APP + 2;

/// The message the worker posts when an answer is in the channel.
///
/// The window's thread is asleep in its message loop until something
/// happens, and the something is this: the answer arrived, and the loop
/// has to look at it now. Without it the answer waits for the next tick
/// of the clock — measured on 2026-11-04, the wait was up to a clock
/// (120 ms) and the median a paint later than it had to be.
pub const WM_WAKE: UINT = WM_APP + 3;

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
    /// Something the window is waiting for has arrived.
    ///
    /// The worker that asks the daemon posts this when the answer is in
    /// the channel: without it the window would look at the channel at
    /// the next tick of the clock, up to a clock later than the answer.
    Wake,
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
    /// The rows of the settings panel, when it has the keyboard.
    ///
    /// Each row is a label and the value it holds, both already spelled
    /// in the language the user reads and in the words the settings
    /// say: the panel draws what it is given, and the settings are the
    /// host's.
    pub settings: Vec<(String, String)>,
    /// Which row of the settings panel is selected.
    pub settings_selected: usize,
    /// The colours this frame is painted in.
    ///
    /// The view carries everything the window draws, and the colours are
    /// drawn things: a panel that changes the theme wears the new one on
    /// the very next frame, and a view that could not say its colours
    /// would be a view the window painted in the ones before.
    pub theme: rgy_launcher::theme::Theme,
    /// How opaque the panel is.
    pub alpha: u32,
    /// Where the window sits on the work area, in thousandths.
    pub anchor: (u32, u32),
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
    /// The colours the window paints in.
    ///
    /// The colours live in the state and not in the [`Window`] wrapper
    /// because the painting happens in the window's procedure, which
    /// reaches the window through this state and through nothing else:
    /// a colour the procedure could not read would be a colour the
    /// picture never wore. They start as the window's own palette and
    /// [`Window::set_colours`] is the one way they change.
    colours: paint::Colours,
    /// The panel's own alpha: ninety per cent.
    ///
    /// Almost black and slightly transparent: a tenth of the desktop
    /// shows through, and the panel keeps its colour where the desktop is
    /// dark. It is the alpha that lets the desktop through where the
    /// panel draws nothing — the rounded corners are the region's own,
    /// and the shadow is the system's own, and the alpha is what is left
    /// of the panel.
    ///
    /// It is the state's own number and not a constant because the theme
    /// carries it: [`Window::set_colours`] takes it with the palette,
    /// clamped to the range a panel is a panel in.
    panel_alpha: u8,
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
        colours: &paint::Colours,
        alpha: u8,
        scale: u32,
        lang: Lang,
    ) -> bool {
        unsafe {
            // The panel's own colour is what the desktop shows through.
            paint::paint(
                self.dc, view, painted, false, icons, fonts, colours, scale, lang,
            );
            // The alpha is worked out after the drawing: GDI writes the
            // colour of a pixel and never its alpha.
            self.compose(colours, alpha);
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
    /// The panel's own colour — whatever theme painted it — is nearly
    /// opaque and the desktop shows through the rest of it; what the
    /// panel draws on it — the border, the query, the rows — is opaque,
    /// so the text keeps its edges.
    fn compose(&self, colours: &paint::Colours, alpha: u8) {
        let width = self.width;
        let height = self.height;
        let stride = (width * 4) as usize;
        unsafe {
            for y in 0..height {
                for x in 0..width {
                    let at = (y as usize) * stride + (x as usize) * 4;
                    let pixel = self.bits.add(at);
                    if is_the_panel(*pixel, *pixel.add(1), *pixel.add(2), colours) {
                        // The panel's own colour: nearly opaque, and the
                        // colour it carries is the colour it has with
                        // that alpha in it.
                        *pixel = premultiplied(colours.background_blue, alpha);
                        *pixel.add(1) = premultiplied(colours.background_green, alpha);
                        *pixel.add(2) = premultiplied(colours.background_red, alpha);
                        *pixel.add(3) = alpha;
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
/// panel, and they keep the panel's alpha. The panel's colour is the
/// CURRENT theme's background — the comparison follows the colours the
/// picture was painted in, and a tolerance pinned to one theme's
/// channels would treat every other theme's panel as drawing, and paint
/// it opaque.
fn is_the_panel(blue: u8, green: u8, red: u8, colours: &paint::Colours) -> bool {
    let near = |value: u8, panel: u8| (value as i32 - panel as i32).abs() <= PANEL_TOLERANCE;
    near(blue, colours.background_blue)
        && near(green, colours.background_green)
        && near(red, colours.background_red)
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
    /// Where the window sits on its work area, in permille of it: `x`
    /// across the room the window does not take, `y` down the area.
    ///
    /// The host moves it with [`Window::set_anchor`]; the default is the
    /// middle of the area, eighteen per cent down — the place the window
    /// was put before the anchor existed.
    anchor: (u32, u32),
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
            colours: paint::Colours::WARM,
            // Ninety per cent: the field's own doc says why.
            panel_alpha: 230,
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
                anchor: DEFAULT_ANCHOR,
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
                &state.colours,
                state.panel_alpha,
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

    /// Paint in another theme.
    ///
    /// The theme is the whole palette at once, and the alpha goes with
    /// it: the panel's colour is the theme's background, and the alpha
    /// is how much of the desktop shows through that colour — the two
    /// are one fact about the panel and not two. The alpha is clamped,
    /// to a quarter at the bottom and to full opacity at the top: a
    /// panel below a quarter is made of nothing, and a number above 255
    /// is not an alpha a byte can carry.
    ///
    /// The window is repainted at once, in the colours it will wear from
    /// now on: a theme the user cannot see taking effect is a theme the
    /// user thinks did nothing.
    pub fn set_colours(&mut self, theme: Theme, alpha: u32) {
        self.wear(theme, alpha);
        self.repaint();
    }

    /// The view the window draws, in the language the window speaks.
    ///
    /// The placeholder is the word of that language: the host owns the
    /// rows and the query, and a window that showed its placeholder in
    /// another language would be a window the user reads in two
    /// languages at once.
    fn install(&mut self, mut view: View) {
        view.placeholder = Text::for_lang(self.state().lang).placeholder.to_string();
        // The colours ride with the view, and the window wears the ones
        // it is given: a frame that arrives in a new theme is painted in
        // it, and a frame in the same one costs nothing — the comparison
        // is a few words and the repaint it would cause is not.
        self.wear(view.theme, view.alpha);
        // The seat moves with the view too: a panel that moved the
        // window re-places it here, where the size it will be is already
        // the size the control carries.
        if view.anchor != self.anchor {
            self.set_anchor(view.anchor.0, view.anchor.1);
        }
        self.state_mut().view = view;
    }

    /// Wear a theme and an opacity, without painting.
    ///
    /// The painting is the caller's: [`Window::set_colours`] is this
    /// wear with a repaint behind it, for the caller that has no frame
    /// on its way.
    fn wear(&mut self, theme: rgy_launcher::theme::Theme, alpha: u32) {
        let colours = paint::Colours::of(&theme);
        let alpha = alpha.clamp(64, 255) as u8;
        let state = self.state_mut();
        if colours != state.colours || alpha != state.panel_alpha {
            state.colours = colours;
            state.panel_alpha = alpha;
        }
    }

    /// Show the window, over everything, without raising what is under it.
    ///
    /// The window is shown with `SW_SHOWNOACTIVATE` so the window
    /// underneath is not raised or restored — the desktop the user was
    /// looking at is still there, untouched — and the keyboard is taken
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
            take_the_keyboard(self.hwnd);
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
    /// at, and the window sits on that monitor's work area at the
    /// anchor's place in it.
    fn place(&mut self) {
        let spot = spot_on(
            self.target_work_area(),
            self.width,
            self.height,
            self.anchor,
        );
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

    /// Put the window at another place on its work area.
    ///
    /// The numbers are permille of the area, `x` across and `y` down,
    /// and both are clamped to the area: an anchor outside it is an
    /// anchor the window cannot honour without leaving the screen. The
    /// window is placed again at once, so the move happens where the
    /// caller made it and not at the next showing.
    pub fn set_anchor(&mut self, x: u32, y: u32) {
        self.anchor = (x.clamp(0, 1000), y.clamp(0, 1000));
        self.place();
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
        let spot = spot_on(self.target_work_area(), width, height, self.anchor);
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
            //
            // It does not stay where it is when that would take it off
            // the work area: the window's own contract is that it never
            // falls outside it, and a window wider than the room on its
            // right has to grow to the left — which is what the anchor
            // at the far end promised. The position is the one the
            // window is already at, clamped into the area, so the common
            // case does not move the window at all.
            let work = self.target_work_area();
            let mut rect = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            let known = GetWindowRect(self.hwnd, &mut rect) != 0;
            let (x, y, flags) = if known {
                let x = rect
                    .left
                    .clamp(work.left, (work.right - width).max(work.left));
                let y = rect
                    .top
                    .clamp(work.top, (work.bottom - height).max(work.top));
                if x == rect.left && y == rect.top {
                    // The window is not moved at all when it is already
                    // where it has to be: a window that moves under the
                    // cursor while the user is typing in it is a window
                    // the user cannot reach.
                    (0, 0, SWP_NOMOVE | SWP_NOACTIVATE)
                } else {
                    (x, y, SWP_NOACTIVATE)
                }
            } else {
                // The platform would not say where the window is: the
                // anchor says where it goes.
                (spot.x, spot.y, SWP_NOACTIVATE)
            };
            SetWindowPos(self.hwnd, HWND_TOPMOST, x, y, width, height, flags);
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
                if self.state_mut().focus_lost && !self.get_out_of_the_way(&mut on_event) {
                    break;
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
                // A frame that is the one on the screen is not painted
                // again, and a frame nobody can see is not painted at
                // all. Measured on 2026-11-04, with the daemon away: the
                // window repainted sixteen times a second on a clock
                // that had no news, for 4.3% of a core, and every answer
                // was painted twice. The picture is the view and the
                // size, and the platform needs a repaint for neither of
                // them changing.
                let same_frame = self.state().view == view && self.model == (width, height);
                self.install(view);
                self.resize(width, height);
                if !same_frame && self.is_visible() {
                    self.repaint();
                }
                true
            }
            Control::Quit => false,
        }
    }

    /// Answer the focus the user took away: the window gets out of the
    /// way, and the answer is given here, where the host is not inside a
    /// platform call.
    ///
    /// Only a window that is on the screen has somewhere to go. The
    /// notification area's own menu puts the window's thread in front of
    /// the user and then drops it — `SetForegroundWindow` before the menu
    /// opens, the deactivation after it closes — and the deactivation
    /// reaches a window that is not on the screen. Answering it would
    /// bring the launcher back up: the user walked away from a menu, and
    /// the launcher opened itself.
    ///
    /// False when the window is done.
    fn get_out_of_the_way(&mut self, on_event: &mut dyn FnMut(Event) -> Control) -> bool {
        self.state_mut().focus_lost = false;
        if !self.is_visible() {
            return true;
        }
        let control = on_event(Event::Toggle);
        self.apply(control)
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
            // The second launcher's own message: the user started the
            // launcher again, and a launcher that is there is a launcher
            // to show.
            if msg.message == WM_SHOW {
                return on_event(Event::Show);
            }
            // The worker's own message: the answer is in the channel, and
            // the loop is awake to look at it.
            if msg.message == WM_WAKE {
                return on_event(Event::Wake);
            }
            // A registered combo: the platform posts this message when
            // the keys were pressed anywhere on the machine, and a
            // posted message comes out of the queue — the pump is where
            // the answer is given, like the hook's own message above.
            if msg.message == WM_HOTKEY {
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

/// Take the keyboard for the window.
///
/// The platform has a **foreground lock**: a process that is not the
/// foreground one, and that did not receive the last input event, is
/// refused `SetForegroundWindow`. The launcher is exactly that process —
/// it is resident, the user's last keystroke went to the editor behind it,
/// and the window is shown with `SW_SHOWNOACTIVATE` so the desktop under
/// it is not restored — so the plain call is refused and the window comes
/// up with the *other* application still holding the keyboard: the user
/// types, and the text lands in the editor behind a search box that looks
/// ready for it. Measured on 2026-11-04: with Notepad focused, the
/// double-Control hotkey showed the window and `GetForegroundWindow` still
/// answered Notepad; the same call with the input queues joined answered
/// the window.
///
/// The documented way around the lock is to **join** the foreground
/// window's input queue for the length of the call: the two threads then
/// share one input state, and `SetForegroundWindow` and `SetFocus` are
/// allowed. The join is undone at once — the queue is not a thing to keep.
///
/// A foreground window that belongs to this very thread is the case where
/// the join is neither possible nor needed: the call already has the
/// right to the keyboard.
fn take_the_keyboard(hwnd: HWND) {
    // SAFETY: the calls are the platform's own, the window is this
    // process's, and the ids are the platform's answers.
    unsafe {
        let foreground = GetForegroundWindow();
        let own = GetCurrentThreadId();
        let foreground_thread = if foreground.is_null() {
            0
        } else {
            GetWindowThreadProcessId(foreground, core::ptr::null_mut())
        };
        let joined = foreground_thread != 0
            && foreground_thread != own
            && AttachThreadInput(own, foreground_thread, 1) != 0;
        BringWindowToTop(hwnd);
        SetForegroundWindow(hwnd);
        SetFocus(hwnd);
        if joined {
            AttachThreadInput(own, foreground_thread, 0);
        }
    }
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

/// Where the window sits on its work area by default, in permille of it.
///
/// Halfway across and eighteen per cent down: the eye is in the upper
/// part of the screen, and the middle of it is where the user's hands
/// are.
const DEFAULT_ANCHOR: (u32, u32) = (500, 180);

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
/// The anchor says where, in permille of the area: `x` across the room
/// the window does not take — so five hundred is the middle, the place
/// the window was centred on before the anchor existed, and a thousand
/// puts the window's right edge on the area's own — and `y` down the
/// area itself, measured from its own top: the taskbar is not part of
/// the area, and neither is whatever is above it.
///
/// The window never falls outside the area. One that is wider than the
/// area is cut to the area, one that is taller than the area is pinned
/// to the top and cut to the area's height, and one that is taller than
/// the room below the anchor is cut to that room.
fn spot_on(work: RECT, width: i32, height: i32, anchor: (u32, u32)) -> Spot {
    let area_w = work.right - work.left;
    let area_h = work.bottom - work.top;
    // A window wider than the area is cut to the area: at any anchor it
    // would hang over the edge of the screen, and the window's own
    // contract is that it never falls outside it. The height is cut the
    // same way below, and the two cuts are the same cut.
    let width = width.min(area_w);
    // The left edge goes as far across the free width as the anchor
    // says: the permille is of the room the window does not take, so the
    // far end of that room is the area's own right edge.
    let x = work.left + (area_w - width).max(0) * anchor.0 as i32 / 1000;
    // The offset is where the window starts, and the room below it is
    // what the window has to fit in. The offset is capped so that there
    // is always a row of room: a window with no height is a window that
    // is not there, and an anchor is a place and not a way of not being
    // there.
    let offset = (area_h * anchor.1 as i32 / 1000).min((area_h - 1).max(0));
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
                                &state.colours,
                                state.panel_alpha,
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
                                &state.colours,
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

/// The handle of a launcher window, when one is on this session.
///
/// The class is the launcher's own and it is the only name two launcher
/// processes share: a second `rgxy` finds the first one's window with
/// this, and asks it to show itself instead of starting a second
/// launcher. Measured on 2026-11-04: two starts were two residents, two
/// keyboard hooks and two icons in the notification area.
pub(crate) fn find_window() -> Option<HWND> {
    // SAFETY: the class name is the crate's own, NUL-terminated array,
    // and the window name is null, which asks for any title.
    let hwnd = unsafe { FindWindowW(CLASS_NAME.as_ptr(), core::ptr::null()) };
    if hwnd.is_null() { None } else { Some(hwnd) }
}

/// Ask a launcher window to show itself.
///
/// The message is the window's own (`WM_SHOW`), and it is **posted**:
/// the window's pump answers it where the host is, and a message that is
/// posted comes out of the queue. The window may be up already — asking
/// it to show itself when it is up leaves it up, which is what the user
/// asked for.
pub(crate) fn ask_to_show(hwnd: HWND) {
    // SAFETY: the window is the launcher's own, and the message is one
    // the window's own dispatch handles.
    unsafe {
        PostMessageW(hwnd, WM_SHOW, 0, 0);
    }
}

/// Wake a window's message loop, from any thread.
///
/// The worker that asks the daemon calls this with the window's handle
/// as a number: a window handle is the platform's, and the thread that
/// asks the daemon holds no window.
pub fn wake(hwnd: usize) {
    // SAFETY: the handle is the launcher's own, and the message is one
    // the window's own dispatch handles.
    unsafe {
        PostMessageW(hwnd as HWND, WM_WAKE, 0, 0);
    }
}

#[cfg(test)]
mod tests {
    /// The lock the tests that put a window on the screen hold.
    ///
    /// A shown window is a **process-wide** thing: the platform has one
    /// foreground window per input queue, and two tests that both take
    /// the keyboard are two tests that fail each other — measured on
    /// 2026-11-04, when `the_window_takes_the_keyboard` ran beside
    /// `a_focus_loss_under_a_hidden_window_opens_nothing`'s shown window
    /// and the foreground was the other test's window. The tests that
    /// show one take the lock; the rest only make windows, and a window
    /// nobody shows is not on the screen to be contended for.
    ///
    /// The lock is a spin: the crate has no `std` outside its tests, and
    /// the sections it guards are a handful of platform calls.
    static SCREEN: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

    /// Hold the screen for as long as the guard lives.
    struct Screen;

    impl Screen {
        fn take() -> Screen {
            while SCREEN
                .compare_exchange(
                    false,
                    true,
                    core::sync::atomic::Ordering::Acquire,
                    core::sync::atomic::Ordering::Relaxed,
                )
                .is_err()
            {
                core::hint::spin_loop();
            }
            Screen
        }
    }

    impl Drop for Screen {
        fn drop(&mut self) {
            SCREEN.store(false, core::sync::atomic::Ordering::Release);
        }
    }

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
        let spot = spot_on(area(), 640, 484, DEFAULT_ANCHOR);
        assert_eq!(spot.x, 640, "the window is in the middle of the area");
    }

    #[test]
    fn the_window_sits_in_the_upper_third_of_the_work_area() {
        let spot = spot_on(area(), 640, 484, DEFAULT_ANCHOR);
        assert_eq!(spot.y, 187, "eighteen per cent down the area");
        assert_eq!(spot.height, 484, "the window keeps the height it asked for");
    }

    /// The defaults are the places the window was put before the anchor
    /// existed, to the pixel: the old centring was half of the free
    /// width and the old offset was eighteen per cent of the area, and
    /// five hundred and a thousandth of the area are the same numbers.
    #[test]
    fn the_default_anchor_is_where_the_window_was_put_before_it() {
        let work = area();
        let spot = spot_on(work, 640, 484, DEFAULT_ANCHOR);
        assert_eq!(spot.x, work.left + (1920 - 640) / 2, "centred, exactly");
        assert_eq!(spot.y, work.top + 1040 * 18 / 100, "eighteen per cent down");
    }

    #[test]
    fn the_window_never_falls_outside_the_work_area() {
        let work = area();
        for anchor in [DEFAULT_ANCHOR, (0, 0), (250, 750), (1000, 1000)] {
            for height in [0, 100, 484, 1000, 1040, 2000] {
                let spot = spot_on(work, 640, height, anchor);
                assert!(spot.y >= work.top, "the window starts inside the area");
                assert!(
                    spot.y + spot.height <= work.bottom,
                    "the window ends inside the area"
                );
                assert!(spot.x >= work.left, "the window starts inside the area");
                assert!(
                    spot.x + spot.width <= work.right,
                    "the window ends inside the area"
                );
            }
        }
    }

    #[test]
    fn a_window_taller_than_the_work_area_is_pinned_to_its_top() {
        let work = area();
        let spot = spot_on(work, 640, 2000, DEFAULT_ANCHOR);
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
        let spot = spot_on(work, 640, 484, DEFAULT_ANCHOR);
        assert_eq!(spot.x, -1280, "the window is centred on that monitor");
        assert_eq!(spot.y, 187);
    }

    /// The anchor's zero is the area's top-left corner.
    ///
    /// The permille is of the room the window does not take, so zero
    /// across is flush with the left edge and zero down is flush with
    /// the top: a launcher pinned to the corner is a launcher the hand
    /// that lives there can reach.
    #[test]
    fn an_anchor_of_zero_is_the_corner() {
        let work = area();
        let spot = spot_on(work, 640, 484, (0, 0));
        assert_eq!(spot.x, work.left, "flush with the left edge");
        assert_eq!(spot.y, work.top, "flush with the top edge");
        assert_eq!(spot.height, 484, "and the window keeps its height");
    }

    /// The anchor's thousand is the area's right edge.
    ///
    /// The permille is of the room the window does not take, so a
    /// thousand across gives the whole of that room to the left of the
    /// window and puts its right edge on the area's own — and not one
    /// pixel past it, which is the contract the cut above keeps.
    #[test]
    fn an_anchor_of_a_thousand_is_the_right_edge() {
        let work = area();
        let spot = spot_on(work, 640, 484, (1000, 180));
        assert_eq!(
            spot.x + spot.width,
            work.right,
            "the window's right edge is the area's right edge"
        );
        assert_eq!(
            spot.x, 1280,
            "and the window is not where the middle puts it"
        );
    }

    /// The anchor the host sets is the anchor the window places at.
    ///
    /// The numbers are clamped to the area: an anchor outside it is an
    /// anchor the window cannot honour without leaving the screen, and
    /// a launcher that honoured it anyway would be half off the desktop.
    #[test]
    fn the_anchor_the_host_sets_is_the_anchor_the_window_keeps() {
        let mut window = Window::new("rgxy-test").expect("the window was made");
        assert_eq!(
            window.anchor, DEFAULT_ANCHOR,
            "the window starts halfway across and eighteen per cent down"
        );
        window.set_anchor(0, 0);
        assert_eq!(window.anchor, (0, 0), "the corner");
        window.set_anchor(1000, 1000);
        assert_eq!(window.anchor, (1000, 1000), "the far corner");
        window.set_anchor(1001, 5000);
        assert_eq!(
            window.anchor,
            (1000, 1000),
            "an anchor outside the area is clamped to it"
        );
    }

    /// The theme the window is given is the theme it paints in.
    ///
    /// The colours live in the state the procedure reads, so the theme
    /// the host sets is the theme the next paint wears — and the alpha
    /// is clamped to the range a panel is a panel in: below a quarter
    /// the panel is made of nothing, and above 255 the byte cannot
    /// carry it.
    #[test]
    fn the_theme_the_window_is_given_is_the_theme_it_paints_in() {
        let mut window = Window::new("rgxy-test").expect("the window was made");
        assert_eq!(
            window.state().colours,
            paint::Colours::WARM,
            "the window starts in the palette it was drawn in"
        );
        assert_eq!(window.state().panel_alpha, 230, "ninety per cent");
        window.set_colours(Theme::NORD, 200);
        assert_eq!(window.state().colours, paint::Colours::of(&Theme::NORD));
        assert_eq!(window.state().panel_alpha, 200);
        window.set_colours(Theme::WARM, 0);
        assert_eq!(
            window.state().panel_alpha,
            64,
            "an alpha of nothing is a quarter"
        );
        window.set_colours(Theme::WARM, 4000);
        assert_eq!(
            window.state().panel_alpha,
            255,
            "an alpha above opaque is opaque"
        );
    }

    /// The panel's own colour is the theme's own, whatever the theme is.
    ///
    /// The composition reads the picture back and decides which pixels
    /// are the panel: the decision follows the colours the picture was
    /// painted in, and a tolerance pinned to one theme's channels would
    /// treat every other theme's panel as drawing — and paint it opaque,
    /// a window that is a solid rectangle in every theme but one.
    #[test]
    fn the_panel_s_own_colour_is_the_theme_s_own() {
        let warm = paint::Colours::of(&Theme::WARM);
        let light = paint::Colours::of(&Theme::LIGHT);
        // The pixel each panel paints of itself is its own...
        assert!(is_the_panel(
            warm.background_blue,
            warm.background_green,
            warm.background_red,
            &warm
        ));
        assert!(is_the_panel(
            light.background_blue,
            light.background_green,
            light.background_red,
            &light
        ));
        // ...and one theme's panel is the other theme's drawing.
        assert!(!is_the_panel(
            warm.background_blue,
            warm.background_green,
            warm.background_red,
            &light
        ));
        // The tolerance follows the theme it is given: the tolerance's
        // own width away from the panel's colour is still the panel, and
        // one past it is not.
        assert!(is_the_panel(
            warm.background_blue + PANEL_TOLERANCE as u8,
            warm.background_green,
            warm.background_red,
            &warm
        ));
        assert!(!is_the_panel(
            warm.background_blue + PANEL_TOLERANCE as u8 + 1,
            warm.background_green,
            warm.background_red,
            &warm
        ));
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
        let spot = spot_on(work, 2400, 484, DEFAULT_ANCHOR);
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

    /// The second launcher's message shows the window, and does not hide
    /// it when it is already up.
    ///
    /// The product's rule: a start of `rgxy` that finds the launcher
    /// already running asks the window to show itself and leaves, and a
    /// user who typed the command twice is a user asking to *see* the
    /// launcher. A toggle would be the wrong answer: the second start
    /// would hide a window the first one put up.
    #[test]
    fn the_second_launchers_message_shows_the_window() {
        let _screen = Screen::take();
        let mut window = Window::new("rgxy-test").expect("the window was made");
        let hwnd = window.hwnd();
        assert!(!window.is_visible(), "the window starts hidden");

        // The message the second launcher posts, and the window's own
        // dispatch answering it the way the pump does. The queue may hold
        // other messages first — a window that was created and never
        // shown still gets the platform's own — so the pump reads until
        // the message it is looking for has been read.
        fn pump_until(window: &mut Window, want: Event) -> bool {
            for _ in 0..20 {
                let mut msg = MSG {
                    hwnd: core::ptr::null_mut(),
                    message: 0,
                    wParam: 0,
                    lParam: 0,
                    time: 0,
                    pt: POINT { x: 0, y: 0 },
                };
                let got = unsafe { GetMessageW(&mut msg, core::ptr::null_mut(), 0, 0) };
                assert!(got > 0, "the message arrived: got {got}");
                let mut asked = None;
                // The host's own answer: a showing control carrying the
                // frame the window would be given. The test's frame is
                // the default one — the picture is not what this test is
                // about, the message and the showing are.
                let control = window.dispatch(&msg, &mut |event| {
                    asked = Some(event.clone());
                    match event {
                        Event::Show | Event::Toggle => Control::Show(View::default(), 600, 89),
                        _ => Control::None,
                    }
                });
                assert!(window.apply(control), "the window is not done");
                if asked.as_ref() == Some(&want) {
                    return true;
                }
            }
            false
        }

        unsafe {
            PostMessageW(hwnd, WM_SHOW, 0, 0);
        }
        assert!(
            pump_until(&mut window, Event::Show),
            "the message asks for a showing"
        );
        assert!(window.is_visible(), "the window is on the screen");

        // And again: the window is up, and the showing leaves it up.
        unsafe {
            PostMessageW(hwnd, WM_SHOW, 0, 0);
        }
        assert!(
            pump_until(&mut window, Event::Show),
            "the second message asks for a showing too"
        );
        assert!(window.is_visible(), "a showing leaves a shown window up");

        window.hide();
    }

    /// The window is shown with the keyboard.
    ///
    /// The platform refuses `SetForegroundWindow` to a process that is not
    /// the foreground one and did not receive the last input event, and the
    /// launcher is that process: the window is shown without activating the
    /// desktop under it, and the user's last keystroke went to the editor
    /// behind. Without the join the window comes up with the other
    /// application still holding the keyboard, and the user types into the
    /// editor.
    ///
    /// The unit can prove the intent — the window shown last has the
    /// keyboard — and the cross-process half is the live measurement on
    /// `take_the_keyboard`: with Notepad focused, the double-Control hotkey
    /// answered the window to `GetForegroundWindow`.
    #[test]
    fn the_window_takes_the_keyboard() {
        let _screen = Screen::take();
        // The window that had the keyboard is given it back at the end:
        // the suite runs on the machine the user is working on, and a
        // test that leaves the keyboard on a hidden window is a test the
        // user has to find their way out of. It is read before the probe,
        // so the window given back is the user's and not the probe.
        let before = unsafe { GetForegroundWindow() };
        // A session that will not let a window take the foreground cannot
        // answer this question: the CI's windows runner has no user at a
        // keyboard, the platform refuses the call for every window there,
        // and a test that failed on the refusal would say the launcher is
        // broken on a machine where nothing can be foreground. The probe
        // is a window of this test's own, and the test says nothing about
        // a session where the probe cannot take it.
        let mut probe = Window::new("rgxy-test").expect("the probe window was made");
        unsafe {
            ShowWindow(probe.hwnd(), SW_SHOWNOACTIVATE);
            SetForegroundWindow(probe.hwnd());
        }
        let can_take = unsafe { GetForegroundWindow() } == probe.hwnd();
        probe.hide();
        drop(probe);
        if !can_take {
            return;
        }
        let mut first = Window::new("rgxy-test").expect("the window was made");
        let mut second = Window::new("rgxy-test").expect("the second window was made");

        first.show();
        assert_eq!(
            unsafe { GetForegroundWindow() },
            first.hwnd(),
            "the window that is shown has the keyboard"
        );

        // A second showing takes the keyboard from the first: a launcher
        // that comes up under the keyboard is a search box the user
        // cannot type into, and the text lands in the window behind it.
        second.show();
        assert_eq!(
            unsafe { GetForegroundWindow() },
            second.hwnd(),
            "the window shown last has the keyboard"
        );

        first.hide();
        second.hide();
        if unsafe { GetForegroundWindow() } == second.hwnd() {
            unsafe {
                SetForegroundWindow(before);
            }
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

    /// A focus loss under a window that is not on the screen opens
    /// nothing.
    ///
    /// The notification area's own menu puts the window's thread in
    /// front of the user and then drops it — `SetForegroundWindow`
    /// before the menu opens, the deactivation after it closes — and
    /// the deactivation reaches a window that is not on the screen.
    /// Answering it would bring the launcher back up: the user walked
    /// away from a menu, and the launcher opened itself.
    #[test]
    fn a_focus_loss_under_a_hidden_window_opens_nothing() {
        let _screen = Screen::take();
        let mut window = Window::new("rgxy-test").expect("the window was made");
        let hwnd = window.hwnd();
        window.hide();
        assert!(!window.is_visible(), "the window is not on the screen");

        let mut msg = MSG {
            hwnd: core::ptr::null_mut(),
            message: 0,
            wParam: 0,
            lParam: 0,
            time: 0,
            pt: POINT { x: 0, y: 0 },
        };
        // The goodbye of the hide itself is answered first, the way the
        // window procedure answers it: a LATER deactivation is not the
        // window's own.
        unsafe {
            PostMessageW(hwnd, WM_ACTIVATEAPP, 0, 0);
        }
        unsafe {
            let got = GetMessageW(&mut msg, core::ptr::null_mut(), 0, 0);
            assert!(got > 0, "the goodbye arrived");
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        assert!(
            !window.state().focus_lost,
            "the window's own goodbye is not the user going somewhere"
        );

        // The deactivation of the hidden window: the tray menu closing.
        unsafe {
            PostMessageW(hwnd, WM_ACTIVATEAPP, 0, 0);
        }
        unsafe {
            let got = GetMessageW(&mut msg, core::ptr::null_mut(), 0, 0);
            assert!(got > 0, "the deactivation arrived");
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        assert!(
            window.state().focus_lost,
            "the platform said the activation was lost"
        );

        // The pump's answer: no event is asked, the window stays where it
        // is, and the flag does not linger for the next showing.
        let mut asked = 0;
        assert!(
            window.get_out_of_the_way(&mut |_| {
                asked += 1;
                Control::Hide
            }),
            "the window is not done"
        );
        assert_eq!(asked, 0, "a hidden window has nowhere to go: {asked}");
        assert!(
            !window.state().focus_lost,
            "the flag was consumed, not saved for the next showing"
        );

        // The same focus loss under a window that IS on the screen is the
        // user going somewhere, and the host is asked.
        window.show();
        assert!(window.is_visible(), "the window is on the screen");
        window.state_mut().focus_lost = true;
        assert!(
            window.get_out_of_the_way(&mut |_| {
                asked += 1;
                Control::Hide
            }),
            "the window is not done"
        );
        assert_eq!(asked, 1, "the host was asked once: {asked}");
        // The window the test put on the screen goes away with the test:
        // a suite that leaves a window up is a suite that leaves the
        // keyboard on a window nobody is looking at.
        window.hide();
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
