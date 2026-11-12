// SPDX-License-Identifier: Apache-2.0

//! The words the window and the driver share.
//!
//! These are the launcher's public types on Linux. They are the **same**
//! types `rgy-launcher-windows` exports — the same `Key`, `Event`, `View`,
//! `Control`, `Compositor` and `Awareness` — because the window driver in
//! `bins/rgxy/src/gui.rs` is one driver: it does not know which platform it
//! is drawing on. A port that invented a second vocabulary would need a
//! second driver, and two drivers are two places for the same bug to live.
//!
//! The types live in their own module, un-gated by `target_os`, for one
//! reason beyond the shape of the interface: the decisions they carry —
//! which key a keysym is, what a frame says — are bytes, and bytes can be
//! tested on the host that a Linux binary cannot run on.

use alloc::string::String;
use alloc::vec::Vec;

use rgy_launcher::latency::Latency;
use rgy_launcher::theme::Theme;
use rgy_launcher::{Action, Mode, Preview, Row};

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
    /// toggle: a double click is two clicks, and a toggle would show the
    /// window and hide it again.
    Show,
    /// The launcher's settings were asked for: show the window with the
    /// panel open.
    Settings,
    /// Something the window is waiting for has arrived.
    Wake,
    /// The window was closed.
    Quit,
    /// The clock ticked: the host decides whether the typing has settled.
    Tick,
}

/// What the window draws.
///
/// It owns its strings rather than borrowing them: X11 hands the window a
/// byte-sized event union, not a borrow, and the driver's frame is a value
/// that has to outlive the call that made it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct View {
    /// The query as the user typed it.
    pub query: String,
    /// Where the caret is, counted in characters from the start.
    pub caret: usize,
    /// The line the empty field shows.
    pub placeholder: String,
    /// The rows on the screen.
    pub rows: Vec<Row>,
    /// Which row is selected, relative to `top`.
    pub selected: usize,
    /// The index of the first row on the screen.
    pub top: usize,
    /// The panel that has the keyboard.
    pub mode: Mode,
    /// The actions of the selected result.
    pub menu: Vec<Action>,
    /// Which action of the menu is selected.
    pub menu_selected: usize,
    /// The warnings under the field.
    pub warnings: Vec<String>,
    /// The one line under the field: what is happening.
    pub status: String,
    /// How long the answers take.
    pub latency: Latency,
    /// The panel over the results, when there is one.
    pub preview: Option<Preview>,
    /// The rows of the settings panel: label, value.
    pub settings: Vec<(String, String)>,
    /// Which row of the settings panel is selected.
    pub settings_selected: usize,
    /// The colours this frame is painted in.
    pub theme: Theme,
    /// How opaque the panel is.
    pub alpha: u32,
    /// Where the window sits on the work area, in thousandths.
    pub anchor: (u32, u32),
    /// How wide the window is, in the model's units.
    pub width: u32,
}

/// What the host says the window should do about the event it was given.
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

/// What the compositor said about the window's own picture.
///
/// X11 has no compositor refusing a window's own pixels: a window always
/// gets its drawable. The answer is kept so that the driver's question —
/// the one it asks a Windows compositor — has one answer on both hosts.
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
/// X11 reports pixels, not a scale: the launcher is drawn at one unit per
/// pixel and the answer is `PerMonitor`, the answer that says nothing has
/// to be stretched. The variants are the Windows ones because the driver
/// does not ask which platform it is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Awareness {
    /// The process knows the scale of the monitor the window is on.
    PerMonitor,
    /// The process knows the system's scale.
    System,
    /// The process does not know the scale at all.
    Unaware,
}
