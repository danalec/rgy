// SPDX-License-Identifier: Apache-2.0

//! The launcher's state.
//!
//! What the window is showing, what the user has typed, which result is
//! selected, and what the daemon is being asked. It is the whole of the
//! launcher's behaviour that is not a platform call — and a platform call
//! is a window, a font, a key and a file.
//!
//! The state machine has three shapes, and they are the three things the
//! launcher can be:
//!
//! | mode | what is on the screen |
//! |---|---|
//! | `Hidden` | nothing; the launcher is resident and waiting |
//! | `Typing` | the input field, and the results under it |
//! | `Menu` | the actions of the selected result |
//!
//! The results are the **last** answer's until a new one arrives: the list
//! is not emptied on a keystroke. A launcher that blanks the list on every
//! keystroke is a launcher that flickers.
//!
//! The preview is not a mode either: it is a panel **over** the results,
//! and it can be there while the user is still typing. Esc closes it, and
//! then closes the window.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::actions::Action;
use crate::commands;
use crate::hotkey::DoubleCtrl;
use crate::layout::{Layout, SIDE_BAR_WIDTH};
use crate::query::Query;
use crate::results::{Results, Row};

/// How many rows the window shows when the host has not said otherwise.
///
/// The window's height is computed from the rows, so a list of fifty
/// rows on a 1080p screen puts half of it under the bottom of the
/// screen — the row the user is navigating with the arrow keys included.
/// Twelve rows are 408 pixels of list and 76 of chrome: 484 pixels,
/// which fits on the smallest laptop screen there is with room to
/// spare, and which is less than half of a 1080p one.
///
/// The host knows the screen and the model does not, so the host can say
/// another number with [`App::set_visible_rows`].
pub const DEFAULT_VISIBLE_ROWS: usize = 12;

/// How many rows of the launcher's geometry fit in the height it is given.
///
/// The window's chrome — the field, the padding and the border — is what
/// is left over, and the rest is rows. The host knows the height the window
/// can use and the model knows the height of a row, and this is where the
/// two meet.
///
/// That height is not the whole of the work area: the host hands this the
/// room the window can actually occupy (see [`room_below_anchor`]). The
/// window does not start at the top of the area, and a clipped window
/// cannot show rows the model counted for a taller one.
///
/// A screen too short for the window still gets a row: a window that
/// shows nothing cannot show the selection either.
pub fn rows_that_fit(screen_height: u32) -> usize {
    let layout = Layout::default();
    let room = screen_height.saturating_sub(layout.height());
    // The default is the ceiling: a launcher that shows every result the
    // daemon has is a launcher that fills the screen, and the list scrolls
    // with the arrow keys anyway. The screen decides how many rows there
    // is room for; it does not decide how many the launcher wants.
    (room / layout.row_height).clamp(1, DEFAULT_VISIBLE_ROWS as u32) as usize
}

/// The room a window has below its vertical anchor, in the model's units.
///
/// The window is placed `anchor_y` permille down the work area and is
/// clipped to the room below that place (the platform's `spot_on`). So
/// [`rows_that_fit`] must be given that room, not the whole area: counted
/// from the whole area it would ask for rows a clipped window cannot draw,
/// and the picture would silently cut off the bottom rows and the last
/// entries of the actions column.
///
/// The offset is capped so that a window pinned to the very bottom of the
/// area still has a row of room: an anchor is a place, not a way of not
/// being there.
pub fn room_below_anchor(area_height: u32, anchor_y: u32) -> u32 {
    let taken = area_height.saturating_mul(anchor_y.min(1000)) / 1000;
    let offset = taken.min(area_height.saturating_sub(1));
    area_height.saturating_sub(offset)
}

/// What the launcher is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// The window is not on the screen. The launcher is resident and
    /// waiting for the hotkey.
    #[default]
    Hidden,
    /// The input field, and the results under it.
    Typing,
    /// The actions of the selected result.
    Menu,
    /// The launcher's own settings.
    Settings,
}

/// What the preview panel shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Preview {
    /// The name of the file, as the panel's title.
    pub title: String,
    /// The lines of the file.
    pub lines: Vec<String>,
}

/// The line the ids a client names stay under.
///
/// The client's space is `[0, 2^52)`: below the daemon's and below the
/// wire's. An id above the wire's line would reach the daemon as a
/// different number, and the daemon would keep the answer under a key the
/// launcher never named.
const CLIENT_SESSIONS: u64 = 1 << 52;

/// The line the daemon's own session ids live above.
///
/// The daemon's space is `[2^52, 2^53)`: the ids it names are above the
/// ones a client names and below the wire's limit, and the two spaces
/// cannot meet — a client that names an id the daemon would have named
/// does not land on a session of somebody else's.
const DAEMON_SESSIONS: u64 = 1 << 52;

/// The line the wire's numbers stay under.
///
/// The protocol's JSON reader has no integer and reads a number as an
/// `f64`, and an integer above `2^53` is not exactly representable there:
/// a session id above it reaches the peer as a different number. Both
/// spaces stay under this line — the client's and the daemon's.
const WIRE_SESSIONS: u64 = 1 << 53;

/// How many bits the window's own number takes.
///
/// A counter of the windows this process has opened: a million of them is
/// a launcher opened once a second for twelve days.
const COUNTER_BITS: u32 = 20;

/// The mask of the counter's bits.
const COUNTER_MASK: u64 = (1 << COUNTER_BITS) - 1;

/// The mask of the seed's bits: the ones above the counter's, up to the
/// client's line.
///
/// The client's space is `[0, 2^52)` and the counter takes the low 20 of
/// its bits, so the seed has the 32 above them: the seed is in
/// `[1, 2^32)` — never zero, because zero is the marker of a seed that has
/// not been computed — and a session id is in `[2^20, 2^52)`.
const SEED_MASK: u64 = (CLIENT_SESSIONS >> COUNTER_BITS) - 1;

// The two spaces must not meet: the client's ends where the daemon's
// begins, and the daemon's ends below the wire's `2^53` — a number above
// that does not survive the JSON round trip, and an id the launcher never
// named would keep the answer under a key nobody can name again. Checked
// here, where the lines are written down, so that a line that moves
// cannot break the rule quietly.
const _: () = assert!(CLIENT_SESSIONS <= DAEMON_SESSIONS);
const _: () = assert!(DAEMON_SESSIONS <= WIRE_SESSIONS);

