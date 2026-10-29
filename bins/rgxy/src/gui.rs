// SPDX-License-Identifier: Apache-2.0

//! The window's driver.
//!
//! Three things and the events between them: the **model** (what the
//! launcher is showing), the **source** (where the answers come from) and
//! the **window** (the pixels). The window calls in, the model decides,
//! the source answers, and the window is told what to do — that is the
//! whole loop, and it is a loop because the launcher is resident: the
//! process stays, the window comes and goes.
//!
//! # The two clocks
//!
//! Both layers ask on the window's clock (120 ms): the name layer re-ranks
//! a warm index, and the content layer **reads files**. A burst of typing
//! is one search, and a search that reads two hundred files is not
//! something to do six times a second.
//!
//! The clock is also what keeps the window honest: the question is asked
//! after the window is up, and the line under the field says the launcher
//! is looking. A key handler that waits for the daemon is a window that
//! does not move.

use std::cell::RefCell;
use std::rc::Rc;

use rgy_launcher::usage::History;
use rgy_launcher::{Action, Answer, App, Layer, Mode, Row, Source, commands};
use rgy_launcher_windows::actions;
use rgy_launcher_windows::hook::Hook;
use rgy_launcher_windows::window::{Compositor, Control, Event, Key, View, Window};

use crate::Options;
use crate::daemon::{DaemonSource, Status};
use crate::mru;
use crate::preview;

/// How many results the window asks for.
const LIMIT: usize = 50;

/// The placeholder the field shows when it is empty.
const PLACEHOLDER: &str = "Buscar arquivos, apps ou comandos…";

/// What the launcher is waiting for.
///
/// The window is up while the answer is on its way: the daemon can take
/// seconds to answer, and a launcher that is invisible while it waits is
/// a launcher that looks broken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Waiting {
    /// Nothing: the launcher is not waiting for an answer.
    Idle,
    /// The name layer's answer: the question has not been asked yet, and
    /// the clock asks it.
    Names,
    /// The content layer's answer: the files were chosen and the clock
    /// waited for them.
    Content,
}

/// What the state needs from a source.
///
/// The launcher's own trait is the question and the answer; the window
/// needs two more things: the line under the field is about the last
/// answer, and the session belongs to the window.
///
/// The trait is here and not in the launcher crate because the launcher
/// crate is `no_std` and knows nothing about a window.
trait WindowSource: Source {
    /// What the window shows about the last answer.
    fn status(&self) -> Rc<RefCell<Status>>;

    /// The session the daemon knows this window by.
    fn set_session(&mut self, id: u64);

    /// Ask the daemon who it is, and say what it said.
    ///
    /// The sentences are the ones the window shows under the field: the
    /// daemon's own when it is not there, and what the daemon said when
    /// it is there but slow.
    fn ask_the_daemon(&mut self) -> Vec<String>;
}

impl WindowSource for DaemonSource {
    fn status(&self) -> Rc<RefCell<Status>> {
        DaemonSource::status(self)
    }

    fn set_session(&mut self, id: u64) {
        DaemonSource::set_session(self, id);
    }

    fn ask_the_daemon(&mut self) -> Vec<String> {
        DaemonSource::ask_the_daemon(self)
    }
}

/// What the machine does with a path, and what it says when it could not.
///
/// The answer is a sentence the launcher can show, and the seam is a
/// closure and not a call so that a test can say what the machine did.
type Opener = Box<dyn Fn(&str) -> Result<(), String>>;

/// What the window is showing, and where the answers come from.
struct State {
    /// The model.
    app: App,
    /// The daemon.
    ///
    /// The trait object and not the daemon itself: a test needs a
    /// source that answers nothing and remembers what it was asked, and
    /// a state that cannot hold one is a state whose questions cannot be
    /// checked.
    source: Box<dyn WindowSource>,
    /// Where a paste's text comes from.
    ///
    /// The seam is a field and not a call so that a test can put a text
    /// in it: a test that takes the user's clipboard is a test that
    /// takes the user's work.
    clipboard: Box<dyn Fn() -> actions::Clipboard>,
    /// Opens a path with the platform's own program.
    ///
    /// The seam is a field and not a call for the same reason the
    /// clipboard is: a test that opens a file is a test that opens the
    /// user's files, and the answer the launcher has to react to is
    /// the one the machine gives.
    open: Opener,
    /// Opens the folder that holds a path.
    ///
    /// Same seam, same reason: a test that reveals a folder opens a
    /// window on the user's desktop.
    reveal: Opener,
    /// What the launcher is waiting for.
    waiting: Waiting,
    /// Whether the daemon has to be asked who it is.
    ///
    /// The question is asked on the clock, and the clock only ticks while
    /// the window is up: the window is up before the daemon is asked, and
    /// the answer lands in the window that is already there.
    ask_daemon: bool,
    /// What the user used, and when.
    ///
    /// The launcher's own memory: the daemon ranks by relevance, and what
    /// the user opened yesterday is the one thing it cannot know.
    history: History,
    /// Where the memory is kept.
    usage_path: Option<std::path::PathBuf>,
    /// What the launcher has to say about the machine it is on.
    ///
    /// The notes are here and not in the warnings because an answer
    /// does not make them go away: a refused hotkey is still refused
    /// when the daemon answers.
    notes: Vec<String>,
}

impl State {
    /// What the launcher waits for, given the query.
    ///
    /// An empty field is not a question: the launcher that asks the
    /// daemon to rank the whole index before the user has typed anything
    /// is a launcher that opens slowly for no reason.
    ///
    /// The content layer is named here because it reads files: it waits
    /// for the clock, and the clock is what asks it.
    fn waiting_for(&self) -> Waiting {
        let text = self.app.query().text();
        if text.is_empty() {
            Waiting::Idle
        } else if self.app.layer() == Layer::Content {
            Waiting::Content
        } else {
            Waiting::Names
        }
    }

