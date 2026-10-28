// SPDX-License-Identifier: Apache-2.0

//! The launcher's state.
//!
//! What the window is showing, what the user has typed, which result is
//! selected, and what the daemon is being asked. It is the whole of the
//! launcher's behaviour that is not a platform call — and a platform call
//! is a window, a font, a key and a file.
//!
//! The state machine has four shapes, and they are the four things the
//! launcher can be:
//!
//! | mode | what is on the screen |
//! |---|---|
//! | `Hidden` | nothing; the launcher is resident and waiting |
//! | `Typing` | the input field, and the results under it |
//! | `Menu` | the actions of the selected result |
//!
//! `Pending` is not a mode: it is a fact about the answer, and the window
//! shows the previous results while the new ones are on their way. A
//! launcher that blanks the list on every keystroke is a launcher that
//! flickers.
//!
//! The preview is not a mode either: it is a panel **over** the results,
//! and it can be there while the user is still typing. Esc closes it, and
//! then closes the window.

use alloc::string::String;
use alloc::string::ToString;
use alloc::vec::Vec;

use crate::actions::Action;
use crate::commands;
use crate::hotkey::DoubleCtrl;
use crate::layout::Layout;
use crate::query::Query;
use crate::results::{Results, Row};

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
    /// Whether an answer is on its way.
    pending: bool,
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
        App {
            mode: Mode::Hidden,
            query: Query::new(),
            results: Results::new(),
            hotkey: DoubleCtrl::new(),
            session: 0,
            warnings: Vec::new(),
            pending: false,
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
        self.pending = false;
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
        let split = self.query.split();
        let mut q = Query::new();
        q.set_text(split.names.to_string());
        q.engine_text()
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
        self.pending = true;
    }

    /// Delete the last character.
    pub fn backspace(&mut self) {
        self.query.backspace();
        self.pending = true;
    }

    /// Delete the word before the caret.
    pub fn delete_word(&mut self) {
        self.query.delete_word();
        self.pending = true;
    }

    /// Empty the query.
    pub fn clear_query(&mut self) {
        self.query.clear();
        self.results.clear();
        self.pending = false;
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
    pub fn set_visible_rows(&mut self, rows: usize) {
        self.results.set_visible(rows);
    }

    /// The answer came back.
    pub fn set_results(&mut self, rows: Vec<Row>) {
        self.results.set(rows);
        self.pending = false;
    }

    /// The answer carried these warnings.
    pub fn set_warnings(&mut self, warnings: Vec<String>) {
        self.warnings = warnings;
    }

    /// The warnings the last answer carried.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Whether an answer is on its way.
    pub fn is_pending(&self) -> bool {
        self.pending
    }

    /// The geometry, for the rows there are.
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

    #[test]
    fn typing_asks_the_daemon_again() {
        let mut app = App::new();
        app.show();
        app.insert('a');
        assert!(app.is_pending(), "a keystroke means a new answer");
        app.set_results(vec![row(0)]);
        assert!(!app.is_pending());
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
        assert!(l.height() > l.input_height, "the window grew for the results");
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