/// The seed of this process: the part of a session id that says which
/// launcher named it.
///
/// Zero means "not computed yet" — the seed is never zero, because an
/// un-computed seed and a zero one cannot be told apart.
static SEED: AtomicU64 = AtomicU64::new(0);

/// The counter of the windows this process has opened.
///
/// It is the counter of the **process** and not of the window: two models
/// in one process take their numbers from the same counter, so two
/// windows never land on the same session.
static COUNTER: AtomicU64 = AtomicU64::new(1);

/// Something with an address, for the seed to read.
///
/// A static in this crate: the image is loaded at a random base where the
/// loader relocates it, and two processes of the same binary do not get
/// the same base. The address of a static is the address of the image.
static ANCHOR: u8 = 0;

/// A session id this process has not named before.
///
/// The id is the seed above and the counter below: the high bits say
/// which launcher named the session, and the low bits say which window it
/// is. Two launchers therefore name different sessions for the same
/// window, and two windows of one launcher name different sessions too.
///
/// The id is in `[2^20, 2^52)`: the client's own space, and under the
/// wire's limit, so the daemon reads the very number that was sent.
///
/// The daemon keys its result sets by this id, so two launchers that
/// named the same one would share a result set: one launcher's answer
/// would be re-ranked against the other's, and the answer to a question
/// would come back as the answer to another.
fn next_session() -> u64 {
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed) & COUNTER_MASK;
    let seed = seed() & SEED_MASK;
    (seed << COUNTER_BITS) | counter
}

/// The seed of this process, computed once.
///
/// The first caller computes it and the rest read it: the seed is the
/// same for every window of the process, and a seed that changed from one
/// window to the next would put two windows of one launcher in two spaces.
fn seed() -> u64 {
    let taken = SEED.load(Ordering::Relaxed);
    if taken != 0 {
        return taken;
    }
    // The low bit is set so that the seed is never zero: zero is the
    // marker of a seed that has not been computed yet.
    let computed = process_seed() | 1;
    match SEED.compare_exchange(0, computed, Ordering::Relaxed, Ordering::Relaxed) {
        Ok(_) => computed,
        Err(taken) => taken,
    }
}

/// The seed of this process, out of the addresses the loader gave it.
///
/// Two launcher processes must not name the same session: the daemon keys
/// its result sets by the id, and one launcher's answer re-ranked against
/// the other's is an answer to somebody else's question.
///
/// The addresses are where the uniqueness comes from. The loader puts the
/// stack at a random address, the heap at another, and the image at a
/// third where it relocates it — and two processes of the same binary do
/// not get the same three. The mixture of them is what the seed is.
///
/// There is no clock and no process id in it, and no call to either: this
/// crate is `no_std`, and the tree's seam (`rgy_discovery`) is not this
/// crate's to call — a platform call in the model would be a call the
/// tree does not declare. The addresses are what `core` has on every
/// platform the launcher runs on, and the seed is computed once per
/// process, so the cost is one small allocation.
///
/// The entropy is the loader's, and the loader's is not the same on every
/// platform: on a host that randomizes none of the three, two processes
/// would take the same seed and the ids would meet. The three sources are
/// mixed because they are randomized apart from each other — a host that
/// randomizes one of them is enough.
fn process_seed() -> u64 {
    // The address of a local: the stack is at a random address, and two
    // processes of one binary do not get the same one.
    let here = 0u8;
    let stack = core::ptr::addr_of!(here) as usize as u64;
    // The address of a block on the heap: the heap is randomized apart
    // from the stack, and this is another randomizer again.
    let block = Box::new(0u8);
    let heap = &*block as *const u8 as usize as u64;
    // The address of a static in this crate: the image is loaded at a
    // random base where the loader relocates it.
    let image = core::ptr::addr_of!(ANCHOR) as usize as u64;
    mix(stack) ^ mix(heap.rotate_left(21)) ^ mix(image.rotate_left(42))
}