    /// The user opened a result: the launcher remembers it, and writes it
    /// down so that the next run of the launcher knows it too.
    fn record_use(&mut self, path: &str) {
        self.history.record(path, mru::now());
        // The write is tried and its answer is kept: a memory that
        // cannot be written is not a launcher that stops working — the
        // use is remembered for this session, and the write is tried
        // again on the next use. The user is told anyway: a memory that
        // is silently not written is a memory the user believes in and
        // the machine does not have.
        let written = match &self.usage_path {
            Some(file) => mru::save(file, &self.history),
            None => return,
        };
        if let Err(reason) = written {
            let message = format!("o histórico não foi gravado: {reason}");
            eprintln!("rgxy: {message}");
            self.notes.push(message);
        }
    }
}

/// Run the launcher, until the window is closed.
pub fn run(source: DaemonSource, options: &Options) -> Result<(), String> {
    let mut window = Window::new("rgxy")?;
    // The hook is a global thing, and a global thing can be refused: the
    // launcher still works, it is just opened another way.
    let mut refused = None;
    let _hook = if options.hook {
        match Hook::install(window.hwnd()) {
            Ok(hook) => Some(hook),
            Err(reason) => {
                // The reason is for the window as well as for the stderr:
                // a program started from a shortcut has no stderr, and a
                // launcher that cannot be opened and says nothing is a
                // process the user has to kill.
                let message = format!("o atalho global não foi instalado: {reason}");
                eprintln!("rgxy: {message}");
                refused = Some(message);
                None
            }
        }
    } else {
        None
    };

    // The memory is read once, when the launcher starts: the launcher is
    // resident, and the file is the memory of the last time it ran.
    let usage_path = mru::store_path();
    let (history, memory_note) = memory(usage_path.as_deref());

    // What the launcher has to say about the machine it is on: a
    // refused hotkey and a memory that could not be read are two
    // facts, and the line under the field carries both of them.
    let mut notes: Vec<String> = Vec::new();
    // The window goes up when the launcher was asked to show itself, and
    // also when the hotkey was refused: the hotkey is the only thing that
    // raises the window, and a launcher that cannot be raised is a
    // process the user has to kill. A memory that could not be read does
    // not raise the window: the launcher is still resident, and the line
    // says what happened when the user opens it.
    let mut open_at_startup = options.show;
    if let Some(message) = refused {
        notes.push(message);
        open_at_startup = true;
    }
    if let Some(message) = memory_note {
        notes.push(message);
    }
    // The compositor refused the window's own picture: the window is up
    // and it paints itself, without the translucency, and the line says
    // so. A window that is not what it was is a thing the user has to be
    // told about, and the line under the field is where the launcher says
    // what is true of the machine it is on.
    if window.compositor() == Compositor::Refused {
        notes.push("o ecrã recusou a superfície translúcida".to_string());
    }

    let mut state = State {
        app: App::new(),
        source: Box::new(source),
        clipboard: Box::new(actions::paste_text),
        open: Box::new(actions::open),
        reveal: Box::new(actions::reveal),
        waiting: Waiting::Idle,
        ask_daemon: true,
        history,
        usage_path,
        notes,
    };

    // The rows the screen has room for: the window is sized from them, and
    // a window whose bottom half is under the bottom of the screen is a
    // window the user cannot use.
    state
        .app
        .set_visible_rows(rgy_launcher::app::rows_that_fit(window.work_area_height()));

    // The window goes up when the launcher was asked to show itself, and
    // also when the hotkey was refused: the hotkey is the only thing that
    // raises the window, and a launcher that cannot be raised is a
    // process the user has to kill.
    if open_at_startup {
        state.app.show();
        state.source.set_session(state.app.session());
        // The window goes up **now**: the launcher was asked to show
        // itself, and a window that waits for the first message is a
        // window waiting for a keystroke that cannot reach it. The daemon
        // is asked after, and the answer lands in the window that is
        // already up.
        state.waiting = state.waiting_for();
        let layout = state.app.layout();
        window.set_view(view(&state));
        window.resize(layout.width as i32, layout.height() as i32);
        window.show();
    }

    // The daemon is asked on the clock, and the clock only ticks while
    // the window is up: the window is up before the daemon is asked, and
    // the answer lands in the window that is already there. A daemon that
    // takes the connection and then says nothing does not keep the window
    // off the screen, and it does not keep the hotkey from working
    // either — the launcher that waits for the daemon before it waits for
    // the user is a launcher that is late for both.

    window.pump(|event| handle(&mut state, event))
}

/// The launcher's memory, and what the launcher has to say about it.
///
/// The memory is read once, when the launcher starts: the launcher is
/// resident, and the file is the memory of the last time it ran.
///
/// A file that is **not there** is not an error: a launcher that has
/// never run has no history, and that is the normal state of a new
/// machine. A file that is there and cannot be read is another thing
/// altogether: the ranking falls back to the daemon's order, the nudge
/// does not come back, and the user is told — the memory is gone for
/// this run, and it is not the daemon's fault.
fn memory(path: Option<&std::path::Path>) -> (History, Option<String>) {
    let Some(path) = path else {
        return (History::new(), None);
    };
    match mru::load(path) {
        Ok(history) => (history, None),
        Err(err) => {
            let message = mru::read_error(&err);
            // The stderr of a program started from a shortcut is
            // nowhere: the line under the field says it as well.
            eprintln!("rgxy: {message}");
            (History::new(), Some(message))
        }
    }
}

/// One event, and what the window should do about it.
fn handle(state: &mut State, event: Event) -> Control {
    match event {
        Event::Toggle => {
            state.app.toggle();
            if state.app.is_visible() {
                // A new window is a new session: the previous window's
                // result set is not the answer to a new question.
                state.source.set_session(state.app.session());
                // The window goes up **now**, before the daemon is asked.
                // The daemon can take seconds to answer, and a launcher
                // that is invisible while it waits is a launcher that
                // looks broken: the answer arrives into the window that
                // is already on the screen.
                state.waiting = state.waiting_for();
                // The daemon's own question is asked on the clock, which
                // only ticks while the window is up.
                state.ask_daemon = true;
                show(state)
            } else {
                // The window goes down and nothing is waited for: the
                // clock stops with it, and the answer that was on its way
                // has nowhere to land.
                state.waiting = Waiting::Idle;
                Control::Hide
            }
        }
        Event::Key(key) => on_key(state, key),
        Event::Tick => tick(state),
        Event::Resize => paint(state),
        Event::Quit => Control::Quit,
    }
}

