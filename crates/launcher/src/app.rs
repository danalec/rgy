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

use alloc::string::String;
use alloc::vec::Vec;

use crate::actions::Action;
use crate::commands;
use crate::hotkey::DoubleCtrl;
use crate::layout::Layout;
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

/// How many rows of the launcher's geometry fit on a screen.
///
/// The window's chrome — the field, the padding and the border — is what
/// is left over, and the rest is rows. The host knows the height of the
/// screen and the model knows the height of a row, and this is where the
/// two meet.
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
}

/// What the preview panel shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Preview {
    /// The name of the file, as the panel's title.
    pub title: String,
    /// The lines of the file.
    pub lines: Vec<String>,
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
    session: u64,
    /// The warnings the last answer carried.
    warnings: Vec<String>,
    /// The panel over the results, when there is one.
    preview: Option<Preview>,
    /// Which action of the menu is selected.
    menu_index: usize,
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
            self.session = self.session.wrapping_add(1);
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
        crate::layer::scope_query(self.query.split().names)
    }

    /// The content pattern, when the query carries the prefix.
    pub fn content_query(&self) -> Option<&str> {
        self.query.split().content
    }

    /// The layer the query is about.
    pub fn layer(&self) -> crate::layer::Layer {
        self.query.split().layer()
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
        let mut layout = Layout::with_rows(rows as u32);
        // The preview panel takes the room the results would have taken:
        // it is a panel **over** them, and a window that grew for it would
        // jump every time the user opened one.
        if let Some(preview) = &self.preview {
            layout.preview = preview.lines.len() as u32 + 1;
        }
        layout
    }

    /// The hotkey's state machine.
    pub fn hotkey(&mut self) -> &mut DoubleCtrl {
        &mut self.hotkey
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

    /// Which action of the menu is selected.
    pub fn menu_index(&self) -> usize {
        self.menu_index
    }

    /// Select the next action of the menu.
    pub fn menu_next(&mut self) {
        let count = Action::menu().len();
        if count == 0 {
            return;
        }
        self.menu_index = (self.menu_index + 1) % count;
    }

    /// Select the previous action of the menu.
    pub fn menu_previous(&mut self) {
        let count = Action::menu().len();
        if count == 0 {
            return;
        }
        self.menu_index = if self.menu_index == 0 {
            count - 1
        } else {
            self.menu_index - 1
        };
    }

    /// The action the menu has selected.
    pub fn menu_action(&self) -> Option<Action> {
        Action::menu().get(self.menu_index).copied()
    }

    /// Close the actions.
    pub fn menu_closed(&mut self) {
        if self.mode == Mode::Menu {
            self.mode = Mode::Typing;
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
    /// the engine sees it: the engine does not know `>` and fails closed
    /// on one.
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
            lines: alloc::vec!["primeira linha".to_string()],
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
            Action::menu().last().copied(),
            "the selection went round"
        );
    }
}