/// The mixer: the bits of an address into the bits of a seed.
///
/// The splitmix64 finalizer — a multiply, a shift and a multiply — and
/// every bit of the address reaches every bit of the seed. The seed is
/// cut to its own bits afterwards, so the bits that are kept are the
/// mixed ones and not the raw address.
fn mix(value: u64) -> u64 {
    let mut z = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The launcher's state.
#[derive(Debug, Clone)]
pub struct App {
    mode: Mode,
    query: Query,
    results: Results,
    hotkey: DoubleCtrl,
    /// The session the daemon knows this window by.
    ///
    /// A new one every time the window opens: the previous window's result
    /// set is not the answer to a new question, and a stale session would
    /// make the daemon re-rank against it.
    ///
    /// The id is this process's seed above the window's own number, so two
    /// launcher processes never name the same session: they would share a
    /// result set in the daemon, and one launcher's answer would be
    /// re-ranked against the other's.
    ///
    /// Zero is the session of a window that has not opened yet: a real one
    /// is never zero, because the seed is never zero.
    session: u64,
    /// The warnings the last answer carried.
    warnings: Vec<String>,
    /// The panel over the results, when there is one.
    preview: Option<Preview>,
    /// Which action of the menu is selected.
    menu_index: usize,
    /// Which row of the settings panel is selected.
    settings_index: usize,
    /// How many rows the settings panel draws while it has the keyboard.
    ///
    /// The panel is the **host's** list — the settings are the host's, and
    /// it is the host that knows how many rows it has to draw — and the
    /// room for them is the geometry's. Zero when no panel is open, which
    /// is what makes the window's height the results'.
    settings_rows: usize,
    /// How wide the window is, in model units.
    ///
    /// The width is the user's dimension: the height is the rows', and
    /// the rows are the screen's. The default is the width the window
    /// has always had.
    width: u32,
}

impl Default for App {
    fn default() -> App {
        App::new()
    }
}

impl App {
    /// A launcher that is hidden, with an empty query.
    pub fn new() -> App {
        let mut results = Results::new();
        // The window is sized for the rows it shows, so the model has to
        // know how many it shows. Without this the window is sized for
        // every result, and half the list ends up under the bottom of the
        // screen.
        results.set_visible(DEFAULT_VISIBLE_ROWS);
        App {
            mode: Mode::Hidden,
            query: Query::new(),
            results,
            hotkey: DoubleCtrl::new(),
            session: 0,
            warnings: Vec::new(),
            preview: None,
            menu_index: 0,
            settings_index: 0,
            settings_rows: 0,
            width: Layout::default().width,
        }
    }

    /// What the launcher is showing.
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Whether the window is on the screen.
    pub fn is_visible(&self) -> bool {
        self.mode != Mode::Hidden
    }

    /// Show the window, and start a new session.
    ///
    /// The query is kept: a user who closes the launcher to look at
    /// something and comes back wants the same question, and the results
    /// are kept too because they are the answer to it.
    ///
    /// The preview is **not** kept: it belongs to the result that was
    /// selected, and a new window selects a new one.
    pub fn show(&mut self) {
        if self.mode == Mode::Hidden {
            self.session = next_session();
        }
        self.mode = Mode::Typing;
        self.preview = None;
    }

    /// Hide the window. The launcher stays resident.
    pub fn hide(&mut self) {
        self.mode = Mode::Hidden;
        self.menu_closed();
        self.preview = None;
    }

    /// Show the window, or hide it if it is already on the screen.
    pub fn toggle(&mut self) {
        if self.is_visible() {
            self.hide();
        } else {
            self.show();
        }
    }

    /// The session the daemon knows this window by.
    pub fn session(&self) -> u64 {
        self.session
    }

    /// The query, as the user typed it.
    pub fn query(&self) -> &Query {
        &self.query
    }

    /// The query the engine is asked for.
    ///
    /// The launcher's short words become the engine's words; everything else
    /// is the user's, untouched. The **name** part is what the engine
    /// ranks — the content part is the second layer's, and the engine never
    /// sees it.
    pub fn engine_query(&self) -> String {
        // The same string is the scope of a content search: the files the
        // content layer reads are the files the engine chose, and the
        // engine is asked for them in the engine's own words.
        self.query.halves().engine()
    }

    /// The content pattern, when the query carries the prefix.
    pub fn content_query(&self) -> Option<&str> {
        self.query.halves().content()
    }

    /// The layer the query is about.
    pub fn layer(&self) -> crate::layer::Layer {
        self.query.halves().layer()
    }

    /// Type one character.
    pub fn insert(&mut self, ch: char) {
        self.query.insert(ch);
    }

    /// Type a whole string, the way a paste arrives.
    pub fn insert_str(&mut self, text: &str) {
        self.query.insert_str(text);
    }

    /// Delete the last character.
    pub fn backspace(&mut self) {
        self.query.backspace();
    }

    /// Delete the character at the caret.
    ///
    /// The **forward** deletion: `Delete` takes the character under the
    /// caret, and `Backspace` the one before it.
    ///
    /// False when there was nothing to delete — the caret is at the end of
    /// the field — and the caller has nothing to ask the daemon about.
    pub fn delete_forward(&mut self) -> bool {
        self.query.delete_forward()
    }

    /// Delete the word before the caret.
    pub fn delete_word(&mut self) {
        self.query.delete_word();
    }

    /// Empty the query.
    pub fn clear_query(&mut self) {
        self.query.clear();
        self.results.clear();
    }

    /// Move the caret one character to the left.
    pub fn move_left(&mut self) {
        self.query.left();
    }

    /// Move the caret one character to the right.
    pub fn move_right(&mut self) {
        self.query.right();
    }

    /// Move the caret to the start of the query.
    pub fn move_home(&mut self) {
        self.query.home();
    }

    /// Move the caret to the end of the query.
    pub fn move_end(&mut self) {
        self.query.end();
    }

    /// The results.
    pub fn results(&self) -> &Results {
        &self.results
    }

    /// The selected row.
    pub fn current(&self) -> Option<&Row> {
        self.results.current()
    }

    /// Select the next row.
    pub fn select_next(&mut self) {
        self.results.select_next();
    }

    /// Select the previous row.
    pub fn select_previous(&mut self) {
        self.results.select_previous();
    }

    /// Select the row `n` places down.
    ///
    /// A page is the height of the list: the user pressed "page down", so
    /// the selection moves by what is on the page, not by one.
    pub fn select_page(&mut self, down: bool) {
        let (_, rows) = self.results.window();
        let step = rows.max(1);
        for _ in 0..step {
            if down {
                self.results.select_next();
            } else {
                self.results.select_previous();
            }
        }
    }

    /// The rows the window shows.
    ///
    /// This is the host's way of saying how many rows fit on the screen:
    /// the window is sized from them, and the list scrolls when there are
    /// more of them than that. A host that says nothing gets the
    /// default.
    pub fn set_visible_rows(&mut self, rows: usize) {
        self.results.set_visible(rows);
    }

    /// The answer came back.
    pub fn set_results(&mut self, rows: Vec<Row>) {
        self.results.set(rows);
    }

    /// The answer carried these warnings.
    pub fn set_warnings(&mut self, warnings: Vec<String>) {
        self.warnings = warnings;
    }

    /// The warnings the last answer carried.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// The geometry, for the rows the window shows.
    ///
    /// The rows are the ones the window shows and not the ones there are:
    /// the height is computed from them, and a window sized for a list
    /// that does not fit on the screen is a window with its bottom half
    /// under the bottom of the screen.
    pub fn layout(&self) -> Layout {
        let (_, rows) = self.results.window();
        let mut layout = Layout::with_rows(rows as u32).with_width(self.width);
        // The preview panel takes the room the results would have taken:
        // it is a panel **over** them, and a window that grew for it would
        // jump every time the user opened one.
        if let Some(preview) = &self.preview {
            layout.preview = preview.lines.len() as u32 + 1;
        }
        // The column of actions is inside the window: the window does not
        // grow for it, the results give up the room. The picture and the
        // geometry have to agree — a window that draws a column the
        // geometry does not know about draws it over the names.
        //
        // The room is the geometry's rule and not the model's: the picture
        // asks the same question, and the two have to answer it the same
        // way or the column is drawn over the names.
        // In `Mode::Settings` the panel has the keyboard and the column
        // is an inert legend: it squeezes the panel's label and value
        // columns and, with stale results on the screen, makes the
        // panel narrower than it is without them. The settings panel
        // owns the whole content width while it holds the keyboard, so
        // the column is not applied in that mode. `paint::model_of`
        // asks the same question, and the two answer it the same way.
        if self.mode != Mode::Settings {
            let entries = self.menu_actions().len() as u32;
            if layout.column_fits(entries) {
                layout = layout.with_side_bar(SIDE_BAR_WIDTH);
            }
        }
        // The settings panel is its own list: the results give up the
        // room to it, and a command line has no results at all — so the
        // room is made from the panel's rows and not from the list's.
        if self.mode == Mode::Settings {
            layout.panel = self.settings_rows as u32;
        }
        layout
    }

    /// The hotkey's state machine.
    pub fn hotkey(&mut self) -> &mut DoubleCtrl {
        &mut self.hotkey
    }

    /// How wide the window is, in model units.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Set how wide the window is, in model units.
    ///
    /// The width is the settings' to give and the geometry's to use: a
    /// value outside the range the settings hold is clamped here, so the
    /// window can never be asked for a width the layout cannot carry.
    pub fn set_width(&mut self, width: u32) {
        self.width = width.clamp(400, 1000);
    }

    /// Open the actions of the selected result.
    pub fn open_menu(&mut self) -> bool {
        if self.current().is_none() {
            return false;
        }
        self.mode = Mode::Menu;
        self.menu_index = 0;
        true
    }

    /// The actions the menu offers for the selected result.
    ///
    /// The list follows the row: a folder cannot be previewed and does
    /// not get the preview in its list. It is empty when there is no
    /// result, because there is nothing to act on.
    pub fn menu_actions(&self) -> &'static [Action] {
        match self.current() {
            Some(row) => Action::for_kind(row.icon()),
            None => &[],
        }
    }

    /// Which action of the menu is selected.
    pub fn menu_index(&self) -> usize {
        self.selected_index()
    }

    /// Select the next action of the menu.
    pub fn menu_next(&mut self) {
        let count = self.menu_actions().len();
        if count == 0 {
            return;
        }
        self.menu_index = (self.selected_index() + 1) % count;
    }

    /// Select the previous action of the menu.
    pub fn menu_previous(&mut self) {
        let count = self.menu_actions().len();
        if count == 0 {
            return;
        }
        let index = self.selected_index();
        self.menu_index = if index == 0 { count - 1 } else { index - 1 };
    }

    /// The action the menu has selected.
    pub fn menu_action(&self) -> Option<Action> {
        self.menu_actions().get(self.selected_index()).copied()
    }

    /// The index of the selected action, inside the list that is offered.
    ///
    /// The list follows the row, and the row can change while the menu
    /// is open — an answer that lands replaces the results. The index is
    /// clamped so that it always names an action that is on the screen.
    fn selected_index(&self) -> usize {
        let count = self.menu_actions().len();
        if count == 0 {
            return 0;
        }
        self.menu_index.min(count - 1)
    }

    /// Close the actions.
    pub fn menu_closed(&mut self) {
        if self.mode == Mode::Menu {
            self.mode = Mode::Typing;
        }
    }

    /// Open the settings panel.
    ///
    /// The panel is over the results, like the preview: the window does
    /// not grow for it, and the results give up the room. The settings
    /// themselves are the host's — the panel shows them and the host
    /// adjusts them — and the model owns the part that is a decision
    /// about the keyboard: which row is selected, and that the panel
    /// has the keyboard while it is open.
    ///
    /// A panel over **no** results has no room, and a list of settings
    /// that is drawn nowhere is a panel the user adjusts blind. So the
    /// host says how many rows the panel draws (see
    /// [`App::set_settings_rows`]) and the geometry makes room for them:
    /// the panel is a list, like the results, and the window is as tall
    /// as the taller of the two.
    pub fn open_settings(&mut self) {
        self.mode = Mode::Settings;
        self.settings_index = 0;
    }

    /// How many rows the settings panel draws.
    ///
    /// The host owns the list and the model owns the room: the host says
    /// the count once, and the geometry uses it while the panel is open.
    /// A panel whose count the geometry did not know was a panel the
    /// window had no room for — the settings were adjusted by keys on a
    /// list that was drawn nowhere.
    pub fn set_settings_rows(&mut self, rows: usize) {
        self.settings_rows = rows;
    }

    /// Close the settings panel, back to the field.
    pub fn settings_closed(&mut self) {
        if self.mode == Mode::Settings {
            self.mode = Mode::Typing;
        }
    }

    /// Which row of the settings panel is selected.
    pub fn settings_index(&self) -> usize {
        self.settings_index
    }

    /// Select the next row of the settings panel.
    ///
    /// The count is the caller's: the rows are the host's — the settings
    /// are — and the model wraps the way every list of the launcher
    /// wraps.
    pub fn settings_next(&mut self, count: usize) {
        if count > 0 {
            self.settings_index = (self.settings_index + 1) % count;
        }
    }

    /// Select the previous row of the settings panel.
    pub fn settings_previous(&mut self, count: usize) {
        if count > 0 {
            self.settings_index = if self.settings_index == 0 {
                count - 1
            } else {
                self.settings_index - 1
            };
        }
    }

    /// The preview panel, when there is one.
    pub fn preview(&self) -> Option<&Preview> {
        self.preview.as_ref()
    }

    /// Show a preview.
    pub fn set_preview(&mut self, preview: Preview) {
        self.preview = Some(preview);
    }

    /// Close the preview.
    pub fn close_preview(&mut self) -> bool {
        if self.preview.is_some() {
            self.preview = None;
            true
        } else {
            false
        }
    }

    /// Whether the query is a command line.
    ///
    /// A command line is not a search, and the launcher takes it before
    /// the engine sees it: the engine does not know `>`, and it would
    /// search for the literal text of the line and answer nothing.
    pub fn is_command(&self) -> bool {
        commands::is_command(self.query.text())
    }

    /// The action the command line names, if it names one.
    pub fn command(&self) -> Option<Action> {
        commands::command_for(self.query.text()).map(|c| c.action)
    }

    /// The action the key means, if the key means one.
    ///
    /// The key is named, not the byte: the platform crate turns a key event
    /// into this and asks.
    pub fn action_for(&self, key: &str) -> Option<Action> {
        match key {
            "enter" => Some(Action::Open),
            "ctrl+enter" => Some(Action::OpenFolder),
            "ctrl+c" => Some(Action::CopyPath),
            "ctrl+p" => Some(Action::Preview),
            "ctrl+k" => Some(Action::Menu),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;

    fn row(i: usize) -> Row {
        Row::new(alloc::format!("/a/file{i}.txt"), i as i32)
    }

    #[test]
    fn a_new_launcher_is_hidden_and_empty() {
        let app = App::new();
        assert_eq!(app.mode(), Mode::Hidden);
        assert!(!app.is_visible());
        assert_eq!(app.query().text(), "");
        assert!(app.results().is_empty());
    }

    #[test]
    fn showing_the_window_starts_a_new_session() {
        let mut app = App::new();
        let first = app.session();
        app.show();
        let second = app.session();
        assert_ne!(first, second, "a new window is a new session");
        // And the same window keeps its session while it is open.
        app.show();
        assert_eq!(app.session(), second);
    }

    /// Two windows of one launcher do not share a session.
    ///
    /// The window's own number is the counter's, and the counter moves on
    /// every time a window opens: a window that took a number another one
    /// has taken would share a result set in the daemon, and the answer to
    /// one question would come back re-ranked against the other's.
    #[test]
    fn two_windows_of_one_launcher_do_not_share_a_session() {
        let mut app = App::new();
        app.show();
        let first = app.session();
        app.hide();
        app.show();
        let second = app.session();
        assert_ne!(first, second, "two windows are two sessions");
    }

    /// The session ids stay in the client's own space.
    ///
    /// The client's space is `[0, 2^52)` and the daemon's is
    /// `[2^52, 2^53)`: the two cannot meet, and a client that names an id
    /// the daemon would have named does not land on a session of somebody
    /// else's.
    ///
    /// They stay below the wire's line as well: the protocol's JSON reader
    /// has no integer and reads a number as an `f64`, and an integer above
    /// `2^53` is not exactly representable there — the daemon would keep
    /// the answer under a key the launcher never named.
    #[test]
    fn the_sessions_stay_in_the_clients_own_space() {
        let mut app = App::new();
        for _ in 0..8 {
            app.show();
            let session = app.session();
            assert!(
                session < DAEMON_SESSIONS,
                "the session is in the daemon's own space: {session}"
            );
            assert!(
                session < CLIENT_SESSIONS,
                "the session is not in the client's own space: {session}"
            );
            assert!(
                session < WIRE_SESSIONS,
                "the session does not survive the wire exactly: {session}"
            );
            app.hide();
        }
    }

    /// The child: it says which session it named, and it says it where the
    /// parent can read it.
    ///
    /// It is ignored in an ordinary run and is run by the test below, which
    /// is the one that sets the variable that says where to write.
    #[test]
    #[ignore]
    fn the_child_process_names_a_session() {
        extern crate std;
        let Some(path) = std::env::var_os("RGY_LAUNCHER_SESSION_FILE") else {
            return;
        };
        let mut app = App::new();
        app.show();
        std::fs::write(path, app.session().to_string()).expect("the session is written");
    }

    /// Two launchers in two processes do not name the same session.
    ///
    /// The invariant is about **processes**, so the test is about
    /// processes: the test binary runs again as a child, the child names a
    /// session of its own, and the two ids must not be the same. A counter
    /// on its own cannot show this — two processes would both start at
    /// one — and this is the test that shows what the seed is for.
    ///
    /// What it shows: two processes of this binary, on this machine, name
    /// different sessions. What it does not show: that every pair of
    /// processes on every machine does — the seed is the loader's address
    /// randomization, and a host that randomizes none of the three
    /// addresses the seed is mixed from would give two processes the same
    /// seed. The three are randomized apart from each other.
    #[test]
    fn two_launchers_in_two_processes_do_not_name_the_same_session() {
        extern crate std;
        let dir = std::env::temp_dir().join(alloc::format!(
            "rgy-launcher-session-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the folder is made");
        let file = dir.join("session");
        let exe = std::env::current_exe().expect("the test binary");
        let child = std::process::Command::new(exe)
            .args([
                "--ignored",
                "--exact",
                "app::tests::the_child_process_names_a_session",
            ])
            .env("RGY_LAUNCHER_SESSION_FILE", &file)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("the test binary runs");
        let out = child.wait_with_output().expect("the child finishes");
        assert!(
            out.status.success(),
            "the child must succeed; stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        let said = std::fs::read_to_string(&file).expect("the child said which session it named");
        let theirs: u64 = said
            .trim()
            .parse()
            .expect("the child's session is a number");
        let mut app = App::new();
        app.show();
        let ours = app.session();
        assert_ne!(ours, theirs, "two launchers named the same session: {ours}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_toggle_shows_and_hides() {
        let mut app = App::new();
        app.toggle();
        assert!(app.is_visible());
        app.toggle();
        assert!(!app.is_visible());
    }

    #[test]
    fn the_short_words_reach_the_engine_rewritten() {
        let mut app = App::new();
        app.show();
        for ch in "pic: invoice".chars() {
            app.insert(ch);
        }
        assert_eq!(app.engine_query(), "type:image invoice");
    }

    /// A keystroke changes the question the engine is asked.
    ///
    /// The question is the model: the engine is asked what the user typed,
    /// and a keystroke that did not change it would be a keystroke the
    /// daemon never hears about.
    #[test]
    fn typing_asks_the_daemon_again() {
        let mut app = App::new();
        app.show();
        app.insert('a');
        assert_eq!(app.query().text(), "a");
        assert_eq!(
            app.engine_query(),
            "a",
            "the engine is asked the new question"
        );
    }

    /// A paste is a keystroke that arrived in bulk: the launcher has to
    /// ask the daemon again, exactly as it does for a letter.
    ///
    /// What is checked is the **question**: the text the user pasted is
    /// the text the engine is asked, in the engine's own words.
    #[test]
    fn a_paste_asks_the_daemon_again() {
        let mut app = App::new();
        app.show();
        app.insert_str("invoice");
        assert_eq!(app.query().text(), "invoice");
        assert_eq!(
            app.engine_query(),
            "invoice",
            "the engine is asked the pasted question"
        );
        assert_eq!(app.content_query(), None, "and there is no content part");
        // A paste that carries the content prefix is the second layer's:
        // the names go to the engine and the pattern stays with the
        // content layer.
        app.insert_str(" grep:total");
        assert_eq!(app.query().text(), "invoice grep:total");
        assert_eq!(app.engine_query(), "invoice");
        assert_eq!(app.content_query(), Some("grep:total"));
    }

    #[test]
    fn the_menu_needs_a_result_to_act_on() {
        let mut app = App::new();
        app.show();
        assert!(!app.open_menu(), "there is nothing to act on");
        app.set_results(vec![row(0)]);
        assert!(app.open_menu());
        assert_eq!(app.mode(), Mode::Menu);
        app.menu_closed();
        assert_eq!(app.mode(), Mode::Typing);
    }

    #[test]
    fn the_menu_is_closed_when_the_window_goes_away() {
        let mut app = App::new();
        app.show();
        app.set_results(vec![row(0)]);
        app.open_menu();
        app.hide();
        assert_eq!(app.mode(), Mode::Hidden);
    }

    #[test]
    fn the_keys_mean_the_actions() {
        let app = App::new();
        assert_eq!(app.action_for("enter"), Some(Action::Open));
        assert_eq!(app.action_for("ctrl+enter"), Some(Action::OpenFolder));
        assert_eq!(app.action_for("ctrl+c"), Some(Action::CopyPath));
        assert_eq!(app.action_for("ctrl+p"), Some(Action::Preview));
        assert_eq!(app.action_for("ctrl+k"), Some(Action::Menu));
        assert_eq!(app.action_for("f5"), None);
    }

    #[test]
    fn the_window_is_the_field_and_the_results() {
        let mut app = App::new();
        app.show();
        app.set_results(vec![row(0), row(1), row(2)]);
        let l = app.layout();
        assert_eq!(l.rows, 3);
        assert!(
            l.height() > l.input_height,
            "the window grew for the results"
        );
    }

    #[test]
    fn a_command_line_is_not_a_search() {
        let mut app = App::new();
        app.show();
        for ch in ">folder".chars() {
            app.insert(ch);
        }
        assert!(app.is_command());
        assert_eq!(app.command(), Some(Action::OpenFolder));
    }

    #[test]
    fn a_word_without_the_prefix_is_a_search() {
        let mut app = App::new();
        app.show();
        for ch in "folder".chars() {
            app.insert(ch);
        }
        assert!(!app.is_command());
        assert_eq!(app.command(), None);
    }

    #[test]
    fn the_preview_is_closed_by_the_window_going_away() {
        let mut app = App::new();
        app.show();
        app.set_preview(Preview {
            title: "notas.txt".to_string(),
            lines: alloc::vec!["first line".to_string()],
        });
        assert!(app.preview().is_some());
        assert!(app.close_preview());
        assert!(app.preview().is_none());
        app.set_preview(Preview::default());
        app.hide();
        assert!(app.preview().is_none());
    }

    #[test]
    fn the_page_moves_by_the_height_of_the_list() {
        let mut app = App::new();
        app.show();
        app.set_visible_rows(3);
        app.set_results((0..10).map(row).collect());
        app.select_page(true);
        assert_eq!(app.results().selected(), Some(3));
        app.select_page(false);
        assert_eq!(app.results().selected(), Some(0));
    }

    #[test]
    fn the_window_is_sized_for_the_rows_it_shows_and_not_for_all_of_them() {
        let mut app = App::new();
        app.show();
        app.set_results((0..50).map(row).collect());
        assert_eq!(
            app.results().window(),
            (0, DEFAULT_VISIBLE_ROWS),
            "the window shows a screenful and not the whole answer"
        );
        assert_eq!(
            app.layout().rows as usize,
            DEFAULT_VISIBLE_ROWS,
            "the window is sized for what it shows"
        );
    }

    #[test]
    fn the_host_can_narrow_the_window() {
        let mut app = App::new();
        app.show();
        app.set_results((0..50).map(row).collect());
        app.set_visible_rows(5);
        assert_eq!(app.results().window(), (0, 5));
        assert_eq!(app.layout().rows, 5);
    }

    #[test]
    fn the_list_scrolls_when_it_is_longer_than_the_window() {
        let mut app = App::new();
        app.show();
        app.set_visible_rows(3);
        app.set_results((0..10).map(row).collect());
        assert_eq!(app.results().window(), (0, 3));
        // The selection walks down and the window follows it.
        for _ in 0..3 {
            app.select_next();
        }
        assert_eq!(app.results().selected(), Some(3));
        assert_eq!(app.results().window(), (1, 3), "the window moved down");
        // And it comes back up.
        for _ in 0..3 {
            app.select_previous();
        }
        assert_eq!(app.results().selected(), Some(0));
        assert_eq!(app.results().window(), (0, 3), "the window went back");
    }

    #[test]
    fn the_selection_stays_in_the_window_when_the_answer_is_long() {
        // The window is the default's, and the answer is longer than it:
        // the row the user is on is a row the user can see.
        let mut app = App::new();
        app.show();
        app.set_results((0..50).map(row).collect());
        for _ in 0..DEFAULT_VISIBLE_ROWS {
            app.select_next();
        }
        let (top, count) = app.results().window();
        let selected = app.results().selected().unwrap_or(0);
        assert!(selected >= top, "the selection is not above the window");
        assert!(
            selected < top + count,
            "the selection is not below the window: {selected} in {top}..{top}"
        );
    }

    #[test]
    fn the_rows_that_fit_are_the_ones_the_geometry_holds() {
        // The chrome is the field, the padding and the border; the rest of
        // the screen is rows.
        let layout = Layout::default();
        let room = |screen: u32| (screen - layout.height()) / layout.row_height;
        // A screen that holds fewer rows than the launcher wants gets the
        // rows it holds: the geometry is the first rule.
        assert!(
            room(480) < DEFAULT_VISIBLE_ROWS as u32,
            "the screen is the small one"
        );
        assert_eq!(rows_that_fit(480), room(480) as usize);
        // A screen that holds more gets the default and not the room: a
        // box that covers the display is a box the user cannot see past.
        assert!(
            room(1080) > DEFAULT_VISIBLE_ROWS as u32,
            "the screen is the big one"
        );
        assert_eq!(rows_that_fit(1080), DEFAULT_VISIBLE_ROWS);
        // And a screen too short for the window still gets a row.
        assert_eq!(rows_that_fit(10), 1);
    }

    /// A short work area with the window below its top leaves less room
    /// than the whole area, and the rows have to be counted in that room.
    ///
    /// 1080 device pixels at 200% are 540 model units; an anchor 18% down
    /// leaves 443 of them. Counted from the whole 540 the launcher asks
    /// for twelve rows (497 units) into a window the platform clips to
    /// 443, and the bottom rows are cut off.
    #[test]
    fn a_short_work_area_below_an_offset_keeps_the_rows_in_the_room() {
        let area = 540u32;
        let anchor_y = 180u32; // the default vertical anchor, in permille
        let room = room_below_anchor(area, anchor_y);
        assert!(room < area, "the anchor leaves less than the whole area");

        let rows = rows_that_fit(room) as u32;
        let height = Layout::with_rows(rows).height();
        assert!(
            height <= room,
            "the rows fit in the room below the anchor: {height} <= {room}"
        );

        // Counted from the whole area (what the bug did), the same window
        // asks for more rows than the clipped window can show.
        let naive = Layout::with_rows(rows_that_fit(area) as u32).height();
        assert!(
            naive > room,
            "the whole area asks for a window taller than the room: {naive} > {room}"
        );
    }

    #[test]
    fn the_default_window_fits_on_the_smallest_screen_there_is() {
        let layout = Layout::with_rows(DEFAULT_VISIBLE_ROWS as u32);
        assert!(
            layout.height() <= 768,
            "the default window is {} pixels tall",
            layout.height()
        );
    }

    #[test]
    fn the_engine_query_and_the_scope_are_the_same_string() {
        // The names the content layer reads are the names the engine
        // ranks, and the engine is asked for both in its own words: the
        // launcher has a `pic:` and the engine has a `type:image`.
        let mut app = App::new();
        app.show();
        for ch in "pic: grep:total".chars() {
            app.insert(ch);
        }
        let split = app.query().split();
        assert_eq!(split.names, "pic:");
        assert_eq!(app.engine_query(), "type:image");
        assert_eq!(
            app.engine_query(),
            crate::layer::scope_query(split.names),
            "the scope and the query are one string"
        );
    }

    #[test]
    fn a_delete_deletes_at_the_caret() {
        let mut app = App::new();
        app.insert_str("abc");
        app.move_left();
        app.move_left();
        assert!(app.delete_forward(), "the character under the caret went");
        assert_eq!(app.query().text(), "ac");
        assert_eq!(app.query().caret(), 1, "the caret stayed where it was");
    }

    #[test]
    fn a_delete_at_the_end_of_the_field_deletes_nothing() {
        let mut app = App::new();
        app.insert_str("abc");
        assert!(
            !app.delete_forward(),
            "the end of the field has nothing to delete"
        );
        assert_eq!(app.query().text(), "abc");
    }

    /// The app asks the two layers the two questions.
    ///
    /// The engine is asked the name part in the engine's words, and the
    /// content layer is asked the content part as it was typed: the
    /// launcher's `pic:` is the engine's `type:image`.
    #[test]
    fn the_app_asks_the_two_layers_the_two_questions() {
        let mut app = App::new();
        app.show();
        for ch in "pic: invoice grep:total".chars() {
            app.insert(ch);
        }
        assert_eq!(app.layer(), crate::layer::Layer::Content);
        assert_eq!(app.engine_query(), "type:image invoice");
        assert_eq!(app.content_query(), Some("grep:total"));
    }

    #[test]
    fn the_menu_has_a_selection_that_moves() {
        let mut app = App::new();
        app.show();
        app.set_results(vec![row(0)]);
        app.open_menu();
        assert_eq!(app.menu_index(), 0);
        assert_eq!(app.menu_action(), Some(Action::Open));
        app.menu_next();
        assert_eq!(app.menu_action(), Some(Action::OpenFolder));
        app.menu_previous();
        assert_eq!(app.menu_action(), Some(Action::Open));
        // And it wraps: the menu is a circle, not a list with a bottom.
        app.menu_previous();
        assert_eq!(
            app.menu_action(),
            app.menu_actions().last().copied(),
            "the selection went round"
        );
    }

    /// The menu lists the actions of the selected row.
    ///
    /// The list follows the row: a folder is not offered the preview and
    /// a document is. What the keyboard walks is what the menu shows,
    /// and what it shows depends on what is selected.
    #[test]
    fn the_menu_lists_the_actions_of_the_selected_row() {
        let mut app = App::new();
        app.show();
        app.set_results(vec![
            Row::new("/a/folder".to_string(), 0).with_folder(true),
            Row::new("/a/notas.txt".to_string(), 1),
        ]);
        assert!(
            !app.menu_actions().contains(&Action::Preview),
            "a folder is not previewed"
        );
        app.select_next();
        assert!(
            app.menu_actions().contains(&Action::Preview),
            "a document is"
        );
        // And with no row there is nothing to act on.
        app.set_results(Vec::new());
        assert!(app.menu_actions().is_empty());
    }

    /// The keyboard walks the actions that are offered.
    ///
    /// The circle the menu walks is the list the row offers: a folder's
    /// list has three entries and a document's has four, and the walk
    /// never lands on an action that is not there.
    #[test]
    fn the_menu_walks_the_actions_that_are_offered() {
        let mut app = App::new();
        app.show();
        app.set_results(vec![Row::new("/a/folder".to_string(), 0).with_folder(true)]);
        app.open_menu();
        assert_eq!(app.menu_action(), Some(Action::Open));
        app.menu_next();
        assert_eq!(app.menu_action(), Some(Action::OpenFolder));
        app.menu_next();
        assert_eq!(app.menu_action(), Some(Action::CopyPath));
        // And the circle closes on the first one: there is no preview to
        // walk to.
        app.menu_next();
        assert_eq!(app.menu_action(), Some(Action::Open));
        assert_eq!(app.menu_index(), 0);
    }

    /// The window carries the column of actions when the results give it
    /// room.
    ///
    /// The column is inside the window: the results give up the room and
    /// the window does not grow — a window that moved under the user's
    /// hands every time the actions opened is a window they fight. A
    /// window with one result is shorter than the column, and a column
    /// that does not fit is not drawn half of itself.
    #[test]
    fn the_layout_carries_the_column_when_the_results_give_it_room() {
        let mut app = App::new();
        app.show();
        app.set_results(vec![row(0)]);
        let short = app.layout();
        assert_eq!(
            short.side_bar_width(),
            0,
            "one result is not room for the column"
        );
        app.set_results(vec![row(0), row(1), row(2), row(3)]);
        let open = app.layout();
        assert_eq!(open.side_bar_width(), SIDE_BAR_WIDTH);
        assert_eq!(open.width, short.width, "the window did not grow");
        assert!(
            open.list_width() < short.list_width(),
            "the results gave up the room of the column"
        );
        // The column lives in the room the results give up: the window
        // is as tall as the results and not a unit more.
        assert_eq!(
            open.height(),
            Layout::with_rows(4).height(),
            "the column did not make the window taller"
        );
    }

    /// The width is the user's dimension, and the layout carries it.
    ///
    /// The default is the width the window has always had, a wider
    /// window is a wider layout, and a width outside the range the
    /// settings hold is clamped: the layout is asked for nothing it
    /// cannot carry.
    #[test]
    fn the_width_is_the_user_s_dimension_and_the_layout_carries_it() {
        let mut app = App::new();
        app.show();
        app.set_results(vec![row(0), row(1)]);
        assert_eq!(app.layout().width, 600, "the default is today's width");
        app.set_width(820);
        assert_eq!(app.width(), 820);
        assert_eq!(app.layout().width, 820, "the layout carries the width");
        assert_eq!(app.layout().height(), Layout::with_rows(2).height());
        app.set_width(10);
        assert_eq!(app.width(), 400, "the narrowest is the floor");
        app.set_width(100_000);
        assert_eq!(app.width(), 1000, "the widest is the ceiling");
    }

    /// The settings panel holds the keyboard while it is open, and gives
    /// it back when it closes.
    ///
    /// Like the menu: the rows wrap, the panel opens from the command
    /// bar and closes on Escape, and the window it returns to is the
    /// field it came from.
    #[test]
    fn the_settings_panel_holds_the_keyboard_while_it_is_open() {
        let mut app = App::new();
        app.show();
        app.open_settings();
        assert_eq!(app.mode(), Mode::Settings);
        assert_eq!(app.settings_index(), 0, "the first row is selected");
        app.settings_next(3);
        assert_eq!(app.settings_index(), 1);
        app.settings_next(3);
        app.settings_next(3);
        assert_eq!(app.settings_index(), 0, "the rows wrap");
        app.settings_previous(3);
        assert_eq!(app.settings_index(), 2, "the rows wrap backwards");
        app.settings_closed();
        assert_eq!(app.mode(), Mode::Typing, "the field has the keyboard back");
        // Hiding the window takes the panel with it: a window that comes
        // back up comes back to the field.
        app.open_settings();
        app.hide();
        app.show();
        assert_eq!(
            app.mode(),
            Mode::Typing,
            "the panel does not survive its own window"
        );
    }

    /// The settings panel makes its own room.
    ///
    /// The panel is a list the host draws, and a command line has **no
    /// results** — so a window sized from the results had no room for the
    /// panel at all, and the settings were adjusted by keys on a list that
    /// was drawn nowhere. The host says how many rows the panel draws and
    /// the geometry makes room for them, which is the same rule the
    /// preview follows.
    #[test]
    fn the_settings_panel_makes_its_own_room() {
        let mut app = App::new();
        app.show();
        // The field a command is typed into: the rows are empty, and the
        // window is as short as the field and the line under it.
        app.set_results(Vec::new());
        let empty = app.layout();
        assert_eq!(empty.list_height(), 0, "no results, no list");

        app.set_settings_rows(12);
        app.open_settings();
        let panel = app.layout();
        assert_eq!(panel.panel, 12, "the geometry knows the panel's rows");
        assert_eq!(
            panel.list_height(),
            12 * panel.row_height + crate::layout::PANEL_INSET * 2,
            "the panel brings the room it draws in"
        );
        assert!(
            panel.height() > empty.height(),
            "the window grows for the panel: {} against {}",
            panel.height(),
            empty.height()
        );

        // The panel closed is the results' size again.
        app.settings_closed();
        assert_eq!(app.layout().list_height(), 0, "the room goes with it");
    }

    /// The settings panel owns the whole content width, with or without
    /// stale results on the screen.
    ///
    /// The actions column used to be added in `Mode::Settings` too: an
    /// inert legend while the panel had the keyboard. With results left
    /// over from the last answer the column took `SIDE_BAR_WIDTH` away
    /// from the panel, so its label and value columns changed width
    /// depending on whether stale results happened to be there. The
    /// column is gated on the modes where the actions are reachable, and
    /// `paint::model_of` gates it the same way.
    #[test]
    fn the_settings_panel_has_the_same_room_with_or_without_stale_results() {
        let mut app = App::new();
        app.show();
        app.set_settings_rows(12);
        app.open_settings();

        // A command line: `>settings` has no results at all.
        app.set_results(Vec::new());
        let without = app.layout();

        // The same panel with the last answer's rows still on the screen:
        // four rows are enough that the column would have fit before the
        // gate.
        app.set_results(vec![row(0), row(1), row(2), row(3)]);
        let with = app.layout();

        assert_eq!(app.mode(), Mode::Settings);
        assert_eq!(
            without.side_bar_width(),
            0,
            "the panel, not the actions, has the keyboard"
        );
        assert_eq!(
            with.side_bar_width(),
            0,
            "stale results do not put the actions column back"
        );
        assert_eq!(
            with.list_width(),
            without.list_width(),
            "the panel owns the whole content width either way"
        );
        assert_eq!(with.panel, without.panel, "the panel has the same rows");
    }
}