/// A key.
fn on_key(state: &mut State, key: Key) -> Control {
    // The menu has the keyboard while it is open.
    if state.app.mode() == Mode::Menu {
        return menu_key(state, key);
    }

    match key {
        Key::Char(c) => state.app.insert(c),
        Key::Backspace => state.app.backspace(),
        Key::CtrlW => state.app.delete_word(),
        Key::CtrlU => state.app.clear_query(),
        Key::Left => {
            state.app.move_left();
            return paint(state);
        }
        Key::Right => {
            state.app.move_right();
            return paint(state);
        }
        Key::Home => {
            state.app.move_home();
            return paint(state);
        }
        Key::End => {
            state.app.move_end();
            return paint(state);
        }
        Key::Up => {
            state.app.select_previous();
            return paint(state);
        }
        Key::Down => {
            state.app.select_next();
            return paint(state);
        }
        Key::PageUp => {
            state.app.select_page(false);
            return paint(state);
        }
        Key::PageDown => {
            state.app.select_page(true);
            return paint(state);
        }
        Key::Enter => {
            // A command line runs the command; anything else opens the
            // selected result.
            if state.app.is_command() {
                return command(state);
            }
            return run_action(state, Action::Open);
        }
        Key::CtrlEnter => return run_action(state, Action::OpenFolder),
        Key::CtrlC => return run_action(state, Action::CopyPath),
        Key::CtrlV => {
            // The clipboard with nothing on it does not change the
            // query, and the launcher says so instead of asking the
            // daemon a question whose answer it already has.
            let clipboard = (state.clipboard)();
            if !paste(state, clipboard) {
                return paint(state);
            }
        }
        Key::CtrlP => return run_action(state, Action::Preview),
        Key::CtrlK => {
            // The menu is the list of the actions of the selected
            // result, and with no result there is nothing to list. The
            // key says so instead of doing nothing: a key that does
            // nothing and says nothing is a launcher the user cannot
            // tell from a frozen one.
            return run_action(state, Action::Menu);
        }
        Key::Esc => {
            // Escape closes the preview, then the menu, then the window.
            if state.app.close_preview() {
                return paint(state);
            }
            if state.app.mode() == Mode::Menu {
                state.app.menu_closed();
                return paint(state);
            }
            state.app.hide();
            return Control::Hide;
        }
        Key::Delete => {
            // The key deletes the character **at** the caret: `Delete` is
            // the forward deletion and `Backspace` is the backward one,
            // and a `Delete` that took the character before the caret
            // would take the wrong one away from a user who is editing
            // in the middle of the line. The model holds the text and the
            // caret, and the deletion is the model's to make.
            //
            // The round-trip is owed only when the text really changed:
            // a caret at the end of the field has nothing to delete, and
            // the daemon is not asked a question nobody asked.
            if !state.app.delete_forward() {
                return paint(state);
            }
        }
        Key::Tab | Key::Other => {}
    }

    // The query changed: the launcher asks its own question.
    changed(state)
}

/// The query changed: the launcher asks its own question.
///
/// The question is asked on the **clock**: the daemon can take seconds to
/// answer, and a key handler that waits for it is a window that does not
/// move. The line under the field says the launcher is looking, and the
/// answer lands in a window that is already up.
fn changed(state: &mut State) -> Control {
    // A command line is not a search: the engine would look for the
    // literal text ">folder" and answer nothing, while the line is a
    // command — so the launcher takes it before the engine sees it.
    if state.app.is_command() {
        // A command is not a question for the daemon either: the answer
        // the window was waiting for is not this one.
        state.waiting = Waiting::Idle;
        if state.app.command().is_none() {
            state
                .app
                .set_warnings(vec![commands::unknown(state.app.query().text())]);
        } else {
            state.app.set_warnings(Vec::new());
        }
        return paint(state);
    }

    // An empty field is not a question: the list goes empty with it, and
    // nobody is asked.
    if state.app.query().text().is_empty() {
        state.waiting = Waiting::Idle;
        cleared(state);
        return paint(state);
    }

    // The keystroke asks its own question, and the answer the window was
    // waiting for is not it: the waiting stops here.
    state.waiting = state.waiting_for();
    paint(state)
}

/// The field is empty: the list is empty too.
fn cleared(state: &mut State) {
    state.app.set_results(Vec::new());
    state.app.set_warnings(Vec::new());
}

/// The clipboard's text, into the query.
///
/// The seam answers with a **kind** and not with a sentence: the user has
/// to be told which of the two things went wrong in the language the user
/// reads, and the seam does not know that language.
///
/// False when the query did not change.
fn paste(state: &mut State, clipboard: actions::Clipboard) -> bool {
    match clipboard {
        actions::Clipboard::Text(text) if !text.is_empty() => {
            state.app.insert_str(&text);
            true
        }
        actions::Clipboard::Text(_) => {
            // Nothing to paste is an answer the user has to see: the key
            // was pressed and the launcher said nothing, and the silence
            // looks like a launcher that is broken.
            state.app.set_warnings(vec!["nada para colar".to_string()]);
            false
        }
        actions::Clipboard::NotText => {
            state
                .app
                .set_warnings(vec!["a área de transferência não tem texto".to_string()]);
            false
        }
        actions::Clipboard::Busy => {
            state
                .app
                .set_warnings(vec!["a área de transferência está ocupada".to_string()]);
            false
        }
    }
}

/// A key, while the actions menu is open.
fn menu_key(state: &mut State, key: Key) -> Control {
    match key {
        Key::Up => state.app.menu_previous(),
        Key::Down => state.app.menu_next(),
        Key::Enter => {
            let action = state.app.menu_action();
            state.app.menu_closed();
            if let Some(action) = action {
                return run_action(state, action);
            }
        }
        Key::CtrlV => {
            // A paste is the field's, and the menu is the keyboard's:
            // the menu closes and the text lands in the query, with the
            // field's own words for a clipboard that cannot be pasted.
            // A key that does nothing and says nothing is a launcher
            // the user cannot tell from a frozen one.
            let clipboard = (state.clipboard)();
            state.app.menu_closed();
            if paste(state, clipboard) {
                return changed(state);
            }
            return paint(state);
        }
        Key::Esc | Key::CtrlK => state.app.menu_closed(),
        _ => {}
    }
    paint(state)
}

