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
//! The name layer asks the daemon on every keystroke: the answer is a
//! re-rank of a warm index, and it is fast enough to feel like typing.
//!
//! The content layer **reads files**, so it waits for the window's clock
//! (120 ms) before it asks. A burst of typing is one search, and a search
//! that reads two hundred files is not something to do six times a second.

use rgy_launcher::usage::History;
use rgy_launcher::{Action, Answer, App, Mode, Row, Source, Split, commands};
use rgy_launcher_windows::actions;
use rgy_launcher_windows::hook::Hook;
use rgy_launcher_windows::window::{Control, Event, Key, View, Window};

use crate::Options;
use crate::daemon::DaemonSource;
use crate::mru;
use crate::preview;

/// How many results the window asks for.
const LIMIT: usize = 50;

/// The placeholder the field shows when it is empty.
const PLACEHOLDER: &str = "Buscar arquivos, apps ou comandos…";

/// What the window is showing, and where the answers come from.
struct State {
    /// The model.
    app: App,
    /// The daemon.
    source: DaemonSource,
    /// Whether the content layer is waiting for the clock.
    content_dirty: bool,
    /// What the user used, and when.
    ///
    /// The launcher's own memory: the daemon ranks by relevance, and what
    /// the user opened yesterday is the one thing it cannot know.
    history: History,
    /// Where the memory is kept.
    usage_path: Option<std::path::PathBuf>,
}

impl State {
    /// The user opened a result: the launcher remembers it, and writes it
    /// down so that the next run of the launcher knows it too.
    fn record_use(&mut self, path: &str) {
        self.history.record(path, mru::now());
        if let Some(file) = &self.usage_path {
            // A memory that cannot be written is not a launcher that
            // stops working: the use is remembered for this session, and
            // the file is written the next time.
            let _ = mru::save(file, &self.history);
        }
    }
}

/// Run the launcher, until the window is closed.
pub fn run(source: DaemonSource, options: &Options) -> Result<(), String> {
    let mut window = Window::new("rgxy")?;
    // The hook is a global thing, and a global thing can be refused: the
    // launcher still works, it is just opened another way.
    let _hook = if options.hook {
        match Hook::install(window.hwnd()) {
            Ok(hook) => Some(hook),
            Err(reason) => {
                eprintln!("rgxy: o atalho global não foi instalado: {reason}");
                None
            }
        }
    } else {
        None
    };

    // The memory is read once, when the launcher starts: the launcher is
    // resident, and the file is the memory of the last time it ran.
    let usage_path = mru::store_path();
    let history = match &usage_path {
        Some(path) => mru::load(path),
        None => History::new(),
    };
    let mut state = State {
        app: App::new(),
        source,
        content_dirty: false,
        history,
        usage_path,
    };

    // The daemon is asked who it is before anything else: the answer says
    // whether there is a pack store, and the answer says whether there is
    // a daemon at all.
    if !state.source.connect() {
        let where_ = state.source.endpoint().describe();
        if state.source.rgx().is_some() {
            eprintln!("rgxy: sem daemon em {where_}; a camada de conteúdo usará o rgx");
        } else {
            eprintln!("rgxy: sem daemon em {where_}; a camada de conteúdo não tem resposta");
        }
    }

    // The rows the screen has room for: the window is sized from them, and
    // a window whose bottom half is under the bottom of the screen is a
    // window the user cannot use.
    state
        .app
        .set_visible_rows(rgy_launcher::app::rows_that_fit(window.work_area_height()));

    if options.show {
        state.app.show();
        state.source.set_session(state.app.session());
        ask(&mut state);
        // The window goes up **now**: the launcher was asked to show
        // itself, and a window that waits for the first message is a
        // window waiting for a keystroke that cannot reach it.
        let layout = state.app.layout();
        window.set_view(view(&state));
        window.resize(layout.width as i32, layout.height() as i32);
        window.show();
    }

    window.pump(|event| handle(&mut state, event))
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
                ask(state);
                show(state)
            } else {
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
        Key::CtrlP => return run_action(state, Action::Preview),
        Key::CtrlK => {
            state.app.open_menu();
            return paint(state);
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
        Key::Tab | Key::Delete | Key::Other => {}
    }

    // The query changed. A command line is not a search, and the engine
    // does not know `>` — it fails closed on one, so the launcher takes it
    // before the engine sees it.
    if state.app.is_command() {
        if state.app.command().is_none() {
            state
                .app
                .set_warnings(vec![commands::unknown(state.app.query().text())]);
        } else {
            state.app.set_warnings(Vec::new());
        }
        return paint(state);
    }

    ask(state);
    paint(state)
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
            state.record_use(&row.path);
            match actions::open(&row.path) {
                Ok(()) => {
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
            state.record_use(&row.path);
            match actions::reveal(&row.path) {
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
        state.app.set_results(Vec::new());
        state.app.set_warnings(Vec::new());
        return;
    }
    let split = Split::of(&text);
    if split.is_content() {
        // The content layer reads files: it waits for the clock.
        state.content_dirty = true;
        return;
    }
    let answer = state.source.search(&text, LIMIT);
    apply(state, answer);
}

/// The clock ticked: the content layer's turn.
fn tick(state: &mut State) -> Control {
    if !state.content_dirty {
        return Control::None;
    }
    state.content_dirty = false;
    let text = state.app.query().text().to_string();
    let answer = state.source.search(&text, LIMIT);
    apply(state, answer);
    paint(state)
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
    if !status.connected {
        parts.push("sem ligação ao daemon".to_string());
    } else {
        parts.push(format!("{} resultados", status.matches));
        parts.push(status.layer.to_string());
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
    }
    if let Some(warning) = state.app.warnings().first() {
        parts.push(warning.clone());
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

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
            source,
            content_dirty: false,
            history: History::new(),
            usage_path: None,
        }
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
}