/// The command line, on Enter.
fn command(state: &mut State) -> Control {
    match state.app.command() {
        Some(action) => run_action(state, action),
        None => {
            state
                .app
                .set_warnings(vec![commands::unknown(state.app.query().text())]);
            paint(state)
        }
    }
}

/// Do what the user asked of the selected result.
fn run_action(state: &mut State, action: Action) -> Control {
    let Some(row) = state.app.current().cloned() else {
        // There is nothing to act on. Saying nothing is what makes the
        // user think the launcher is broken, so the line under the field
        // says what happened.
        state
            .app
            .set_warnings(vec!["nenhum resultado selecionado".to_string()]);
        return paint(state);
    };
    match action {
        Action::Open => {
            // The answer first: the use is recorded after the open, not
            // before it. The history is about what the user did, and a
            // result that could not be opened is not a result the user
            // used.
            let opened = (state.open)(&row.path);
            match opened {
                Ok(()) => {
                    state.record_use(&row.path);
                    state.app.hide();
                    Control::Hide
                }
                Err(reason) => {
                    // The window stays: the user asked for something and
                    // the machine could not do it. A launcher that closes
                    // and says nothing is a launcher that swallowed the
                    // failure.
                    state.app.set_warnings(vec![reason]);
                    paint(state)
                }
            }
        }
        Action::OpenFolder => {
            // The folder that holds the result is not the result: opening
            // it is not using it, and the history says what the user used.
            let revealed = (state.reveal)(&row.path);
            match revealed {
                Ok(()) => {
                    state.app.hide();
                    Control::Hide
                }
                Err(reason) => {
                    state.app.set_warnings(vec![reason]);
                    paint(state)
                }
            }
        }
        Action::CopyPath => {
            // The window stays: the user is copying a path, not leaving.
            let message = match actions::copy_path(&row.path) {
                Ok(()) => "caminho copiado".to_string(),
                Err(reason) => reason,
            };
            state.app.set_warnings(vec![message]);
            paint(state)
        }
        Action::Preview => {
            state.app.set_preview(preview::of(&row.path));
            paint(state)
        }
        Action::Menu => {
            // The result is there: `run_action` has already said so when
            // it is not, and the menu is the list of this result's
            // actions.
            state.app.open_menu();
            paint(state)
        }
    }
}

/// Ask for the answers, now.
fn ask(state: &mut State) {
    let text = state.app.query().text().to_string();
    if text.is_empty() {
        // An empty box is not a question. The launcher that opens in a
        // blink does not ask the daemon to rank the whole index before
        // the user has typed anything.
        cleared(state);
        return;
    }
    if state.app.layer() == Layer::Content {
        // The content layer reads files: it waits for the clock.
        state.waiting = Waiting::Content;
        return;
    }
    let answer = state.source.search(&text, LIMIT);
    apply(state, answer);
}

/// The clock ticked: the turn of whatever the launcher was waiting for.
///
/// The window is already on the screen when the daemon is asked — the
/// clock only ticks while the window is up — and the answer lands in
/// that window.
fn tick(state: &mut State) -> Control {
    match state.waiting {
        Waiting::Idle => {}
        Waiting::Names => {
            // The window is up; now the daemon is asked.
            state.waiting = Waiting::Idle;
            ask(state);
            return paint(state);
        }
        Waiting::Content => {
            state.waiting = Waiting::Idle;
            let text = state.app.query().text().to_string();
            let answer = state.source.search(&text, LIMIT);
            apply(state, answer);
            return paint(state);
        }
    }
    // The daemon's own question — whether it is there, and whether it has
    // a pack store — is asked on the clock too, and it waits for a tick
    // with nothing else to do: the question the user typed comes first.
    //
    // The question is asked in the launcher's own window — two seconds,
    // and not the client's thirty — because the launcher lives on the
    // thread that draws the window.
    if state.ask_daemon {
        state.ask_daemon = false;
        let messages = state.source.ask_the_daemon();
        for message in &messages {
            eprintln!("rgxy: {message}");
        }
        if !messages.is_empty() {
            // Whatever the user has to be told, the window tells it: the
            // stderr of a program started from a shortcut is nowhere.
            state.app.set_warnings(messages);
        }
        return paint(state);
    }
    Control::None
}

/// The answer, on the screen.
fn apply(state: &mut State, answer: Answer) {
    // The daemon's order is the relevance order, and it rules. The
    // history is a nudge on top of it: a file the user opened recently
    // comes up a few places, and no further.
    let answer = state.history.reorder(answer, mru::now());
    let rows: Vec<Row> = answer
        .paths
        .iter()
        .enumerate()
        .map(|(i, path)| {
            let mut row = Row::new(path.clone(), 0);
            row.span = answer.span(i).to_vec();
            // Whether the result is a folder: the daemon says, and the
            // icon beside the name is the icon of the thing.
            row.is_folder = answer.folder(i);
            let context = answer.context(i);
            if !context.is_empty() {
                row = row.with_context(context.to_string());
            }
            row
        })
        .collect();
    state.app.set_results(rows);
    let warnings = state.source.status().borrow().warnings.clone();
    state.app.set_warnings(warnings);
}

/// The window's view of the model.
fn view(state: &State) -> View {
    let app = &state.app;
    let (top, count) = app.results().window();
    let selected = app.results().selected().unwrap_or(0);
    let rows: Vec<Row> = app
        .results()
        .items()
        .iter()
        .skip(top)
        .take(count)
        .cloned()
        .collect();
    View {
        query: app.query().text().to_string(),
        // The caret goes where the model says it is, and not after the
        // last character: the model moves it with the arrow keys, and a
        // paste lands where it is.
        caret: app.query().caret(),
        placeholder: PLACEHOLDER.to_string(),
        rows,
        selected: selected.saturating_sub(top),
        top,
        mode: app.mode(),
        menu: Action::menu().to_vec(),
        menu_selected: app.menu_index(),
        warnings: app.warnings().to_vec(),
        status: status_line(state),
        preview: app.preview().cloned(),
    }
}

/// Repaint, at the size the model asks for.
fn paint(state: &State) -> Control {
    let layout = state.app.layout();
    Control::Paint(view(state), layout.width as i32, layout.height() as i32)
}

/// The window, up, at the size the model asks for.
///
/// The first time the window appears it has to be **shown**: the window is
/// born hidden, and a window that is only painted is a window nobody sees.
/// The clock starts with it, and the clock is what the content layer waits
/// for — so a window that is never shown is also a content layer that never
/// runs.
fn show(state: &State) -> Control {
    let layout = state.app.layout();
    Control::Show(view(state), layout.width as i32, layout.height() as i32)
}

/// The one line under the field.
fn status_line(state: &State) -> String {
    let status = state.source.status();
    let status = status.borrow();
    let mut parts: Vec<String> = Vec::new();
    if state.waiting != Waiting::Idle {
        // The answer is on its way, and the line says so: a launcher that
        // is quiet while it waits is a launcher that looks stuck.
        parts.push("procurando…".to_string());
    }
    if status.asked && !status.connected {
        // The daemon was asked and it is not there. The line does not
        // say that about a daemon nobody has asked about yet.
        parts.push("sem ligação ao daemon".to_string());
    }
    // The count, the layer, the backend and the time belong to the
    // **answer**: they are shown whether the daemon is up or not. The
    // `rgx` fallback answers while the daemon is away, and a line that
    // hides the answer because the daemon is not there hides the one
    // thing the user asked for.
    parts.push(format!("{} resultados", status.matches));
    if !status.layer.is_empty() {
        parts.push(status.layer.to_string());
    }
    if status.layer == "conteúdo" {
        if !status.backend.is_empty() {
            parts.push(status.backend.clone());
        }
        if status.tv {
            parts.push("TV".to_string());
        }
    }
    if status.took_us > 0 {
        if status.took_us < 1000 {
            parts.push("<1 ms".to_string());
        } else {
            parts.push(format!("{} ms", status.took_us / 1000));
        }
    }
    for note in &state.notes {
        // The note is about the machine: it is true while the launcher
        // is the way it is, and an answer does not change it.
        parts.push(note.clone());
    }
    for warning in state.app.warnings() {
        parts.push(warning.clone());
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of the test's own.
    fn scratch(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("rgy-rgxy-{name}-{}", std::process::id()))
    }

    /// A state with no daemon and no results.
    fn empty() -> State {
        let mut source = DaemonSource::new(
            rgy_proto::endpoint::Endpoint::Socket(std::path::PathBuf::from(
                "/tmp/rgy-rgxy-nope.sock",
            )),
            None,
        );
        let _ = source.search("invoice", LIMIT);
        State {
            app: App::new(),
            source: Box::new(source),
            clipboard: Box::new(|| actions::Clipboard::Text(String::new())),
            open: Box::new(|_| Err("um teste não abre ficheiros".to_string())),
            reveal: Box::new(|_| Err("um teste não abre pastas".to_string())),
            waiting: Waiting::Idle,
            ask_daemon: false,
            history: History::new(),
            usage_path: None,
            notes: Vec::new(),
        }
    }

    /// A source that remembers the questions it was asked.
    ///
    /// The daemon is not there in a test, and a question that is never
    /// asked is a question that cannot break: the double answers
    /// nothing, and the test says what the launcher asked.
    struct Recording {
        asked: Rc<RefCell<Vec<String>>>,
        status: Rc<RefCell<Status>>,
    }

    impl Recording {
        fn new(asked: Rc<RefCell<Vec<String>>>) -> Recording {
            Recording {
                asked,
                status: Rc::new(RefCell::new(Status::default())),
            }
        }
    }

    impl Source for Recording {
        fn search(&mut self, query: &str, _limit: usize) -> Answer {
            self.asked.borrow_mut().push(query.to_string());
            Answer::empty()
        }
    }

    impl WindowSource for Recording {
        fn status(&self) -> Rc<RefCell<Status>> {
            Rc::clone(&self.status)
        }

        fn set_session(&mut self, _id: u64) {}

        fn ask_the_daemon(&mut self) -> Vec<String> {
            // A test has no daemon: the question is asked and nothing
            // answers it. The status knows that it was asked, so a test
            // can see when the launcher spoke to it.
            self.status.borrow_mut().asked = true;
            Vec::new()
        }
    }

    /// A state whose source is a double, and the questions it was asked.
    fn recording() -> (State, Rc<RefCell<Vec<String>>>) {
        let asked = Rc::new(RefCell::new(Vec::new()));
        let state = State {
            app: App::new(),
            source: Box::new(Recording::new(Rc::clone(&asked))),
            clipboard: Box::new(|| actions::Clipboard::Text(String::new())),
            open: Box::new(|_| Err("um teste não abre ficheiros".to_string())),
            reveal: Box::new(|_| Err("um teste não abre pastas".to_string())),
            waiting: Waiting::Idle,
            ask_daemon: false,
            history: History::new(),
            usage_path: None,
            notes: Vec::new(),
        };
        (state, asked)
    }

    /// A state whose source is a double, and whose clipboard holds `text`.
    fn recording_with(text: &str) -> (State, Rc<RefCell<Vec<String>>>) {
        let (mut state, asked) = recording();
        let text = text.to_string();
        state.clipboard = Box::new(move || actions::Clipboard::Text(text.clone()));
        (state, asked)
    }

    #[test]
    fn the_window_is_shown_when_the_launcher_opens() {
        // The window is born hidden, and nothing used to show it: the
        // launcher was invisible and the clock the content layer waits for
        // never started — so the whole content layer was code that could
        // not run.
        let mut state = empty();
        let control = handle(&mut state, Event::Toggle);
        assert!(
            matches!(control, Control::Show(..)),
            "the window goes up: {control:?}"
        );
    }

    #[test]
    fn the_window_is_hidden_when_the_launcher_closes() {
        let mut state = empty();
        let _ = handle(&mut state, Event::Toggle);
        let control = handle(&mut state, Event::Toggle);
        assert!(
            matches!(control, Control::Hide),
            "the window goes down: {control:?}"
        );
    }

    #[test]
    fn the_window_goes_up_before_the_daemon_is_asked() {
        // The launcher that asks the daemon before it shows itself is a
        // launcher that looks broken: the daemon can take seconds to
        // answer, and the window is not there while it does.
        let mut state = empty();
        state.app.insert('i');
        let control = handle(&mut state, Event::Toggle);
        assert!(
            matches!(control, Control::Show(..)),
            "the window goes up: {control:?}"
        );
        // And the question is still to be asked: the daemon has not been
        // asked yet, and the answer is on its way to the window that is
        // already up.
        assert_eq!(state.waiting, Waiting::Names, "the answer is on its way");
    }

    #[test]
    fn the_answer_arrives_into_the_window_that_is_already_up() {
        // The clock is what the answer waits for: the window is up, and
        // the answer lands in it.
        let mut state = empty();
        state.app.insert('i');
        let _ = handle(&mut state, Event::Toggle);
        let control = handle(&mut state, Event::Tick);
        assert!(
            matches!(control, Control::Paint(..)),
            "the answer is painted: {control:?}"
        );
        assert_eq!(state.waiting, Waiting::Idle, "the answer came");
    }

    #[test]
    fn an_empty_field_asks_nothing() {
        // An empty box is not a question: the launcher that opens in a
        // blink does not ask the daemon to rank the whole index before
        // the user has typed anything.
        let mut state = empty();
        let control = handle(&mut state, Event::Toggle);
        assert!(matches!(control, Control::Show(..)), "{control:?}");
        assert_eq!(state.waiting, Waiting::Idle, "there is no question");
    }

    #[test]
    fn the_line_says_the_launcher_is_looking() {
        // A launcher that is quiet while it waits is a launcher that
        // looks stuck.
        let mut state = empty();
        state.app.insert('i');
        let _ = handle(&mut state, Event::Toggle);
        let line = status_line(&state);
        assert!(line.contains("procurando"), "{line}");
    }

    #[test]
    fn the_status_line_says_the_daemon_is_not_there() {
        // The source without a daemon: the line says so, and says nothing
        // else that could be mistaken for an answer.
        let state = empty();
        let line = status_line(&state);
        assert!(line.contains("sem ligação"), "{line}");
    }

    #[test]
    fn a_command_with_nothing_to_act_on_says_so() {
        // `>copy` in an empty window used to do nothing at all, and the
        // silence looked like a broken launcher.
        let mut state = empty();
        let _ = run_action(&mut state, Action::CopyPath);
        let line = status_line(&state);
        assert!(line.contains("nenhum resultado"), "{line}");
    }

    /// The paste lands in the query.
    ///
    /// A search box the user cannot paste into is a search box that makes
    /// them type a path by hand, and the path is the one thing everybody
    /// has.
    #[test]
    fn a_paste_is_a_query() {
        let mut state = empty();
        state.app.insert('i');
        state.app.insert('n');
        assert!(paste(
            &mut state,
            actions::Clipboard::Text("voice".to_string())
        ));
        assert_eq!(state.app.query().text(), "invoice");
    }

    /// A paste is a question, and the question is asked.
    ///
    /// The test drives the **key** and not the paste: a test that calls
    /// the paste itself cannot say whether the key asks the daemon, and
    /// asking the daemon is the whole of the change.
    #[test]
    fn a_paste_asks_the_daemon() {
        let (mut state, asked) = recording_with("invoice");
        let _ = on_key(&mut state, Key::CtrlV);
        assert_eq!(state.app.query().text(), "invoice", "the text landed");
        // The question is asked on the clock: the window is up, and the
        // answer is on its way.
        assert_eq!(state.waiting, Waiting::Names, "the answer is on its way");
        assert!(asked.borrow().is_empty(), "the daemon is asked later");
        let _ = handle(&mut state, Event::Tick);
        assert_eq!(asked.borrow().len(), 1, "the daemon was asked once");
        assert_eq!(asked.borrow()[0], "invoice", "the daemon was asked");
    }

    /// A keystroke does not wait for the daemon.
    ///
    /// The daemon can take seconds to answer, and a key handler that
    /// waits for it is a window that does not move: the question is
    /// asked on the clock, and the line says the launcher is looking.
    #[test]
    fn a_keystroke_does_not_block_on_the_daemon() {
        let (mut state, asked) = recording();
        let _ = on_key(&mut state, Key::Char('i'));
        assert!(asked.borrow().is_empty(), "the daemon is asked later");
        assert_eq!(state.waiting, Waiting::Names, "the answer is on its way");
        let line = status_line(&state);
        assert!(line.contains("procurando"), "{line}");
        let _ = handle(&mut state, Event::Tick);
        assert_eq!(asked.borrow().len(), 1, "the daemon was asked once");
        assert_eq!(asked.borrow()[0], "i", "the daemon was asked");
    }

    /// The caret is where the model says it is.
    ///
    /// The window used to draw the caret after the last character,
    /// always: a caret that is always at the end is a decoration, and
    /// the model moves it with the arrow keys.
    #[test]
    fn the_caret_is_where_the_model_says() {
        let mut state = empty();
        state.app.insert('a');
        state.app.insert('b');
        state.app.insert('c');
        state.app.move_left();
        let shown = view(&state);
        assert_eq!(shown.query, "abc");
        assert_eq!(shown.caret, 2, "the caret is before the last character");
        state.app.move_home();
        assert_eq!(view(&state).caret, 0, "the caret is at the start");
    }

    /// The `Delete` key deletes the character at the caret.
    ///
    /// `Delete` is the **forward** deletion: the character under the
    /// caret goes. The key used to fall into the arm that does nothing:
    /// it deleted no character, it said nothing, and it still cost a
    /// round-trip to the daemon.
    #[test]
    fn the_delete_key_deletes_the_character_at_the_caret() {
        let mut state = empty();
        for ch in "abc".chars() {
            state.app.insert(ch);
        }
        state.app.move_home();
        state.app.move_right();
        assert_eq!(state.app.query().caret(), 1, "the caret is in the middle");
        let _ = on_key(&mut state, Key::Delete);
        assert_eq!(
            state.app.query().text(),
            "ac",
            "the character under the caret went"
        );
        // The caret stays where it was: the character that came after it
        // took its place, and the caret is still between the two.
        assert_eq!(
            state.app.query().caret(),
            1,
            "the caret is where the deletion happened"
        );
    }

    /// `Delete` and `Backspace` are two keys and two deletions.
    ///
    /// `Delete` takes the character under the caret and `Backspace` the
    /// one before it: a launcher that made the two the same would take
    /// the wrong character away from a user who is editing in the middle
    /// of the line.
    #[test]
    fn delete_and_backspace_are_two_deletions() {
        let mut state = empty();
        for ch in "abc".chars() {
            state.app.insert(ch);
        }
        state.app.move_home();
        state.app.move_right();
        let _ = on_key(&mut state, Key::Delete);
        assert_eq!(
            state.app.query().text(),
            "ac",
            "the character under the caret went"
        );
        // And now the one before it: the caret is where it was, and the
        // character in front of it is the one that goes.
        let _ = on_key(&mut state, Key::Backspace);
        assert_eq!(
            state.app.query().text(),
            "c",
            "the character before the caret went"
        );
        assert_eq!(state.app.query().caret(), 0, "the caret walked back");
    }

    /// A `Delete` that deletes nothing costs no round-trip.
    ///
    /// The caret at the end of the field has nothing to delete, and the
    /// daemon is not asked a question nobody asked.
    #[test]
    fn a_delete_that_deletes_nothing_costs_no_round_trip() {
        let (mut state, asked) = recording();
        state.app.insert('a');
        let _ = on_key(&mut state, Key::Delete);
        assert_eq!(state.app.query().text(), "a", "nothing was deleted");
        assert_eq!(state.waiting, Waiting::Idle, "no question was armed");
        let _ = handle(&mut state, Event::Tick);
        assert!(asked.borrow().is_empty(), "the daemon was not asked");
    }

    /// A memory that could not be read is said out loud.
    ///
    /// The history is the launcher's own memory: a file that cannot be
    /// read leaves the ranking on the daemon's order and the nudge
    /// gone, and a launcher that says nothing about it is a launcher
    /// whose memory the user believes in and the machine does not have.
    #[test]
    fn a_memory_that_cannot_be_read_is_said_out_loud() {
        let dir = scratch("gui-memory");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the folder is there");
        // The store's own path, taken by a directory: the read fails,
        // and it is not "there is no history" — it is a history that
        // could not be read.
        let path = dir.join("usage.tsv");
        std::fs::create_dir_all(&path).expect("the store's path is taken");

        let (history, note) = memory(Some(&path));
        assert!(history.is_empty(), "there is no history to show");
        let note = note.expect("the memory that could not be read is said");
        assert!(note.contains("histórico"), "{note}");

        // And the line under the field carries it.
        let mut state = empty();
        state.notes = vec![note];
        let line = status_line(&state);
        assert!(line.contains("histórico"), "{line}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A memory that is not there is not an error.
    ///
    /// A launcher that has never run has no history, and that is the
    /// normal state of a new machine: there is nothing to say about
    /// it.
    #[test]
    fn a_memory_that_is_not_there_is_not_an_error() {
        let dir = scratch("gui-memory-absent");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the folder is there");
        let (history, note) = memory(Some(&dir.join("usage.tsv")));
        assert!(history.is_empty(), "there is no history yet");
        assert!(note.is_none(), "there is nothing to say: {note:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The note about the machine is in the line.
    ///
    /// A launcher that says nothing about a refused hotkey is a
    /// launcher the user cannot use: the stderr of a program started
    /// from a shortcut is nowhere.
    #[test]
    fn the_note_about_the_machine_is_in_the_line() {
        let mut state = empty();
        state.notes = vec!["o atalho global não foi instalado: sem permissão".to_string()];
        let line = status_line(&state);
        assert!(line.contains("atalho global"), "{line}");
    }

    /// The clipboard speaks the user's language.
    ///
    /// The rest of the interface is Portuguese with the accents it is
    /// written with: a message without them is a message from somewhere
    /// else.
    #[test]
    fn the_clipboard_speaks_the_users_language() {
        let mut state = empty();
        assert!(!paste(&mut state, actions::Clipboard::NotText));
        assert_eq!(
            state.app.warnings().first().map(String::as_str),
            Some("a área de transferência não tem texto")
        );
        assert!(!paste(&mut state, actions::Clipboard::Busy));
        assert_eq!(
            state.app.warnings().first().map(String::as_str),
            Some("a área de transferência está ocupada")
        );
    }

    /// A clipboard with nothing to paste says so.
    ///
    /// The key was pressed and the launcher said nothing, and the silence
    /// looks like a launcher that is broken.
    #[test]
    fn an_empty_clipboard_says_so() {
        let mut state = empty();
        assert!(!paste(&mut state, actions::Clipboard::Text(String::new())));
        let line = status_line(&state);
        assert!(line.contains("nada para colar"), "{line}");
    }

    #[test]
    fn the_line_is_quiet_when_there_is_something_to_act_on() {
        // The message is for the empty window: with a result selected, the
        // action runs and the line says nothing about a missing result.
        // The preview is the action here because it touches nothing but
        // the file — a test has no business taking the clipboard.
        let mut state = empty();
        state
            .app
            .set_results(vec![Row::new("C:/a/notas.txt".to_string(), 0)]);
        let _ = run_action(&mut state, Action::Preview);
        let line = status_line(&state);
        assert!(!line.contains("nenhum resultado"), "{line}");
    }

    /// A result that was opened is a result the user used.
    ///
    /// The history is about what the user did, and the use is recorded
    /// when the machine answered that the file was opened.
    #[test]
    fn a_result_that_was_opened_is_used() {
        let mut state = empty();
        state
            .app
            .set_results(vec![Row::new("C:/a/notas.txt".to_string(), 0)]);
        state.open = Box::new(|_| Ok(()));
        let _ = run_action(&mut state, Action::Open);
        assert!(
            state.history.get("C:/a/notas.txt").is_some(),
            "the use is recorded: {:?}",
            state.history.entries()
        );
    }

    /// A result that could not be opened is not a result the user used.
    ///
    /// The use used to be recorded **before** the open, so a file the
    /// machine refused to open counted as used, and the ranking of the
    /// next run followed a file nobody ever saw.
    #[test]
    fn a_result_that_could_not_be_opened_is_not_used() {
        let mut state = empty();
        state
            .app
            .set_results(vec![Row::new("C:/a/notas.txt".to_string(), 0)]);
        state.open = Box::new(|_| Err("não pôde ser aberto".to_string()));
        let _ = run_action(&mut state, Action::Open);
        assert!(
            state.history.is_empty(),
            "a failed open is not a use: {:?}",
            state.history.entries()
        );
    }

    /// The folder that holds a result is not a use of it.
    ///
    /// `>folder` opens the folder the result sits in, and the user did
    /// not open the result: the history says what the user used.
    #[test]
    fn the_folder_of_a_result_is_not_a_use_of_it() {
        let mut state = empty();
        state
            .app
            .set_results(vec![Row::new("C:/a/notas.txt".to_string(), 0)]);
        state.reveal = Box::new(|_| Ok(()));
        let _ = run_action(&mut state, Action::OpenFolder);
        assert!(
            state.history.is_empty(),
            "opening the folder is not using the file: {:?}",
            state.history.entries()
        );
    }

    /// The menu key with nothing to act on says so.
    ///
    /// The key used to open nothing and say nothing, and the user could
    /// not tell the launcher from a frozen one.
    #[test]
    fn the_menu_key_with_nothing_to_act_on_says_so() {
        let mut state = empty();
        let _ = on_key(&mut state, Key::CtrlK);
        let line = status_line(&state);
        assert!(line.contains("nenhum resultado"), "{line}");
        assert_ne!(state.app.mode(), Mode::Menu, "there is no menu to open");
    }

    /// The daemon is asked on the clock, and not before the window.
    ///
    /// The window goes up before the daemon is asked: a daemon that takes
    /// the connection and then says nothing must not keep the window off
    /// the screen, and it must not keep the hotkey from working either.
    #[test]
    fn the_daemon_is_asked_on_the_clock_and_not_before_the_window() {
        let (mut state, _) = recording();
        // The launcher starts: the daemon has to be asked who it is, and
        // the question is not asked yet.
        state.ask_daemon = true;
        assert!(
            !state.source.status().borrow().asked,
            "the daemon was not asked yet"
        );

        // The window goes up.
        let _ = handle(&mut state, Event::Toggle);
        assert!(
            !state.source.status().borrow().asked,
            "the window goes up before the daemon is asked"
        );

        // And the clock asks.
        let _ = handle(&mut state, Event::Tick);
        assert!(
            state.source.status().borrow().asked,
            "the clock asks the daemon, and the clock only ticks while the
            window is up"
        );
    }

    /// The line shows the answer, even when the daemon is not there.
    ///
    /// The count, the layer, the backend and the time belong to the
    /// **answer**: the `rgx` fallback answers while the daemon is away,
    /// and a line that says "sem daemon" and hides the answer hides the
    /// one thing the user asked for.
    #[test]
    fn the_line_shows_the_answer_even_when_the_daemon_is_not_there() {
        let (state, _) = recording();
        {
            let status = state.source.status();
            let mut st = status.borrow_mut();
            st.connected = false;
            st.matches = 3;
            st.layer = "conteúdo";
            st.backend = "rgx".to_string();
            st.took_us = 1500;
        }
        let line = status_line(&state);
        assert!(line.contains("3 resultados"), "{line}");
        assert!(line.contains("conteúdo"), "{line}");
        assert!(line.contains("rgx"), "{line}");
        assert!(line.contains("1 ms"), "{line}");
    }

    /// The line does not say the daemon is not there before it was asked.
    ///
    /// The daemon was not asked yet: the line that says "sem ligação ao
    /// daemon" about a daemon nobody has spoken to is a line that says
    /// something it does not know.
    #[test]
    fn the_line_does_not_say_the_daemon_is_not_there_before_asking() {
        let (state, _) = recording();
        let line = status_line(&state);
        assert!(!line.contains("sem ligação ao daemon"), "{line}");
    }

    /// A paste inside the menu is not a silent no-op.
    ///
    /// The menu had the keyboard and swallowed the key: the user pressed
    /// Ctrl+V and the launcher said nothing, and the silence looks like
    /// a launcher that is broken. A paste is the field's, so the menu
    /// closes and the text lands in the query.
    #[test]
    fn a_paste_inside_the_menu_is_not_a_silent_no_op() {
        let (mut state, asked) = recording_with("invoice");
        state
            .app
            .set_results(vec![Row::new("C:/a/notas.txt".to_string(), 0)]);
        let _ = on_key(&mut state, Key::CtrlK);
        assert_eq!(state.app.mode(), Mode::Menu, "the menu is open");

        let _ = on_key(&mut state, Key::CtrlV);

        assert_eq!(state.app.query().text(), "invoice", "the text landed");
        assert_ne!(state.app.mode(), Mode::Menu, "the menu is closed");
        assert_eq!(state.waiting, Waiting::Names, "the answer is on its way");
        assert!(asked.borrow().is_empty(), "the daemon is asked later");
    }

    /// The history that could not be written is said out loud.
    ///
    /// The use is remembered for this session and the write is tried
    /// again on the next use, but the user is told: a memory that is
    /// silently not written is a memory the user believes in and the
    /// machine does not have. The note reaches the line under the field,
    /// the same way the note about the hotkey does.
    #[test]
    fn the_history_that_could_not_be_written_is_said_out_loud() {
        let mut state = empty();
        // The parent of the store is a **file**: the write cannot be
        // made, and the launcher has to say so.
        let blocker = std::env::temp_dir().join(format!("rgy-rgxy-history-{}", std::process::id()));
        std::fs::write(&blocker, b"this is a file, not a folder").unwrap();
        state.usage_path = Some(blocker.join("history.json"));
        state
            .app
            .set_results(vec![Row::new("C:/a/notas.txt".to_string(), 0)]);
        state.open = Box::new(|_| Ok(()));
        let _ = run_action(&mut state, Action::Open);
        let line = status_line(&state);
        assert!(line.contains("histórico"), "{line}");
        let _ = std::fs::remove_file(&blocker);
    }

    /// The menu key with a result opens the actions of that result.
    ///
    /// The key is not only the "nothing to say" path: with a result
    /// selected it has to open the menu, as it always did.
    #[test]
    fn the_menu_key_with_a_result_opens_the_menu() {
        let mut state = empty();
        state
            .app
            .set_results(vec![Row::new("C:/a/notas.txt".to_string(), 0)]);
        let _ = on_key(&mut state, Key::CtrlK);
        assert_eq!(state.app.mode(), Mode::Menu, "the menu is open");
    }
}
