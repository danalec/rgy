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
//!
//! # The worker
//!
//! The daemon is asked on a **thread of its own**. The thread that owns
//! the window must never wait for the daemon: the daemon can take
//! seconds to answer, and a window that waits for it is a window that
//! does not move and does not take keys. The question goes down a
//! channel, the answer comes back the same way, and the answer is
//! applied on the thread that draws the window.
//!
//! One question is on its way at a time. A keystroke typed while the
//! answer is in flight does not start a second question: the answer
//! that comes back is the answer to the question that was asked, and an
//! answer for a question the user has moved on from is dropped.

use std::sync::mpsc::{Receiver, Sender, channel};

use rgy_launcher::TapKey;
use rgy_launcher::latency::LatencyMeter;
use rgy_launcher::settings::Hotkey;
use rgy_launcher::text::{Lang, Text};
use rgy_launcher::usage::History;
use rgy_launcher::{Action, Answer, App, Layer, Mode, Preview, Row, Source, commands};
use rgy_launcher_windows::actions;
use rgy_launcher_windows::combo::Combo;
use rgy_launcher_windows::hook::Hook;
use rgy_launcher_windows::window::{Compositor, Control, Event, Key, View, Window};

use crate::Options;
use crate::daemon::{DaemonSource, Status};
use crate::mru;
use crate::preview;

/// How many results the window asks for.
const LIMIT: usize = 50;

/// The armed hotkey: the shape the settings chose.
///
/// The two shapes are two mechanisms — the double tap of a lone
/// modifier, watched by the low-level hook, and a combo the platform
/// watches for — and only one is ever armed. The settings choose, and
/// the panel can change the choice while the launcher runs: the old
/// one is dropped and the new one armed in its place, and both guards
/// give their global thing back when they go. The guards are **held**
/// and not read: the drop is the work.
#[allow(dead_code)]
enum Arm {
    /// A lone modifier, tapped twice inside a window of milliseconds.
    Tap(Hook),
    /// Modifiers held with a key, registered with the platform.
    Combo(Combo),
}

impl Arm {
    /// Arm the shape the settings chose for the window's hotkey.
    fn of(hwnd: *mut core::ffi::c_void, hotkey: &Hotkey) -> Result<Arm, String> {
        match hotkey {
            Hotkey::DoubleTap { key, window_ms } => {
                Ok(Arm::Tap(Hook::install(hwnd, *key, *window_ms)?))
            }
            Hotkey::Combo { modifiers, key } => {
                Ok(Arm::Combo(Combo::install(hwnd, *modifiers, *key)?))
            }
        }
    }
}

/// The chart's width, in characters.
///
/// The window is narrower than a terminal, and the chart is the first
/// thing that goes when the space runs out: the percentiles are the
/// figures that stay.
const SPARK_COLUMNS: usize = 16;

/// What the launcher is waiting for.
///
/// The window is up while the answer is on its way: the daemon can take
/// seconds to answer, and a launcher that is invisible while it waits is
/// a launcher that looks broken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Waiting {
    /// Nothing: the launcher is not waiting for an answer.
    Idle,
    /// An answer is on its way.
    Answer,
}

/// What the state needs from a source.
///
/// The launcher's own trait is the question and the answer; the window
/// needs three more things: the daemon is asked on another thread, the
/// session belongs to the window, and the answer comes back to the
/// thread that draws it.
///
/// A preview that came back, with the path it answers for.
///
/// The path is the answer's: a preview for a path the user has moved on
/// from is dropped, and the panel does not show a file the user did not
/// ask about.
type PreviewReply = (String, Preview);

/// The trait is here and not in the launcher crate because the launcher
/// crate is `no_std` and knows nothing about a window.
trait WindowSource {
    /// The session the daemon knows this window by.
    fn set_session(&mut self, id: u64);

    /// Ask the daemon who it is.
    ///
    /// False when the question did not go down: the worker is gone, and
    /// an answer that cannot come is not an answer.
    fn ask_who(&mut self) -> bool;

    /// Ask the daemon for the rows.
    ///
    /// False when the question did not go down, for the same reason.
    fn ask(&mut self, query: &str, limit: usize, generation: u64) -> bool;

    /// The answer that came back, if there is one.
    fn take(&mut self) -> Option<Reply>;
}

/// A question that goes down to the worker.
enum Request {
    /// The daemon's own question: whether it is there, and whether it
    /// has a pack store.
    Who,
    /// The user's question, and the number of the question it is.
    Search {
        /// The question, as the user typed it.
        query: String,
        /// How many results are asked for.
        limit: usize,
        /// The number of the question.
        generation: u64,
    },
    /// The session the daemon knows this window by.
    Session(u64),
}

/// An answer that comes back from the worker.
enum Reply {
    /// The daemon's own question, answered.
    Who {
        /// The sentences the user is to be told.
        messages: Vec<String>,
        /// The status the answer left behind.
        status: Status,
    },
    /// The user's question, answered.
    Search {
        /// The number of the question the answer is for.
        generation: u64,
        /// The rows.
        answer: Answer,
        /// The status the answer left behind.
        status: Status,
    },
}

/// What the thread that asks the daemon does.
///
/// The trait is the seam a test uses: a worker that takes three seconds
/// to answer is a worker the window must not wait for.
trait Worker {
    /// The answer to the question. `None` when the question needs no
    /// answer.
    fn answer(&mut self, request: Request) -> Option<Reply>;

    /// The window to wake when the answer is in the channel.
    ///
    /// The answer is applied on the window's thread, and that thread is
    /// asleep in the message loop until something happens: a timer, a
    /// key, the platform. The worker that put the answer in the channel
    /// is the one that knows it is there, so it is the one that wakes
    /// the loop — a launcher that waits for the next tick to look at an
    /// answer that has already arrived is a launcher that shows it up to
    /// a clock later than it could.
    ///
    /// The default is a worker that has no window to wake: the tests'
    /// workers answer questions, and the window's own answers to the
    /// questions that matter.
    fn wake(&self) -> Option<usize> {
        None
    }
}

/// The worker that asks the daemon.
///
/// The source lives here, on the thread that asks: the window's thread
/// must not hold the connection, because the connection is what waits.
struct DaemonWorker {
    /// The source that answers.
    source: DaemonSource,
    /// The window to wake when an answer is on the channel.
    ///
    /// The handle is carried as a number: a window handle is the
    /// platform's own, and the thread that asks holds no window.
    wake: Option<usize>,
}

impl Worker for DaemonWorker {
    fn answer(&mut self, request: Request) -> Option<Reply> {
        match request {
            Request::Who => Some(Reply::Who {
                messages: self.source.ask_the_daemon(),
                status: self.source.snapshot(),
            }),
            Request::Search {
                query,
                limit,
                generation,
            } => Some(Reply::Search {
                generation,
                answer: self.source.search(&query, limit),
                status: self.source.snapshot(),
            }),
            Request::Session(id) => {
                self.source.set_session(id);
                None
            }
        }
    }

    fn wake(&self) -> Option<usize> {
        self.wake
    }
}

/// The thread that asks the daemon.
///
/// The worker is built **inside** the thread: the source that answers
/// holds a connection, and a connection is not something the thread
/// that draws the window may keep.
fn spawn_worker<F, W>(make: F) -> (Sender<Request>, Receiver<Reply>)
where
    F: FnOnce() -> W + Send + 'static,
    W: Worker + 'static,
{
    let (outbox, requests) = channel::<Request>();
    let (answers, inbox) = channel::<Reply>();
    std::thread::Builder::new()
        .name("rgxy-daemon".to_string())
        .spawn(move || {
            let mut worker = make();
            while let Ok(request) = requests.recv() {
                let Some(reply) = worker.answer(request) else {
                    continue;
                };
                if answers.send(reply).is_err() {
                    // The window is gone: nobody is going to read the
                    // answer.
                    break;
                }
                // The answer is in the channel, and the window's thread
                // is asleep in its message loop: the worker is the one
                // that knows the answer is there, so it is the one that
                // wakes the loop for it.
                if let Some(hwnd) = worker.wake() {
                    rgy_launcher_windows::window::wake(hwnd);
                }
            }
        })
        .expect("the daemon's thread starts");
    (outbox, inbox)
}

/// The daemon, asked on a worker thread.
///
/// The window's thread must never wait for the daemon: the daemon can
/// take seconds to answer, and the window that waits for it is a
/// window that does not move. The question goes down a channel and the
/// answer comes back the same way, and the thread that draws the
/// window blocks on neither.
struct Daemon {
    /// The questions that go down.
    outbox: Sender<Request>,
    /// The answers that come back.
    inbox: Receiver<Reply>,
}

impl Daemon {
    /// The daemon, asked on a worker thread that wakes the window.
    ///
    /// The handle is the window's own: when an answer lands in the
    /// channel the worker wakes the loop with it, so the answer is
    /// applied when it arrives and not at the next tick of the clock.
    /// `None` is a daemon with no window to wake — the tests' sources
    /// answer questions, and the window's own daemon is the one with a
    /// handle.
    fn new(source: DaemonSource, wake: Option<usize>) -> Daemon {
        // The source that answers is the worker's: the window's thread
        // must not hold the connection, because the connection is what
        // waits.
        let endpoint = source.endpoint().clone();
        let rgx = source.rgx().map(str::to_string);
        let window = source.window();
        let lang = source.lang();
        let (outbox, inbox) = spawn_worker(move || DaemonWorker {
            source: DaemonSource::with_window(endpoint, rgx, window, lang),
            wake,
        });
        Daemon { outbox, inbox }
    }

    /// A daemon whose worker is gone.
    ///
    /// The channel the questions go down is closed: nothing reads it,
    /// and the question has nowhere to go.
    #[cfg(test)]
    fn dead() -> Daemon {
        let (outbox, requests) = channel::<Request>();
        drop(requests);
        let (_answers, inbox) = channel::<Reply>();
        Daemon { outbox, inbox }
    }

    /// A daemon that asks through a worker of the test's own.
    #[cfg(test)]
    fn with_worker<F, W>(make: F) -> Daemon
    where
        F: FnOnce() -> W + Send + 'static,
        W: Worker + 'static,
    {
        let (outbox, inbox) = spawn_worker(make);
        Daemon { outbox, inbox }
    }
}

impl WindowSource for Daemon {
    fn set_session(&mut self, id: u64) {
        // The session belongs to the source, and the source is on the
        // worker: the session goes down the same channel the questions
        // do.
        let _ = self.outbox.send(Request::Session(id));
    }

    fn ask_who(&mut self) -> bool {
        // The send fails when the worker is gone: the channel it reads
        // is closed, and the question has nowhere to go.
        self.outbox.send(Request::Who).is_ok()
    }

    fn ask(&mut self, query: &str, limit: usize, generation: u64) -> bool {
        self.outbox
            .send(Request::Search {
                query: query.to_string(),
                limit,
                generation,
            })
            .is_ok()
    }

    fn take(&mut self) -> Option<Reply> {
        self.inbox.try_recv().ok()
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
    /// The settings, as the file gave them and as the panel holds them.
    ///
    /// The panel edits them in place and writes the file on a thread of
    /// its own; a value the panel changed is a value the launcher is
    /// already wearing — the colours, the seat, the width — and the
    /// next start reads the same answer back.
    settings: rgy_launcher::settings::Settings,
    /// The armed hotkey, when one is armed.
    ///
    /// The panel can change the choice while the launcher runs: the old
    /// guard is dropped — giving the global thing back — and the new
    /// one armed in its place. A refusal keeps the old one armed and
    /// says so on the line under the field: a hotkey that did not arm
    /// and said nothing is a hotkey the user presses twice.
    arm: Option<Arm>,
    /// The window's own handle, for the things the panel re-arms.
    hwnd: *mut core::ffi::c_void,
    /// How many rows this screen has room for.
    ///
    /// The settings' row count is a wish and the screen is the ceiling:
    /// the panel adjusts the wish, and the ceiling is read once, where
    /// the window was made.
    rows_fit: usize,
    /// Where the settings file is, when the panel writes it.
    ///
    /// The path is a field and not a call for the reason the memory's
    /// is: a test that adjusted a setting would write the user's own
    /// file, and `None` is how a test says "there is no file here".
    settings_path: Option<std::path::PathBuf>,
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
    /// Whether a question is on its way to the daemon.
    ///
    /// One at a time: a keystroke typed while the first question is on
    /// its way does not start a second question, and the answer that
    /// comes back is the answer to the question that was asked.
    in_flight: bool,
    /// The question the launcher is on.
    ///
    /// The number goes up every time the query changes, and the answer
    /// that comes back carries the number of the question it answers: an
    /// answer for a question the user has moved on from is dropped
    /// rather than shown as the answer to the question on the screen.
    generation: u64,
    /// How many clocks have passed since the launcher last tried a daemon
    /// that is not there.
    ///
    /// The clock asks at 120 ms, and a daemon that is not up will not be up
    /// in the next 120 ms: the retry backs off to one try a second
    /// (`RETRY_TICKS`). The count is reset when the daemon answers — a
    /// launcher that has a daemon is a launcher with nothing to retry.
    retry: u64,
    /// The query the rows on the screen answer.
    ///
    /// The rows are the answer to a question, and the question is not
    /// always the one in the field: a keystroke typed while the answer
    /// was on its way leaves the list showing the results of the
    /// question before it.
    rows_query: Option<String>,
    /// How long the answers take.
    ///
    /// The meter is the model's, and the launcher keeps one: the daemon
    /// says how long each answer took, and the window draws the chart of
    /// the last few dozen of them. A window that shows one figure and
    /// calls it a chart is a window that draws a history it never had.
    meter: LatencyMeter,
    /// The status of the answer that is on the screen.
    ///
    /// The status comes with the answer: the count, the layer and the
    /// time belong to the answer, and a status that describes another
    /// answer describes a question the user is not asking.
    status: Status,
    /// What the launcher has to say about the machine it is on.
    ///
    /// One note and not a list: the note is about the machine, it is
    /// true while the launcher is the way it is, and an answer does not
    /// change it. A list would grow with every use and push the answer
    /// off the line.
    note: Option<String>,
    /// The notes that arrive while the window is down.
    ///
    /// The write of the memory is on a thread of its own, and what it
    /// has to say arrives here.
    notes: Receiver<String>,
    /// Where the notes go.
    note_tx: Sender<String>,
    /// The previews that come back from the thread that reads them.
    ///
    /// The preview reads the file, and the file may be on a share that
    /// is not there: the read blocks for the SMB timeout, and the
    /// thread that draws the window must not block.
    previews: Receiver<PreviewReply>,
    /// Where the previews go.
    preview_tx: Sender<PreviewReply>,
    /// The path whose preview is being read.
    ///
    /// Two previews can be on their way at once, and the one the user
    /// asked for last is the one that counts: a preview that arrives
    /// for a path the user has moved on from is dropped, and the panel
    /// does not show a file the user did not ask about.
    preview_pending: Option<String>,
    /// What the user used, and when.
    ///
    /// The launcher's own memory: the daemon ranks by relevance, and what
    /// the user opened yesterday is the one thing it cannot know.
    history: History,
    /// Where the memory is kept.
    usage_path: Option<std::path::PathBuf>,
    /// The language the launcher is speaking.
    ///
    /// The window says sentences — under the field, in the menu, in the
    /// panel — and a sentence is said in the language the user reads.
    lang: Lang,
    /// What the launcher has already said on the stderr.
    ///
    /// The daemon's absence is a **state**, and the launcher asks about it
    /// on the clock — and the clock ticks every hundred and twenty
    /// milliseconds. A state said again and again is a flood: the
    /// launcher's stderr fills in a minute, and a pipe nobody reads fills
    /// in a second.
    ///
    /// So the stderr carries the message once, when it changes, and the
    /// window carries it the whole time, in the line under the field.
    said: Vec<String>,
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
        } else {
            Waiting::Answer
        }
    }

    /// The user opened a result: the launcher remembers it, and writes it
    /// down so that the next run of the launcher knows it too.
    fn record_use(&mut self, path: &str) {
        self.history.record(path, mru::now());
        let lang = self.lang;
        // The write is on a thread of its own: the store is behind a
        // lock, and the lock can be held for a couple of seconds. The
        // launcher that opened the result is already gone by then, and
        // the window that waits for a file lock is a window that does
        // not move.
        let Some(file) = self.usage_path.clone() else {
            return;
        };
        let history = self.history.clone();
        let notes = self.note_tx.clone();
        std::thread::spawn(move || {
            // The write is tried and its answer is kept: a memory that
            // cannot be written is not a launcher that stops working — the
            // use is remembered for this session, and the write is tried
            // again on the next use. The user is told anyway: a memory
            // that is silently not written is a memory the user believes
            // in and the machine does not have.
            if let Err(reason) = mru::save(&file, &history) {
                let message = Text::for_lang(lang)
                    .history_not_saved
                    .replace("{reason}", &reason.to_string());
                eprintln!("rgxy: {message}");
                let _ = notes.send(message);
            }
        });
    }
}

/// Run the launcher, until the window is closed.
///
/// The settings are the file's, read once before the window is made:
/// the panel writes the file and the next start reads it, and there is
/// no second place the settings live. A file that could not be read is
/// the defaults plus a note — a launcher that refuses to start over its
/// own settings is a launcher the user cannot fix from the inside.
pub fn run(
    source: DaemonSource,
    options: &Options,
    settings: &rgy_launcher::settings::Settings,
    settings_note: Option<String>,
) -> Result<(), String> {
    // The language is the source's: the source says the sentences about the
    // daemon, and the window says the ones about itself.
    let lang = source.lang();
    let mut window = Window::new("rgxy")?.with_lang(lang);
    // The window is the settings': the colours it is painted in, the
    // seat it sits on and the glass it is made of are the file's, and
    // they are applied before anything is drawn so the first frame is
    // already the user's.
    window.set_colours(
        settings.overlay.palette.theme(rgy_launcher::Theme::WARM),
        settings.overlay.alpha,
    );
    window.set_anchor(settings.overlay.x, settings.overlay.y);
    // The hotkey is a global thing, and a global thing can be refused:
    // the launcher still works, it is just opened another way. Which
    // shape is armed is the settings' own — the double tap of a lone
    // modifier, or a combo the platform watches for — and the panel can
    // change it while the launcher runs, so the armed one is held for
    // the whole of the run below.
    let mut refused = None;
    let mut arm = None;
    if options.hook {
        match Arm::of(window.hwnd(), &settings.overlay.hotkey) {
            Ok(armed) => arm = Some(armed),
            Err(reason) => {
                // The reason is for the window as well as for the stderr:
                // a program started from a shortcut has no stderr, and a
                // launcher that cannot be opened and says nothing is a
                // process the user has to kill.
                let message = Text::for_lang(lang)
                    .hotkey_not_installed
                    .replace("{reason}", &reason.to_string());
                eprintln!("rgxy: {message}");
                refused = Some(message);
            }
        }
    }

    // The memory is read once, when the launcher starts: the launcher is
    // resident, and the file is the memory of the last time it ran.
    let usage_path = mru::store_path();
    let (history, memory_note) = memory(usage_path.as_deref(), lang);

    // What the launcher has to say about the machine it is on: a
    // refused hotkey and a memory that could not be read are two
    // facts, and the line under the field carries both of them — in
    // one note, because a note that grows with every use pushes the
    // answer off the line.
    let mut facts: Vec<String> = Vec::new();
    // The window goes up when the launcher was asked to show itself, and
    // also when the hotkey was refused: the hotkey is the only thing that
    // raises the window, and a launcher that cannot be raised is a
    // process the user has to kill. A memory that could not be read does
    // not raise the window: the launcher is still resident, and the line
    // says what happened when the user opens it.
    let mut open_at_startup = options.show;
    if let Some(message) = refused {
        facts.push(message);
        open_at_startup = true;
    }
    if let Some(message) = memory_note {
        facts.push(message);
    }
    if let Some(message) = settings_note {
        facts.push(message);
    }
    // The compositor refused the window's own picture: the window is up
    // and it paints itself, without the translucency, and the line says
    // so. A window that is not what it was is a thing the user has to be
    // told about, and the line under the field is where the launcher says
    // what is true of the machine it is on.
    if window.compositor() == Compositor::Refused {
        facts.push(Text::for_lang(lang).screen_refused_the_surface.to_string());
    }
    let note = if facts.is_empty() {
        None
    } else {
        Some(facts.join("; "))
    };

    let (note_tx, notes): (Sender<String>, Receiver<String>) = channel();
    let (preview_tx, previews): (Sender<PreviewReply>, Receiver<PreviewReply>) = channel();
    // The settings panel draws the rows the launcher has, and the window
    // is sized for them while the panel is open: the count is the host's
    // (`SETTINGS`), and a panel whose count the geometry did not know was
    // a list of settings drawn nowhere.
    let mut app = App::new();
    app.set_settings_rows(SETTINGS.len());
    let mut state = State {
        app,
        settings: settings.clone(),
        arm,
        hwnd: window.hwnd(),
        rows_fit: rgy_launcher::app::rows_that_fit(window.work_area_height()),
        settings_path: crate::settings_store::store_path(),
        source: Box::new(Daemon::new(source, Some(window.hwnd() as usize))),
        clipboard: Box::new(actions::paste_text),
        // The seams answer in the user's language: the machine's own
        // refusal is said in the language the user reads.
        open: Box::new(move |path: &str| actions::open(path, lang)),
        reveal: Box::new(move |path: &str| actions::reveal(path, lang)),
        waiting: Waiting::Idle,
        ask_daemon: true,
        in_flight: false,
        generation: 0,
        retry: 0,
        rows_query: None,
        meter: LatencyMeter::new(),
        status: Status::default(),
        note,
        notes,
        note_tx,
        previews,
        preview_tx,
        preview_pending: None,
        history,
        usage_path,
        lang,
        said: Vec::new(),
    };

    // The rows are the user's wish capped by the screen: a user who asks
    // for twenty rows on a screen that has room for twelve gets twelve,
    // and a user who asks for five gets five. The width is the user's
    // own dimension, with no screen to answer to.
    state.app.set_width(settings.overlay.width);
    state
        .app
        .set_visible_rows(settings.overlay.rows.min(state.rows_fit as u32) as usize);

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
    // the user is a launcher that is late for both. The question goes
    // down to the worker, and the thread that draws the window never
    // waits for the answer.

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
fn memory(path: Option<&std::path::Path>, lang: Lang) -> (History, Option<String>) {
    let Some(path) = path else {
        return (History::new(), None);
    };
    match mru::load(path) {
        Ok(history) => (history, None),
        Err(err) => {
            let message = mru::read_error(&err, lang);
            // The stderr of a program started from a shortcut is
            // nowhere: the line under the field says it as well.
            eprintln!("rgxy: {message}");
            (History::new(), Some(message))
        }
    }
}

/// One event, and what the window should do about it.
fn handle(state: &mut State, event: Event) -> Control {
    // The notes that arrived while the window was down are about the
    // machine, and the line under the field is where the machine is
    // described.
    take_notes(state);
    // The preview that came back is applied here too: it was read on
    // another one, for the same reason.
    let previewed = take_preview(state);
    // The answer that came back is applied here, on the thread that
    // draws the window: the daemon was asked on another one.
    let answered = apply_reply(state);
    let control = match event {
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
                // clock stops with it. The answer that was on its way
                // lands when the window comes back up — the question it
                // answers is still the one in the field.
                state.waiting = Waiting::Idle;
                Control::Hide
            }
        }
        Event::Show => {
            // The window comes up, and it does not go down. The icon in
            // the notification area asks with this: a double click is two
            // clicks, and a toggle would show the window and hide it
            // again.
            let was_up = state.app.is_visible();
            state.app.show();
            if !was_up {
                // A new window is a new session: the previous window's
                // result set is not the answer to a new question.
                state.source.set_session(state.app.session());
                // The window goes up **now**, before the daemon is asked,
                // for the same reason the shortcut's window does.
                state.waiting = state.waiting_for();
                state.ask_daemon = true;
            }
            show(state)
        }
        Event::Key(key) => on_key(state, key),
        Event::Tick => tick(state),
        Event::Wake => {
            // The answer is in the channel, and the top of this
            // function has already applied it. A question typed while it
            // was on its way is asked **now** — the tick is the
            // launcher's patience, not its speed.
            if state.waiting != Waiting::Idle && !state.in_flight {
                return tick(state);
            }
            Control::None
        }
        Event::Resize => paint(state),
        Event::Quit => Control::Quit,
    };
    if (answered || previewed) && matches!(control, Control::None) {
        // The answer is on the screen and nothing else changed: the
        // window has to be told.
        return paint(state);
    }
    control
}

/// The notes that arrived while the window was down.
///
/// The note is about the machine: it is true while the launcher is the
/// way it is, and an answer does not change it. One note and not a
/// list: a list grows with every use and pushes the answer off the
/// line.
fn take_notes(state: &mut State) {
    while let Ok(message) = state.notes.try_recv() {
        state.note = Some(message);
    }
}

/// The preview that came back.
///
/// The preview answers for the path it was asked about: two previews
/// can be on their way at once, and the one that lands is the one the
/// user asked for last. A preview for a path the user has moved on from
/// is dropped -- the panel does not show a file the user did not ask
/// about.
fn take_preview(state: &mut State) -> bool {
    let mut applied = false;
    while let Ok((path, preview)) = state.previews.try_recv() {
        if state.preview_pending.as_deref() == Some(path.as_str()) {
            state.preview_pending = None;
            state.app.set_preview(preview);
            applied = true;
        }
    }
    applied
}

/// A key.
fn on_key(state: &mut State, key: Key) -> Control {
    // The settings panel has the keyboard while it is open.
    if state.app.mode() == Mode::Settings {
        return settings_key(state, key);
    }

    // The menu has the keyboard while it is open.
    if state.app.mode() == Mode::Menu {
        return menu_key(state, key);
    }

    // The daemon is not there: the query does not grow.
    //
    // A query nobody will answer is a query the user typed into a field that
    // only fills up: the characters go in, nothing comes back, and the field
    // says nothing about why. The launcher KNOWS the daemon is not there --
    // the answer said so, and a daemon that is up but slow is not a daemon
    // that is not there -- so the characters and the paste are refused while
    // it is not, and the line under the field says why.
    //
    // The keys that erase still work: the user typed something before the
    // launcher learned the daemon was gone, and a field that cannot be
    // emptied is a field the user is stuck with.
    //
    // And the **command line** still types: a `>`-line is the launcher's
    // own and not a question for the daemon, so `>settings` opens the
    // panel exactly when the daemon is away and the user wants to look at
    // the launcher's own settings. Measured on 2026-11-04: with the daemon
    // down, the `>` was refused like any other character, and the panel —
    // the one thing that needs no daemon — could not be reached.
    if state.status.asked
        && !state.status.connected
        && adds_to_the_query(key)
        && !the_line_is_a_command(state, key)
    {
        return paint(state);
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

/// Whether the key puts something in the query.
///
/// Two keys do: the characters and the paste. The others move the caret,
/// move the selection, erase, run an action or close the window -- and a
/// window with no daemon has nothing to select and nothing to open, but it
/// has a field the user has to be able to empty.
fn adds_to_the_query(key: Key) -> bool {
    matches!(key, Key::Char(_) | Key::CtrlV)
}

/// Whether the key keeps the line a command line.
///
/// A `>`-line is the launcher's own and not a question for the daemon, so
/// it types while the daemon is away — and the panel it opens is the one
/// thing that needs no daemon at all. Two ways the line can be a command
/// while the daemon is down: it already is one, or this key is the `>`
/// that starts it at the head of the field.
fn the_line_is_a_command(state: &State, key: Key) -> bool {
    if state.app.is_command() {
        return true;
    }
    matches!(key, Key::Char('>')) && state.app.query().caret() == 0
}

/// The query changed: the launcher asks its own question.
///
/// The question is asked on the **clock**: the daemon can take seconds to
/// answer, and a key handler that waits for it is a window that does not
/// move. The line under the field says the launcher is looking, and the
/// answer lands in a window that is already up.
fn changed(state: &mut State) -> Control {
    // The question moved on: the answer that comes back for the
    // previous question is dropped, and the rows that are on the screen
    // are the answer to the question before it.
    state.generation = state.generation.wrapping_add(1);
    // A command line is not a search: the engine would look for the
    // literal text ">folder" and answer nothing, while the line is a
    // command — so the launcher takes it before the engine sees it.
    if state.app.is_command() {
        // A command is not a question for the daemon either: the answer
        // the window was waiting for is not this one.
        state.waiting = Waiting::Idle;
        if state.app.command().is_none() {
            state.app.set_warnings(vec![commands::unknown(
                state.app.query().text(),
                state.lang,
            )]);
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
///
/// The line under the field goes with it. It said "37 results" and
/// "content" about an answer that was no longer on the screen, and a line
/// that describes another answer describes a question the user is not
/// asking.
fn cleared(state: &mut State) {
    state.app.set_results(Vec::new());
    state.app.set_warnings(Vec::new());
    state.rows_query = None;
    state.status.matches = 0;
    state.status.layer = None;
    state.status.backend.clear();
    state.status.took_us = 0;
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
            state.app.set_warnings(vec![
                Text::for_lang(state.lang).nothing_to_paste.to_string(),
            ]);
            false
        }
        actions::Clipboard::NotText => {
            state.app.set_warnings(vec![
                Text::for_lang(state.lang).clipboard_is_empty.to_string(),
            ]);
            false
        }
        actions::Clipboard::Busy => {
            state.app.set_warnings(vec![
                Text::for_lang(state.lang).clipboard_is_busy.to_string(),
            ]);
            false
        }
    }
}

/// A key, while the actions menu is open.
///
/// The menu has the keyboard for the keys that **choose an action**: the
/// arrows move it, `Enter` runs it, and `Escape` closes it.
///
/// Every other key is the **field's** — a letter, a `Backspace`, a
/// `Ctrl+U`, a paste, and the keys of the actions themselves — and the
/// menu closes and the key does what it does in the field. The paste
/// already worked that way, and a letter that was swallowed while the
/// menu was open was a key that did nothing and said nothing: a launcher
/// the user cannot tell from a frozen one. The user who opens the menu
/// and starts typing is asking a new question, and the menu is in the
/// way of it.
///
/// The key is handed to the field's own handler with the menu already
/// closed, so there is one place where a letter means what a letter
/// means.
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
        other => {
            state.app.menu_closed();
            return on_key(state, other);
        }
    }
    paint(state)
}

/// The command line, on Enter.
fn command(state: &mut State) -> Control {
    match state.app.command() {
        Some(action) => run_action(state, action),
        None => {
            state.app.set_warnings(vec![commands::unknown(
                state.app.query().text(),
                state.lang,
            )]);
            paint(state)
        }
    }
}

/// One row of the settings panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Setting {
    /// The overlay's template.
    Theme,
    /// How wide the window is.
    Width,
    /// How many rows the list shows.
    Rows,
    /// Where the window sits across the work area.
    Across,
    /// Where the window sits down the work area.
    Down,
    /// How opaque the panel is.
    Alpha,
    /// The key that opens the launcher.
    Hotkey,
    /// How long the window between the two taps is.
    HotkeyWindow,
    /// The terminal's template.
    Tui,
    /// The picker's template.
    Fzf,
    /// The picker's free arguments, read from the file.
    FzfArgs,
    /// The way back to the faces as they were.
    Defaults,
}

/// The rows of the settings panel, in the order the panel lists them.
///
/// The order is the code's and the panel's one order: the rows are
/// navigated by index, and an order that differed between the two would
/// be a panel that adjusts another row than the one it lit.
const SETTINGS: &[Setting] = &[
    Setting::Theme,
    Setting::Width,
    Setting::Rows,
    Setting::Across,
    Setting::Down,
    Setting::Alpha,
    Setting::Hotkey,
    Setting::HotkeyWindow,
    Setting::Tui,
    Setting::Fzf,
    Setting::FzfArgs,
    Setting::Defaults,
];

/// The rows of the settings panel, spelled for the picture.
///
/// The labels are the vocabulary's and the values are the settings'
/// own words — a template's name, a number, a hotkey's spell — so the
/// panel says exactly what the file says and the file says exactly what
/// the panel set.
fn settings_rows(state: &State) -> Vec<(String, String)> {
    let text = Text::for_lang(state.lang);
    let overlay = &state.settings.overlay;
    SETTINGS
        .iter()
        .map(|setting| {
            let (label, value) = match setting {
                Setting::Theme => (text.settings_theme, overlay.palette.template.clone()),
                Setting::Width => (text.settings_width, overlay.width.to_string()),
                Setting::Rows => (text.settings_rows, overlay.rows.to_string()),
                Setting::Across => (text.settings_position, format!("{}%", overlay.x / 10)),
                Setting::Down => (text.settings_position, format!("{}%", overlay.y / 10)),
                Setting::Alpha => (text.settings_alpha, overlay.alpha.to_string()),
                Setting::Hotkey => (text.settings_hotkey, overlay.hotkey.spell()),
                Setting::HotkeyWindow => (
                    text.settings_hotkey_window,
                    match overlay.hotkey {
                        Hotkey::DoubleTap { window_ms, .. } => format!("{window_ms} ms"),
                        Hotkey::Combo { .. } => "—".to_string(),
                    },
                ),
                Setting::Tui => (text.settings_tui, state.settings.tui.template.clone()),
                Setting::Fzf => (
                    text.settings_fzf,
                    state.settings.fzf.palette.template.clone(),
                ),
                Setting::FzfArgs => (
                    text.settings_fzf_args,
                    if state.settings.fzf.args.is_empty() {
                        "—".to_string()
                    } else {
                        state.settings.fzf.args.join(" ")
                    },
                ),
                Setting::Defaults => (text.settings_defaults, String::new()),
            };
            (label.to_string(), value)
        })
        .collect()
}

/// A key, while the settings panel is open.
///
/// The rule is the menu's: the panel holds the keyboard only for the
/// keys that adjust it — the arrows that move and step, `Enter` that
/// steps forward, `Escape` that closes — and every other key is the
/// **field's**, handed over with the panel already closed, so there is
/// one place where a letter means what a letter means.
fn settings_key(state: &mut State, key: Key) -> Control {
    match key {
        Key::Up => state.app.settings_previous(SETTINGS.len()),
        Key::Down => state.app.settings_next(SETTINGS.len()),
        Key::Left => adjust_setting(state, -1),
        Key::Right | Key::Enter => adjust_setting(state, 1),
        Key::Esc => state.app.settings_closed(),
        other => {
            state.app.settings_closed();
            return on_key(state, other);
        }
    }
    paint(state)
}

/// Adjust the selected setting, one step in the direction the key said.
///
/// The step lands at once: the setting is worn on the next frame — the
/// theme, the seat, the width — the hotkey is re-armed where it stands,
/// and the file is written off the window's thread. A row the arrows
/// cannot step still answers the key: the defaults are restored by any
/// step on their row.
fn adjust_setting(state: &mut State, dir: i32) {
    let index = state.app.settings_index().min(SETTINGS.len() - 1);
    match SETTINGS[index] {
        Setting::Theme => step_template(&mut state.settings.overlay.palette.template, dir),
        Setting::Width => {
            state.settings.overlay.width = step(state.settings.overlay.width, 20, 400, 1000, dir);
            state.app.set_width(state.settings.overlay.width);
        }
        Setting::Rows => {
            state.settings.overlay.rows = step(state.settings.overlay.rows, 1, 1, 30, dir);
            state
                .app
                .set_visible_rows(state.settings.overlay.rows.min(state.rows_fit as u32) as usize);
        }
        Setting::Across => {
            state.settings.overlay.x = step(state.settings.overlay.x, 25, 0, 1000, dir)
        }
        Setting::Down => {
            state.settings.overlay.y = step(state.settings.overlay.y, 25, 0, 1000, dir)
        }
        Setting::Alpha => {
            state.settings.overlay.alpha = step(state.settings.overlay.alpha, 10, 64, 255, dir)
        }
        Setting::Hotkey => {
            // Cycling the key takes the double-tap shape: a combo is a
            // sentence a free-text field would have to write, and the
            // file is where that sentence is written.
            let key = match state.settings.overlay.hotkey {
                Hotkey::DoubleTap { key, .. } => key,
                Hotkey::Combo { .. } => rgy_launcher::TapKey::Ctrl,
            };
            let next = cycle(TapKey::ALL, key, dir);
            state.settings.overlay.hotkey = Hotkey::DoubleTap {
                key: next,
                window_ms: 400,
            };
            rearm(state);
        }
        Setting::HotkeyWindow => {
            let (key, window_ms) = match state.settings.overlay.hotkey {
                Hotkey::DoubleTap { key, window_ms } => (key, window_ms),
                Hotkey::Combo { .. } => (rgy_launcher::TapKey::Ctrl, 400),
            };
            let window_ms = step(window_ms, 50, 100, 1500, dir);
            state.settings.overlay.hotkey = Hotkey::DoubleTap { key, window_ms };
            rearm(state);
        }
        Setting::Tui => step_template(&mut state.settings.tui.template, dir),
        Setting::Fzf => step_template(&mut state.settings.fzf.palette.template, dir),
        Setting::FzfArgs => {}
        Setting::Defaults => {
            state.settings = rgy_launcher::settings::Settings::default();
            state.app.set_width(state.settings.overlay.width);
            state
                .app
                .set_visible_rows(state.settings.overlay.rows.min(state.rows_fit as u32) as usize);
            rearm(state);
        }
    }
    save_settings(state);
}

/// A number, one step in the direction the key said, inside the range
/// the settings hold.
fn step(value: u32, by: i32, min: u32, max: u32, dir: i32) -> u32 {
    let moved = value as i64 + by as i64 * dir as i64;
    moved.clamp(min as i64, max as i64) as u32
}

/// A template's name, one step in the direction the key said.
///
/// A name nobody has is the first one's neighbour: a file that named a
/// theme from a newer launcher still gets to step somewhere.
fn step_template(current: &mut String, dir: i32) {
    let names = rgy_launcher::Theme::names();
    let at = names
        .iter()
        .position(|name| *name == current.as_str())
        .map(|at| at as i64)
        .unwrap_or(0);
    let next = (at + dir as i64).rem_euclid(names.len() as i64) as usize;
    *current = names[next].to_string();
}

/// One of a short list, one step in the direction the key said.
fn cycle<T: Copy + PartialEq>(all: &[T], current: T, dir: i32) -> T {
    let at = all
        .iter()
        .position(|one| *one == current)
        .map(|at| at as i64)
        .unwrap_or(0);
    all[(at + dir as i64).rem_euclid(all.len() as i64) as usize]
}

/// Arm the hotkey the settings now name.
///
/// The old shape is dropped **first** — its guard gives the global thing
/// back — and a refusal says so on the line under the field: a hotkey
/// that did not arm and said nothing is a hotkey the user presses twice.
///
/// The order is the whole point and not a detail. A combo is registered
/// with a window and an id, and registering another combo with the same
/// window and the same id **replaces** the one that was there — so the
/// guard that drops the old one would take the new one with it, and the
/// launcher would stop answering to the key the user has just chosen.
/// The old shape goes first, and the new one is armed in its place.
///
/// A refusal now leaves the launcher without a hotkey, and that is the
/// honest answer: the old shape is gone, and keeping it would be keeping
/// the choice the user has just changed.
fn rearm(state: &mut State) {
    drop(state.arm.take());
    match Arm::of(state.hwnd, &state.settings.overlay.hotkey) {
        Ok(armed) => state.arm = Some(armed),
        Err(reason) => {
            let message = Text::for_lang(state.lang)
                .hotkey_not_armed
                .replace("{reason}", &reason);
            let _ = state.note_tx.send(message);
        }
    }
}

/// Write the settings to the file, off the window's thread.
///
/// The rule is the history's: a write that can block is never on the
/// thread that draws the window. The settings are small enough that
/// every change is worth a whole write, and the file is the panel's and
/// the hand's one store — what the user set is what the next start
/// reads.
fn save_settings(state: &State) {
    let Some(path) = state.settings_path.clone() else {
        return;
    };
    let settings = state.settings.clone();
    let lang = state.lang;
    let tx = state.note_tx.clone();
    std::thread::spawn(move || {
        if let Err(reason) = crate::settings_store::save(&path, &settings) {
            let message = Text::for_lang(lang)
                .settings_not_saved
                .replace("{reason}", &reason.to_string());
            eprintln!("rgxy: {message}");
            let _ = tx.send(message);
        }
    });
}

/// Do what the user asked of the selected result.
fn run_action(state: &mut State, action: Action) -> Control {
    // The settings are nobody's row: they are the launcher's own panel,
    // and asking the selected result for them would be asking a file
    // what colour the window is. They open with nothing selected, the
    // way the menu of the commands does.
    if action == Action::Settings {
        state.app.open_settings();
        return paint(state);
    }
    let Some(row) = state.app.current().cloned() else {
        // There is nothing to act on. Saying nothing is what makes the
        // user think the launcher is broken, so the line under the field
        // says what happened.
        state.app.set_warnings(vec![
            Text::for_lang(state.lang).no_result_selected.to_string(),
        ]);
        return paint(state);
    };
    // The action has to fit the row: the preview reads the head of a
    // file as text, and for a folder, an image, a video, a sound or a
    // program that head is noise. The refusal says why, in the user's
    // language — a key that was pressed and refused in silence is a key
    // the user presses again.
    if let Some(reason) = action.refusal(row.icon(), state.lang) {
        state.app.set_warnings(vec![reason]);
        return paint(state);
    }
    match action {
        Action::Open => {
            // The answer first: the use is recorded after the open, not
            // before it. The history is about what the user did, and a
            // result that could not be opened is not a result the user
            // used.
            let opened = (state.open)(&row.path);
            match opened {
                Ok(()) => {
                    // The window goes down **before** the memory is
                    // written: the write is behind a lock that another
                    // writer can hold for a couple of seconds, and the
                    // launcher that opened the result does not wait for
                    // any of it.
                    state.app.hide();
                    state.record_use(&row.path);
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
            let message = match actions::copy_path(&row.path, state.lang) {
                Ok(()) => Text::for_lang(state.lang).path_copied.to_string(),
                Err(reason) => reason,
            };
            state.app.set_warnings(vec![message]);
            paint(state)
        }
        Action::Preview => {
            // The preview reads the file, and the file may be on a share
            // that is not there: the read blocks for the SMB timeout, and
            // the thread that draws the window must not block. The read
            // goes to a thread of its own, and the panel says it is
            // waiting.
            let path = row.path.clone();
            let tx = state.preview_tx.clone();
            let lang = state.lang;
            state.preview_pending = Some(path.clone());
            state.app.set_preview(preview::pending(&path, lang));
            std::thread::spawn(move || {
                let _ = tx.send((path.clone(), preview::of(&path, lang)));
            });
            paint(state)
        }
        Action::Menu => {
            // The result is there: `run_action` has already said so when
            // it is not, and the menu is the list of this result's
            // actions.
            state.app.open_menu();
            paint(state)
        }
        // Taken above, before the row is asked for: the settings are
        // nobody's row. The arm is here because the match is on the whole
        // action, and a setting that somehow reached it opens the panel
        // the way the rowless path above does.
        Action::Settings => {
            state.app.open_settings();
            paint(state)
        }
    }
}

/// The lines the stderr has to carry, when the news changed.
///
/// The launcher asks about the daemon on the clock, and the clock does
/// not stop: a message said on every tick is a flood, and a flood fills
/// the machine's log — and the pipe of a launcher whose reader is slow.
///
/// So the stderr carries the message once, when it changes, and the
/// window carries it the whole time, in the line under the field.
fn said_once(said: &mut Vec<String>, messages: &[String]) -> Option<Vec<String>> {
    if messages == said.as_slice() {
        return None;
    }
    *said = messages.to_vec();
    Some(
        messages
            .iter()
            .map(|message| format!("rgxy: {message}"))
            .collect(),
    )
}

/// The answer that came back, on the screen.
///
/// The answer is applied here, on the thread that draws the window: it
/// was asked on another one, and a row installed by a thread that does
/// not own the model is a row nobody can paint.
///
/// False when nothing was applied: an answer for a question the user
/// has moved on from is dropped, and the question that is on the screen
/// is asked again on the next tick.
fn apply_reply(state: &mut State) -> bool {
    let Some(reply) = state.source.take() else {
        return false;
    };
    match reply {
        Reply::Who { messages, status } => {
            state.in_flight = false;
            state.status = status;
            // The daemon answered: from here the retry is the launcher's
            // next question and not a try at a daemon that is not there.
            if state.status.connected {
                state.retry = 0;
            }
            // The stderr carries the message once, when it changes.
            //
            // And the write is on a thread of its own: a pipe that is
            // full does not take the message back, and a window that
            // waits for the pipe is a window that stops answering. The
            // launcher is resident, and a resident launcher that freezes
            // when the machine's log is full is a launcher the user has
            // to kill.
            if let Some(lines) = said_once(&mut state.said, &messages) {
                std::thread::spawn(move || {
                    for line in lines {
                        eprintln!("{line}");
                    }
                });
            }
            if !messages.is_empty() {
                // Whatever the user has to be told, the window tells it:
                // the stderr of a program started from a shortcut is
                // nowhere.
                state.app.set_warnings(messages);
            }
        }
        Reply::Search {
            generation,
            answer,
            status,
        } => {
            state.in_flight = false;
            if generation != state.generation {
                // The answer is for a question the user has moved on
                // from: it is dropped rather than shown as the answer
                // to the question that is on the screen.
                return false;
            }
            state.status = status;
            // The daemon says how long its answer took, and that is the
            // measurement the meter keeps: the search, not the pipe
            // around it.
            //
            // A zero is not a measurement — the fallback does not say how
            // long it took — and a zero in the window would make the
            // median zero, which is a number nobody can read.
            if state.status.took_us > 0 {
                state.meter.record(u128::from(state.status.took_us));
            }
            apply(state, answer);
            state.waiting = Waiting::Idle;
        }
    }
    true
}

/// The clock ticked: the turn of whatever the launcher was waiting for.
///
/// The window is already on the screen when the daemon is asked — the
/// clock only ticks while the window is up — and the answer lands in
/// that window.
fn tick(state: &mut State) -> Control {
    if state.in_flight {
        // One question at a time: the answer that is on its way is the
        // answer to the question that was asked, and a keystroke typed
        // while it is on its way does not start a second one.
        return Control::None;
    }
    if state.waiting != Waiting::Idle {
        // The question the user typed comes first.
        let generation = state.generation;
        let query = state.app.query().text().to_string();
        if !state.source.ask(&query, LIMIT, generation) {
            return worker_gone(state);
        }
        state.in_flight = true;
        return paint(state);
    }
    // The daemon's own question — whether it is there, and whether it has
    // a pack store — is asked on the clock too, and it waits for a tick
    // with nothing else to do: the question the user typed comes first.
    if state.ask_daemon {
        state.ask_daemon = false;
        if !state.source.ask_who() {
            return worker_gone(state);
        }
        state.in_flight = true;
        return paint(state);
    }
    // The daemon is not there: the launcher asks again on the clock, and
    // the line says so until it is. The daemon is a program that can
    // start after the window opened, and a launcher that says "sem
    // ligacao ao daemon" about a daemon that is there is a launcher
    // that lies.
    //
    // The asking backs off: a daemon that is not up yet is a daemon that
    // will not be up in the next 120 ms, and a launcher that opens a
    // connection every clock for ever is a launcher doing 8.3 round trips
    // a second for nothing. Measured on 2026-11-04, with the daemon
    // absent: that was 4.3% of a core before the no-news paint was
    // skipped and 0.26% after — the connects are the 0.26%. Once a second
    // is still fast enough for a daemon that starts at login, and it is
    // the patience the launcher's own two seconds already speak.
    if state.status.asked && !state.status.connected {
        state.retry = state.retry.wrapping_add(1);
        if !state.retry.is_multiple_of(RETRY_TICKS) {
            return Control::None;
        }
        if !state.source.ask_who() {
            return worker_gone(state);
        }
        state.in_flight = true;
        return paint(state);
    }
    Control::None
}

/// How many clocks pass between two tries at a daemon that is not there.
///
/// The clock is 120 ms; eight of them is a second, which is slow enough
/// that a launcher with no daemon is not a process doing work and fast
/// enough that a daemon that starts is answered within a second.
const RETRY_TICKS: u64 = 8;

/// The worker that asks the daemon is gone.
///
/// The question did not go down, so no answer is coming: the launcher
/// is not waiting for one, and it says why. A launcher that waits for
/// an answer that cannot come is a launcher that stopped answering the
/// keyboard, and one that says nothing about it is a launcher the user
/// cannot tell from a frozen one.
fn worker_gone(state: &mut State) -> Control {
    state.in_flight = false;
    state.waiting = Waiting::Idle;
    state.note = Some(Text::for_lang(state.lang).worker_died.to_string());
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
    // The rows on the screen answer the question that was asked: the
    // line says so when the question on the screen is another one.
    state.rows_query = Some(state.app.query().text().to_string());
    let warnings = state.status.warnings.clone();
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
        placeholder: Text::for_lang(state.lang).placeholder.to_string(),
        rows,
        selected: selected.saturating_sub(top),
        top,
        mode: app.mode(),
        menu: app.menu_actions().to_vec(),
        menu_selected: app.menu_index(),
        warnings: app.warnings().to_vec(),
        status: status_line(state),
        // The figures the launcher has: how long the last answer took,
        // and the chart of the ones before it.
        latency: state.meter.view(SPARK_COLUMNS),
        preview: app.preview().cloned(),
        settings: settings_rows(state),
        settings_selected: app.settings_index(),
        // What the window wears: the panel adjusts the settings and the
        // very next frame carries them, so a theme the user chose is on
        // the screen the moment the arrow leaves the key.
        theme: state
            .settings
            .overlay
            .palette
            .theme(rgy_launcher::Theme::WARM),
        alpha: state.settings.overlay.alpha,
        anchor: (state.settings.overlay.x, state.settings.overlay.y),
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
    let status = &state.status;
    let words = Text::for_lang(state.lang);
    let mut parts: Vec<String> = Vec::new();
    if state.app.mode() == Mode::Menu {
        // The menu has the keyboard, and the line says so: the arrows move
        // the action and not the result, and a key that moves the
        // selection somewhere else without saying anything is a key the
        // user cannot tell from a frozen one.
        parts.push(words.menu_has_the_keyboard.to_string());
    }
    if state.waiting != Waiting::Idle {
        // The answer is on its way, and the line says so: a launcher that
        // is quiet while it waits is a launcher that looks stuck.
        parts.push(words.searching.to_string());
    }
    if status.asked && !status.connected {
        // The daemon was asked and it is not there. The line does not
        // say that about a daemon nobody has asked about yet.
        parts.push(rgy_launcher::text::no_daemon(state.lang).to_string());
    }
    // The count, the layer, the backend and the time belong to the
    // **answer**: they are shown whether the daemon is up or not. The
    // `rgx` fallback answers while the daemon is away, and a line that
    // hides the answer because the daemon is not there hides the one
    // thing the user asked for.
    //
    // But an answer is a thing that exists: a window that has just opened
    // has been asked nothing, and "0 results" over a field nobody has
    // typed in is the launcher claiming to have searched. Measured on
    // 2026-11-04, on the first open: the line under the field said "0
    // results" over the placeholder. So the count is shown when there is
    // an answer on the screen (`rows_query` is the question it answers),
    // and a question that was asked and answered with nothing says
    // "0 results" — which is then the truth.
    if state.rows_query.is_some() {
        let mut count = words.results.replace("{}", &status.matches.to_string());
        if let Some(query) = &state.rows_query
            && query != state.app.query().text()
            && !state.app.results().is_empty()
        {
            // The rows on the screen answer another question, and the line
            // says which one: a list of results under a field that says
            // something else is a list of results for another question.
            count = format!("{count} {}", words.of_query.replace("{query}", query));
        }
        parts.push(count);
    }
    match status.layer {
        None => {}
        Some(Layer::Names) => parts.push(words.layer_names.to_string()),
        Some(Layer::Content) => {
            parts.push(words.layer_content.to_string());
            if !status.backend.is_empty() {
                parts.push(status.backend.clone());
            }
            if status.tv {
                parts.push("TV".to_string());
            }
        }
    }
    if let Some(note) = &state.note {
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
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    /// A directory of the test's own.
    fn scratch(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("rgy-rgxy-{name}-{}", std::process::id()))
    }

    /// An answer with one row in it.
    fn answer_with(path: &str) -> Answer {
        Answer {
            paths: vec![path.to_string()],
            spans: vec![Vec::new()],
            contexts: Vec::new(),
            folders: vec![false],
        }
    }

    /// A state with no daemon and no results.
    fn empty() -> State {
        let (mut state, _) = recording();
        // The daemon was asked and it is not there: the line says so.
        state.status.asked = true;
        state.status.connected = false;
        state
    }

    /// A source that remembers the questions it was asked.
    ///
    /// The daemon is not there in a test, and a question that is never
    /// asked is a question that cannot break: the double answers, and
    /// the test says what the launcher asked.
    struct Recording {
        asked: Rc<RefCell<Vec<String>>>,
        /// How long the daemon says its answer took.
        took: Rc<Cell<u64>>,
        status: Status,
        answers: Vec<Reply>,
    }

    impl Recording {
        fn new(asked: Rc<RefCell<Vec<String>>>, took: Rc<Cell<u64>>) -> Recording {
            Recording {
                asked,
                took,
                status: Status::default(),
                answers: Vec::new(),
            }
        }
    }

    impl WindowSource for Recording {
        fn set_session(&mut self, _id: u64) {}

        fn ask_who(&mut self) -> bool {
            // A test has no daemon: the question is asked and nothing
            // answers it. The status knows that it was asked, so a test
            // can see when the launcher spoke to it.
            self.status.asked = true;
            self.answers.push(Reply::Who {
                messages: Vec::new(),
                status: self.status.clone(),
            });
            true
        }

        fn ask(&mut self, query: &str, _limit: usize, generation: u64) -> bool {
            self.asked.borrow_mut().push(query.to_string());
            // The answer carries the time the daemon took, the way the
            // daemon's own answer does.
            let mut status = self.status.clone();
            status.took_us = self.took.get();
            self.answers.push(Reply::Search {
                generation,
                answer: answer_with("C:/a/notas.txt"),
                status,
            });
            true
        }

        fn take(&mut self) -> Option<Reply> {
            if self.answers.is_empty() {
                None
            } else {
                Some(self.answers.remove(0))
            }
        }
    }

    /// A state whose source is a double, and the questions it was asked.
    fn recording() -> (State, Rc<RefCell<Vec<String>>>) {
        recording_timed(0)
    }

    /// A state whose source is a double, and whose answers took this long.
    ///
    /// The daemon says how long its answer took, and the meter keeps the
    /// figure: a test that cannot make the daemon take time cannot check
    /// that the meter is fed.
    fn recording_timed(us: u64) -> (State, Rc<RefCell<Vec<String>>>) {
        let asked = Rc::new(RefCell::new(Vec::new()));
        let took = Rc::new(Cell::new(us));
        let (note_tx, notes): (Sender<String>, Receiver<String>) = channel();
        let (preview_tx, previews): (Sender<PreviewReply>, Receiver<PreviewReply>) = channel();
        let state = State {
            app: App::new(),
            settings: rgy_launcher::settings::Settings::default(),
            arm: None,
            // A test has no window to hand to the re-arm, and none of the
            // panel's rows is adjusted in a test that does not say so.
            hwnd: core::ptr::null_mut(),
            rows_fit: 12,
            // A test does not write the user's settings file: the panel
            // adjusts the settings in memory, and the file is the
            // machine's.
            settings_path: None,
            source: Box::new(Recording::new(Rc::clone(&asked), Rc::clone(&took))),
            clipboard: Box::new(|| actions::Clipboard::Text(String::new())),
            open: Box::new(|_| Err("a test does not open files".to_string())),
            reveal: Box::new(|_| Err("a test does not open folders".to_string())),
            waiting: Waiting::Idle,
            ask_daemon: false,
            in_flight: false,
            generation: 0,
            retry: 0,
            rows_query: None,
            meter: LatencyMeter::new(),
            status: Status::default(),
            note: None,
            notes,
            note_tx,
            previews,
            preview_tx,
            preview_pending: None,
            history: History::new(),
            usage_path: None,
            lang: Lang::English,
            said: Vec::new(),
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

    /// The note the worker sends, when it arrives.
    ///
    /// The write of the memory is on a thread of its own, and the test
    /// waits for it: a test that does not wait is a test that races the
    /// machine. The wait is longer than the write's own deadline: the
    /// write that cannot take the lock gives up when the deadline is
    /// gone, and the note that says so comes after it.
    fn wait_for_note(state: &mut State) -> Option<String> {
        for _ in 0..300 {
            take_notes(state);
            if let Some(note) = state.note.clone() {
                return Some(note);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        None
    }

    /// A worker that takes its time.
    ///
    /// The daemon can take seconds to answer, and the window that waits
    /// for it is a window that does not move. The worker remembers what
    /// it was asked, so a test can say how many questions went down.
    struct SlowWorker {
        /// How long the answer takes.
        delay: std::time::Duration,
        /// The questions that went down.
        asked: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl Worker for SlowWorker {
        fn answer(&mut self, request: Request) -> Option<Reply> {
            match request {
                Request::Who => {
                    std::thread::sleep(self.delay);
                    Some(Reply::Who {
                        messages: Vec::new(),
                        status: Status::default(),
                    })
                }
                Request::Search {
                    query, generation, ..
                } => {
                    self.asked.lock().unwrap().push(query.clone());
                    std::thread::sleep(self.delay);
                    Some(Reply::Search {
                        generation,
                        answer: Answer::empty(),
                        status: Status::default(),
                    })
                }
                // The session is not a question: it does not wait.
                Request::Session(_) => None,
            }
        }
    }

    /// The lock beside the store.
    ///
    /// The name is the store's own business, and the test finds it
    /// instead of spelling it: a test that spells the name of a file
    /// that belongs to another module breaks when that module changes
    /// its mind.
    fn lock_file(store: &std::path::Path) -> std::path::PathBuf {
        let dir = store.parent().expect("the store is in a folder");
        let mut beside: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
            .expect("the folder is there")
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path != store)
            .collect();
        beside.sort();
        beside
            .into_iter()
            .next()
            .expect("the lock is beside the store")
    }

    /// Hold the lock, the way another writer holds it.
    fn hold(path: &std::path::Path) -> std::fs::File {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .share_mode(0)
            .open(path)
            .expect("the lock is held")
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

    /// The icon shows the window, and leaves it up.
    ///
    /// A double click is two clicks: a window that opens and closes again
    /// is a window that is not there.
    #[test]
    fn the_icon_shows_the_window_and_leaves_it_up() {
        let mut state = empty();
        let control = handle(&mut state, Event::Show);
        assert!(
            matches!(control, Control::Show(..)),
            "the window goes up: {control:?}"
        );
        let control = handle(&mut state, Event::Show);
        assert!(
            matches!(control, Control::Show(..)),
            "the window stays up: {control:?}"
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
        assert_eq!(state.waiting, Waiting::Answer, "the answer is on its way");
    }

    #[test]
    fn the_answer_arrives_into_the_window_that_is_already_up() {
        // The clock is what the answer waits for: the window is up, and
        // the answer lands in it.
        let mut state = empty();
        state.app.insert('i');
        let _ = handle(&mut state, Event::Toggle);
        // The question goes down on the first tick, and the answer comes
        // back on the next one: the daemon was asked on another thread.
        let _ = handle(&mut state, Event::Tick);
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
        assert!(line.contains("searching"), "{line}");
    }

    #[test]
    fn the_status_line_says_the_daemon_is_not_there() {
        // The source without a daemon: the line says so, and says nothing
        // else that could be mistaken for an answer.
        let state = empty();
        let line = status_line(&state);
        assert!(line.contains("no connection"), "{line}");
    }

    /// A window nobody has asked anything says nothing about results.
    ///
    /// The first open showed "0 results" over the placeholder: the
    /// launcher claiming to have searched, on a field nobody had typed
    /// in. The count belongs to an answer, and there is none until a
    /// question has been answered.
    #[test]
    fn the_line_of_a_window_that_was_asked_nothing_says_nothing_about_results() {
        let mut state = empty();
        state.status.asked = false;
        state.status.connected = true;
        let line = status_line(&state);
        assert!(
            !line.contains("results"),
            "no question has been asked: {line:?}"
        );
        assert!(!line.contains("names"), "no layer has answered: {line:?}");

        // A question that was answered with nothing says "0 results":
        // that is the truth about a question that was asked.
        state.rows_query = Some("zebra".to_string());
        state.status.matches = 0;
        state.status.layer = Some(Layer::Names);
        let line = status_line(&state);
        assert!(line.contains("0 results"), "{line}");
    }

    /// The line under the field says the answer is not whole.
    ///
    /// The window used to show a short list with nothing saying the list
    /// was short: an answer that looks whole. The sentence comes from the
    /// daemon's own answer, and the line under the field carries it.
    #[test]
    fn the_status_line_says_the_answer_is_not_whole() {
        let mut state = empty();
        // What the daemon's answer carried.
        state.status.connected = true;
        state.status.warnings = vec!["the answer stops at the limit".to_string()];
        apply(&mut state, answer_with("C:/a.txt"));
        let line = status_line(&state);
        assert!(line.contains("the answer stops at the limit"), "{line}");
    }

    #[test]
    fn a_command_with_nothing_to_act_on_says_so() {
        // `>copy` in an empty window used to do nothing at all, and the
        // silence looked like a broken launcher.
        let mut state = empty();
        let _ = run_action(&mut state, Action::CopyPath);
        let line = status_line(&state);
        assert!(line.contains("no result"), "{line}");
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
        assert_eq!(state.waiting, Waiting::Answer, "the answer is on its way");
        assert!(asked.borrow().is_empty(), "the daemon is asked later");
        let _ = handle(&mut state, Event::Tick);
        assert_eq!(asked.borrow().len(), 1, "the daemon was asked once");
        assert_eq!(asked.borrow()[0], "invoice", "the daemon was asked");
    }

    /// The search does not take typing while the daemon is not there.
    ///
    /// A query nobody will answer is a query the user typed into a field that
    /// only fills up. The launcher knows the daemon is not there -- the answer
    /// said so -- so the characters are refused, and the line under the field
    /// says why.
    ///
    /// The control is the same key with the daemon there: the same key, the
    /// same window, the character lands. A rule that is not about the daemon
    /// is a rule that stops the typing always.
    #[test]
    fn the_search_does_not_take_typing_while_the_daemon_is_not_there() {
        let (mut state, _) = recording();
        state.status.asked = true;
        state.status.connected = false;
        let _ = on_key(&mut state, Key::Char('r'));
        assert_eq!(
            state.app.query().text(),
            "",
            "the character did not land: the daemon is not there"
        );

        // The control: the daemon is there, and the character lands.
        let (mut state, _) = recording();
        state.status.asked = true;
        state.status.connected = true;
        let _ = on_key(&mut state, Key::Char('r'));
        assert_eq!(state.app.query().text(), "r", "the character landed");

        // And the daemon nobody has asked about yet: the launcher does not
        // know, so it does not pretend the daemon is gone.
        let (mut state, _) = recording();
        let _ = on_key(&mut state, Key::Char('r'));
        assert_eq!(
            state.app.query().text(),
            "r",
            "the character landed: nobody has said the daemon is gone"
        );
    }

    /// The launcher waits a second between tries at a daemon that is away.
    ///
    /// The clock is 120 ms, and the launcher asked on every one of them:
    /// measured on 2026-11-04 with the daemon absent, that was a connection
    /// attempt 8.3 times a second — 0.26% of a core after the no-news paint
    /// was skipped, and 4.3% before it. A daemon that is not up will not be
    /// up in the next clock, so the retry backs off to one try a second,
    /// and a daemon that starts is still answered within a second.
    #[test]
    fn the_launcher_waits_between_tries_at_a_daemon_that_is_away() {
        let (mut state, _) = recording();
        state.status.asked = true;
        state.status.connected = false;

        // Seven clocks pass and the launcher asks nobody: the daemon is not
        // up, and the clock is not a reason to ask it again.
        for _ in 0..7 {
            let control = tick(&mut state);
            assert!(
                matches!(control, Control::None),
                "the launcher did not try again yet"
            );
        }
        // The eighth is a second after the last try, and it asks.
        let control = tick(&mut state);
        assert!(
            matches!(control, Control::Paint(..)),
            "the eighth clock tries the daemon again"
        );
        assert!(state.in_flight, "the question is on its way");

        // A daemon that answers takes the back-off with it: the next
        // question is a question, not a retry.
        state.in_flight = false;
        state.status.connected = true;
        state.retry = 0;
        assert_eq!(state.retry, 0, "a launcher with a daemon is not retrying");
    }

    /// The field can still be emptied while the daemon is not there.
    ///
    /// The user typed something before the launcher learned the daemon was
    /// gone, and a field that cannot be emptied is a field the user is stuck
    /// with. The paste is a typing too, and it is refused the same way.
    /// The command line still types while the daemon is away.
    ///
    /// The panel is the launcher's own and needs no daemon, and the moment
    /// the user wants it is exactly when the daemon is not there. Measured
    /// on 2026-11-04: with the daemon down, `>` was refused like any other
    /// character and `>settings` could not be typed at all.
    #[test]
    fn the_command_line_types_while_the_daemon_is_away() {
        let (mut state, _) = recording();
        state.status.asked = true;
        state.status.connected = false;

        // A `>` at the head of the field starts a command line, and the
        // rest of the word lands there too.
        let _ = on_key(&mut state, Key::Char('>'));
        assert_eq!(state.app.query().text(), ">", "the command prefix landed");
        for c in "settings".chars() {
            let _ = on_key(&mut state, Key::Char(c));
        }
        assert_eq!(state.app.query().text(), ">settings", "the word landed");

        // A character that is not part of the command line is refused: a
        // search is what the daemon is not there for.
        let (mut state, _) = recording();
        state.status.asked = true;
        state.status.connected = false;
        let _ = on_key(&mut state, Key::Char('x'));
        assert_eq!(
            state.app.query().text(),
            "",
            "a search is refused: the daemon is not there"
        );
    }

    #[test]
    fn the_field_can_still_be_emptied_while_the_daemon_is_not_there() {
        let (mut state, _) = recording_with("invoice");
        state.status.asked = true;
        state.status.connected = false;

        // The paste does not land.
        let _ = on_key(&mut state, Key::CtrlV);
        assert_eq!(
            state.app.query().text(),
            "",
            "the clipboard did not land: the daemon is not there"
        );

        // The characters that were typed before the launcher knew do not go
        // away by themselves, and the user can take them away.
        let (mut state, _) = recording();
        state.status.asked = true;
        state.status.connected = false;
        state.app.insert('i');
        state.app.insert('n');
        let _ = on_key(&mut state, Key::Backspace);
        assert_eq!(state.app.query().text(), "i", "the backspace worked");

        let _ = on_key(&mut state, Key::CtrlU);
        assert_eq!(state.app.query().text(), "", "the field was emptied");
    }

    /// The window keeps the meter's figures.
    ///
    /// The daemon says how long its answer took, and the window draws
    /// what it said: a window that shows a number it never measured is a
    /// window that reports a measurement it never took.
    #[test]
    fn the_answer_feeds_the_meter() {
        let (mut state, _) = recording_timed(8_100);
        let _ = on_key(&mut state, Key::Char('r'));
        // The question goes down on the clock, and the answer comes back
        // on the tick after it.
        let _ = handle(&mut state, Event::Tick);
        let _ = handle(&mut state, Event::Tick);
        let figures = view(&state).latency;
        assert_eq!(figures.last_us, Some(8_100));
        assert_eq!(
            figures.first_us,
            Some(8_100),
            "the first answer is the cold one"
        );
    }

    /// An answer that says nothing about how long it took is not a
    /// measurement.
    ///
    /// The fallback does not say, and a zero in the window would make the
    /// median zero — a number nobody can read.
    #[test]
    fn an_answer_with_no_time_is_not_a_measurement() {
        let (mut state, _) = recording_timed(0);
        let _ = on_key(&mut state, Key::Char('r'));
        let _ = handle(&mut state, Event::Tick);
        let _ = handle(&mut state, Event::Tick);
        assert!(view(&state).latency.is_empty());
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
        assert_eq!(state.waiting, Waiting::Answer, "the answer is on its way");
        let line = status_line(&state);
        assert!(line.contains("searching"), "{line}");
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

        let (history, note) = memory(Some(&path), Lang::English);
        assert!(history.is_empty(), "there is no history to show");
        let note = note.expect("the memory that could not be read is said");
        assert!(note.contains("history"), "{note}");

        // And the line under the field carries it.
        let mut state = empty();
        state.note = Some(note);
        let line = status_line(&state);
        assert!(line.contains("history"), "{line}");
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
        let (history, note) = memory(Some(&dir.join("usage.tsv")), Lang::English);
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
        state.note = Some("the global shortcut was not installed: no permission".to_string());
        let line = status_line(&state);
        assert!(line.contains("global shortcut"), "{line}");
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
            Some("the clipboard has no text")
        );
        assert!(!paste(&mut state, actions::Clipboard::Busy));
        assert_eq!(
            state.app.warnings().first().map(String::as_str),
            Some("the clipboard is busy")
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
        assert!(line.contains("nothing to paste"), "{line}");
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
        assert!(!line.contains("no result"), "{line}");
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
        state.open = Box::new(|_| Err("could not be opened".to_string()));
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
        assert!(line.contains("no result"), "{line}");
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
        assert!(!state.status.asked, "the daemon was not asked yet");

        // The window goes up.
        let _ = handle(&mut state, Event::Toggle);
        assert!(
            !state.status.asked,
            "the window goes up before the daemon is asked"
        );

        // And the clock asks. The answer comes back on the next tick:
        // the daemon was asked on another thread.
        let _ = handle(&mut state, Event::Tick);
        let _ = handle(&mut state, Event::Tick);
        assert!(
            state.status.asked,
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
        let (mut state, _) = recording();
        {
            let st = &mut state.status;
            st.asked = true;
            st.connected = false;
            st.matches = 3;
            st.layer = Some(Layer::Content);
            st.backend = "rgx".to_string();
            st.took_us = 1500;
        }
        // The answer is on the screen: the count belongs to it, and the
        // daemon not being there does not take the answer away.
        apply(&mut state, answer_with("C:/a.txt"));
        state.status.matches = 3;
        let line = status_line(&state);
        assert!(line.contains("3 results"), "{line}");
        assert!(line.contains("no connection"), "the daemon is gone: {line}");
        assert!(line.contains("content"), "{line}");
        assert!(line.contains("rgx"), "{line}");
        // The time is not in the line: it is the meter's, and the meter
        // draws it in green at the right of the query. A time that is in
        // both places is a number the user reads twice.
        assert!(!line.contains("ms"), "{line}");
    }

    /// The line under the field describes the rows that are on the screen.
    ///
    /// The field is emptied: the list goes with it, and so does the line.
    /// A line that says "37 resultados" over an empty list is a line about
    /// another answer — and "0 results" is no better: nobody asked.
    #[test]
    fn the_line_does_not_describe_an_answer_that_is_gone() {
        let (mut state, _) = recording();
        {
            let st = &mut state.status;
            st.matches = 37;
            st.layer = Some(Layer::Content);
            st.backend = "rgx".to_string();
            st.took_us = 1500;
        }
        cleared(&mut state);
        let line = status_line(&state);
        assert!(
            !line.contains("results"),
            "the answer is gone and so is the count: {line}"
        );
        assert!(!line.contains("37"), "{line}");
        assert!(!line.contains("content"), "{line}");
        assert!(!line.contains("rgx"), "{line}");
        assert!(!line.contains("1 ms"), "{line}");
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
        assert!(!line.contains("no connection to the daemon"), "{line}");
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
        assert_eq!(state.waiting, Waiting::Answer, "the answer is on its way");
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
        let (mut state, _) = recording();
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

        // The write is on another thread, and the note comes when it is
        // over.
        let note = wait_for_note(&mut state).expect("the note arrives");
        assert!(note.contains("history"), "{note}");
        let line = status_line(&state);
        assert!(line.contains("history"), "{line}");
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

    /// A letter typed while the menu is open is the field's.
    ///
    /// The menu has the keyboard for the keys that choose an action, and a
    /// letter is not one of them: the user who opens the menu and starts
    /// typing is asking a new question. The letter used to be swallowed —
    /// a key that does nothing and says nothing, which is a launcher the
    /// user cannot tell from a frozen one.
    #[test]
    fn a_letter_typed_in_the_menu_is_the_field_s() {
        let (mut state, asked) = recording();
        state
            .app
            .set_results(vec![Row::new("C:/a/notas.txt".to_string(), 0)]);
        let _ = on_key(&mut state, Key::CtrlK);
        assert_eq!(state.app.mode(), Mode::Menu, "the menu is open");

        let _ = on_key(&mut state, Key::Char('i'));

        assert_eq!(state.app.query().text(), "i", "the letter landed");
        assert_ne!(state.app.mode(), Mode::Menu, "the menu is closed");
        assert_eq!(state.waiting, Waiting::Answer, "the answer is on its way");
        assert!(asked.borrow().is_empty(), "the daemon is asked later");
    }

    /// The actions' own keys work while the menu is open.
    ///
    /// The menu lists the actions and the key that does each one, and the
    /// key still does it: the menu is where the actions are, not a wall
    /// between the user and them.
    #[test]
    fn the_actions_own_keys_still_work_in_the_menu() {
        let (mut state, _asked) = recording();
        state
            .app
            .set_results(vec![Row::new("C:/a/notas.txt".to_string(), 0)]);
        let _ = on_key(&mut state, Key::CtrlK);
        assert_eq!(state.app.mode(), Mode::Menu, "the menu is open");

        let _ = on_key(&mut state, Key::CtrlP);

        assert!(
            state.app.preview().is_some(),
            "the preview did not open from the menu"
        );
        assert_ne!(state.app.mode(), Mode::Menu, "the menu is closed");
    }

    /// The command bar opens the settings panel.
    ///
    /// The settings are nobody's row: `>settings` opens the panel with
    /// nothing selected, the way the commands are a question nobody's
    /// result has to answer.
    #[test]
    fn the_command_bar_opens_the_settings_panel() {
        let (mut state, _asked) = recording();
        for word in [">settings"] {
            for c in word.chars() {
                let _ = on_key(&mut state, Key::Char(c));
            }
        }
        let _ = on_key(&mut state, Key::Enter);
        assert_eq!(state.app.mode(), Mode::Settings, "the panel is open");
        assert_eq!(state.app.settings_index(), 0, "the first row is selected");
    }

    /// The settings panel has the keyboard for the keys that adjust it,
    /// and a letter is not one of them.
    ///
    /// The rule is the menu's: the user who opens the panel and starts
    /// typing is asking a new question, and the panel is in the way of
    /// it. The panel closes and the letter lands.
    #[test]
    fn a_letter_typed_in_the_settings_panel_is_the_field_s() {
        let (mut state, asked) = recording();
        state.app.open_settings();
        let _ = on_key(&mut state, Key::Char('i'));
        assert_eq!(state.app.query().text(), "i", "the letter landed");
        assert_ne!(state.app.mode(), Mode::Settings, "the panel is closed");
        assert_eq!(state.waiting, Waiting::Answer, "the answer is on its way");
        assert!(asked.borrow().is_empty(), "the daemon is asked later");
    }

    /// The arrows adjust the setting they are on, and the adjustment is
    /// the state's own.
    ///
    /// The panel's promise is that the arrow lands at once: the width
    /// the model carries is the width the file will say, and the hotkey
    /// the settings name is the one that would be armed (a test has no
    /// window to arm it on, and the settings still carry the answer).
    #[test]
    fn the_arrows_adjust_the_setting_they_are_on() {
        let (mut state, _asked) = recording();
        state.app.open_settings();
        // The first row is the theme: a step forward names the next
        // template, and a step back the one before it.
        let _ = on_key(&mut state, Key::Right);
        assert_eq!(
            state.settings.overlay.palette.template, "midnight",
            "the step names the next template"
        );
        let _ = on_key(&mut state, Key::Left);
        assert_eq!(
            state.settings.overlay.palette.template, "warm",
            "the step back returns the template"
        );
        // The width is the second row: the model carries it too, because
        // the model is what the window is sized from.
        let _ = on_key(&mut state, Key::Down);
        let _ = on_key(&mut state, Key::Right);
        assert_eq!(state.settings.overlay.width, 620, "the width stepped");
        assert_eq!(state.app.width(), 620, "the model carries the width");
        // The hotkey is the seventh row: a step takes the double-tap
        // shape and names the next modifier.
        state.app.open_settings();
        for _ in 0..6 {
            let _ = on_key(&mut state, Key::Down);
        }
        let _ = on_key(&mut state, Key::Right);
        assert_eq!(
            state.settings.overlay.hotkey,
            Hotkey::DoubleTap {
                key: TapKey::Alt,
                window_ms: 400
            },
            "the hotkey stepped to the next modifier"
        );
    }

    /// The defaults row restores the faces as they were.
    #[test]
    fn the_defaults_row_restores_the_faces_as_they_were() {
        let (mut state, _asked) = recording();
        state.settings.overlay.width = 900;
        state.settings.overlay.palette.template = "light".to_string();
        state.app.open_settings();
        // The last row is the defaults: any step on it restores them.
        for _ in 0..SETTINGS.len() - 1 {
            let _ = on_key(&mut state, Key::Down);
        }
        let _ = on_key(&mut state, Key::Enter);
        assert_eq!(
            state.settings,
            rgy_launcher::settings::Settings::default(),
            "the settings are the faces as they were"
        );
    }

    /// The launcher says the machine's news once, and not once a tick.
    ///
    /// The launcher asks about the daemon on the clock, and the clock
    /// ticks every hundred and twenty milliseconds: a message said on
    /// every tick is a flood, and a flood fills the machine's log — and
    /// the pipe of a launcher whose reader is slow. The stderr carries
    /// the message once, when it changes; the window carries it the whole
    /// time, in the line under the field.
    ///
    /// Measured on 2026-10-31: the launcher wrote the same sentence eight
    /// times a second, and a launcher whose stderr was a pipe that nobody
    /// read filled that pipe in a second and then **stopped answering** —
    /// the window did not take a message for two seconds.
    #[test]
    fn the_launcher_says_the_news_once() {
        let mut said = Vec::new();
        let news = vec![String::from("no daemon in pipe:rgy-searchd")];

        assert_eq!(
            said_once(&mut said, &news),
            Some(vec![String::from("rgxy: no daemon in pipe:rgy-searchd")]),
            "the news is said the first time"
        );

        // The same news on the next tick: there is nothing new to say.
        assert_eq!(said_once(&mut said, &news), None, "the news was said again");

        // Other news: the stderr carries it.
        assert_eq!(
            said_once(&mut said, &[String::from("the daemon is slow")]),
            Some(vec![String::from("rgxy: the daemon is slow")]),
            "the new news is said"
        );

        // And back to the first: it changed, so the stderr carries it.
        assert!(
            said_once(&mut said, &news).is_some(),
            "the news that changed is said again"
        );
    }

    /// The menu says that it has the keyboard.
    ///
    /// The key moved the keyboard to the menu: the arrows run an action and
    /// not the list. A key that moves the selection somewhere else and
    /// says nothing is a key the user cannot tell from a frozen one, and
    /// the line under the field is where the launcher says what is
    /// happening.
    ///
    /// And the word is not there the rest of the time: a line that says it
    /// always says nothing.
    #[test]
    fn the_menu_says_that_it_has_the_keyboard() {
        let mut state = empty();
        state
            .app
            .set_results(vec![Row::new("C:/a/notas.txt".to_string(), 0)]);
        let closed = status_line(&state);
        assert!(
            !closed.contains(Text::for_lang(state.lang).menu_has_the_keyboard),
            "{closed}"
        );

        let _ = on_key(&mut state, Key::CtrlK);
        let open = status_line(&state);
        assert!(
            open.contains(Text::for_lang(state.lang).menu_has_the_keyboard),
            "{open}"
        );
    }

    /// The window does not wait for the daemon.
    ///
    /// The daemon takes three seconds to answer, and the window that
    /// waits for it is a window that does not move and does not take
    /// keys: the question goes down a channel and the clock returns.
    #[test]
    fn the_window_does_not_wait_for_the_daemon() {
        let (mut state, _) = recording();
        state.source = Box::new(Daemon::with_worker(|| SlowWorker {
            delay: std::time::Duration::from_secs(3),
            asked: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }));
        state.app.insert('i');
        let _ = handle(&mut state, Event::Toggle);

        let started = std::time::Instant::now();
        let _ = handle(&mut state, Event::Tick);
        let waited = started.elapsed();
        assert!(
            waited < std::time::Duration::from_secs(1),
            "the window waited for the daemon: {waited:?}"
        );
        // And the answer is not there yet: it is on its way.
        assert!(
            state.app.results().is_empty(),
            "the answer is still on its way"
        );
    }

    /// A keystroke does not start a second question.
    ///
    /// A keystroke typed while the answer is in flight does not ask the
    /// daemon anything: the question is asked on the clock, and the
    /// clock waits for the question that is already on its way.
    #[test]
    fn a_keystroke_does_not_start_a_second_question() {
        let (mut state, asked) = recording();
        let _ = on_key(&mut state, Key::Char('i'));
        let _ = handle(&mut state, Event::Tick);
        assert_eq!(asked.borrow().len(), 1, "the daemon was asked once");
        assert!(state.in_flight, "the question is on its way");

        // The user types while the answer is on its way.
        let _ = on_key(&mut state, Key::Char('n'));
        assert_eq!(
            asked.borrow().len(),
            1,
            "the keystroke did not start a second question"
        );
        assert!(state.in_flight, "the question is still on its way");
    }

    /// The question that is on its way is not asked a second time.
    ///
    /// The daemon is still answering when the clock ticks again, and a
    /// second question would be a second daemon waiting on the same
    /// pipe.
    #[test]
    fn a_question_that_is_on_its_way_is_not_asked_twice() {
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = std::sync::Arc::clone(&asked);
        let (mut state, _) = recording();
        state.source = Box::new(Daemon::with_worker(move || SlowWorker {
            delay: std::time::Duration::from_secs(3),
            asked: std::sync::Arc::clone(&recorder),
        }));
        state.app.insert('i');
        let _ = handle(&mut state, Event::Toggle);
        let _ = handle(&mut state, Event::Tick);
        // The worker has the question; the answer is three seconds away.
        for _ in 0..100 {
            if !asked.lock().unwrap().is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // The clock ticks again, and the question is not asked a second
        // time.
        let _ = handle(&mut state, Event::Tick);
        let _ = handle(&mut state, Event::Tick);
        let asked = asked.lock().unwrap();
        assert_eq!(asked.len(), 1, "the daemon was asked once: {asked:?}");
        assert_eq!(asked[0], "i", "the daemon was asked");
    }

    /// An answer for a question the user moved on from is dropped.
    ///
    /// The rows on the screen are the answer to a question, and a
    /// question the user has moved on from is not the one the answer is
    /// for.
    #[test]
    fn an_answer_for_a_question_the_user_moved_on_from_is_dropped() {
        let (mut state, asked) = recording();
        let _ = on_key(&mut state, Key::Char('i'));
        // The question goes down.
        let _ = handle(&mut state, Event::Tick);
        assert_eq!(asked.borrow().len(), 1, "the daemon was asked once");
        assert_eq!(asked.borrow()[0], "i", "the daemon was asked");

        // The user types while the answer is on its way: the second
        // keystroke does not start a second question, and the answer to
        // the first one is dropped when it comes back.
        let _ = on_key(&mut state, Key::Char('n'));
        let _ = handle(&mut state, Event::Tick);
        assert_eq!(
            asked.borrow().len(),
            2,
            "the question on the screen is asked again"
        );
        assert_eq!(asked.borrow()[1], "in", "the question on the screen");
        assert!(
            state.app.results().is_empty(),
            "the answer to another question is not shown"
        );

        // And the answer to the question on the screen lands.
        let _ = handle(&mut state, Event::Tick);
        assert!(
            !state.app.results().is_empty(),
            "the answer to the question on the screen is on the screen"
        );
    }

    /// The rows on the screen say which question they answer.
    ///
    /// A keystroke typed while the answer is in flight leaves the list
    /// showing the results of the question before it, and the line says
    /// which question that is.
    #[test]
    fn the_rows_on_the_screen_say_which_question_they_answer() {
        let (mut state, _) = recording();
        let _ = on_key(&mut state, Key::Char('i'));
        // The question goes down, and the answer comes back.
        let _ = handle(&mut state, Event::Tick);
        let _ = handle(&mut state, Event::Tick);
        assert!(
            !state.app.results().is_empty(),
            "the answer is on the screen"
        );

        // The user types: the rows on the screen answer another
        // question, and the line says which one.
        let _ = on_key(&mut state, Key::Char('n'));
        let line = status_line(&state);
        assert!(line.contains("of \"i\""), "the line marks the rows: {line}");
    }

    /// The note about the machine is one note.
    ///
    /// The note is about the machine, and a note that grows with every
    /// use pushes the answer off the line.
    #[test]
    fn the_note_about_the_machine_is_one_note() {
        let mut state = empty();
        state.note = Some("the global shortcut was not installed: no permission".to_string());
        state.note = Some("the history was not saved: the disk is full".to_string());
        let line = status_line(&state);
        assert!(line.contains("history"), "{line}");
        assert!(!line.contains("global shortcut"), "{line}");
    }

    /// The worker that asks the daemon is gone.
    ///
    /// The question did not go down, so no answer is coming: the
    /// launcher is not waiting for one, and it says why. A launcher that
    /// waits for an answer that cannot come is a launcher that stopped
    /// answering the keyboard, and one that says nothing about it is a
    /// launcher the user cannot tell from a frozen one.
    #[test]
    fn a_worker_that_is_gone_is_said_out_loud() {
        let (mut state, _) = recording();
        state.source = Box::new(Daemon::dead());
        state.app.insert('i');
        let _ = handle(&mut state, Event::Toggle);

        let _ = handle(&mut state, Event::Tick);
        assert!(
            !state.in_flight,
            "the launcher is not waiting for an answer that cannot come"
        );
        assert_eq!(state.waiting, Waiting::Idle, "the launcher is not looking");
        let line = status_line(&state);
        assert!(line.contains("died"), "the launcher says why: {line}");
        assert!(
            !line.contains("searching"),
            "the launcher is not looking for an answer: {line}"
        );
    }

    /// The preview does not wait for the file.
    ///
    /// The file may be on a share that is not there, and the read blocks
    /// for the SMB timeout: the window that waits for it is a window
    /// that does not move. The read goes to a thread of its own, the
    /// panel says it is waiting, and the preview lands when the read is
    /// over.
    #[test]
    fn the_preview_does_not_wait_for_the_file() {
        let (mut state, _) = recording();
        let dir = scratch("preview");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the folder is there");
        let file = dir.join("notas.txt");
        std::fs::write(&file, "zebra\n").expect("the file is written");
        state
            .app
            .set_results(vec![Row::new(file.to_string_lossy().into_owned(), 0)]);
        let _ = run_action(&mut state, Action::Preview);

        // The panel says it is waiting: the read has not come back yet.
        let panel = state.app.preview().expect("the panel is there");
        assert!(
            panel.lines.join(" ").contains("reading"),
            "the panel says it is waiting: {panel:?}"
        );

        // And the preview lands when the read is over.
        let mut landed = false;
        for _ in 0..200 {
            take_preview(&mut state);
            if let Some(panel) = state.app.preview()
                && panel.lines.join(" ").contains("zebra")
            {
                landed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(landed, "the preview landed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The launcher asks again while the daemon is away.
    ///
    /// The daemon comes up after the window opened: the launcher has to
    /// notice, because the line that says the daemon is not there is a
    /// line about the daemon that is not there. The asking backs off to
    /// one try a second (`RETRY_TICKS` clocks): a launcher that opens a
    /// connection every clock is a launcher doing 8.3 round trips a second
    /// for nothing — measured on 2026-11-04, with the daemon away.
    #[test]
    fn the_launcher_asks_again_while_the_daemon_is_away() {
        let (mut state, _asked) = recording();
        // The daemon was asked and it was not there.
        state.status.asked = true;
        state.status.connected = false;
        // The window is up, and there is no question to ask.
        state.ask_daemon = false;
        state.waiting = Waiting::Idle;

        // The first clock asks nobody: the wait between tries is the
        // point of the back-off.
        let _ = handle(&mut state, Event::Tick);
        assert!(
            !state.in_flight,
            "the first clock does not try again: the launcher just tried"
        );
        let mut asked = false;
        for _ in 0..RETRY_TICKS {
            let _ = handle(&mut state, Event::Tick);
            if state.in_flight {
                asked = true;
                break;
            }
        }
        assert!(
            asked,
            "the launcher asks again within the second: the daemon may have come up"
        );
    }

    /// The launcher does not keep asking a daemon that is there.
    ///
    /// The question is asked when there is one: a launcher that asks
    /// the daemon about itself every 120 ms is a launcher that burns a
    /// machine that is answering.
    #[test]
    fn the_launcher_does_not_keep_asking_a_daemon_that_is_there() {
        let (mut state, _asked) = recording();
        state.status.asked = true;
        state.status.connected = true;
        state.ask_daemon = false;
        state.waiting = Waiting::Idle;

        let _ = handle(&mut state, Event::Tick);
        assert!(!state.in_flight, "the launcher is not asking again");
    }

    /// The window does not wait for the store.
    ///
    /// The store is behind a lock, and the lock is held by another
    /// writer: the write waits for it, and the launcher that opened the
    /// result is already gone.
    #[test]
    fn the_window_does_not_wait_for_the_store() {
        let (mut state, _) = recording();
        let dir = scratch("gui-store");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the folder is there");
        let store = dir.join("usage.tsv");
        // One write, so that the lock is there to be held.
        mru::save(&store, &History::new()).expect("the store is written");
        // The lock is held: the write cannot be made, and the writer
        // waits for it.
        let held = hold(&lock_file(&store));

        state.usage_path = Some(store);
        state
            .app
            .set_results(vec![Row::new("C:/a/notas.txt".to_string(), 0)]);
        state.open = Box::new(|_| Ok(()));

        let started = std::time::Instant::now();
        let _ = run_action(&mut state, Action::Open);
        let waited = started.elapsed();
        assert!(
            waited < std::time::Duration::from_millis(200),
            "the window waited for the store: {waited:?}"
        );
        // And the window is already down.
        assert!(!state.app.is_visible(), "the window went down first");

        // The write gives up, and the launcher says so.
        let note = wait_for_note(&mut state).expect("the write is over");
        assert!(note.contains("history"), "{note}");
        drop(held);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The view shows the actions of the selected row.
    ///
    /// The column on the screen is the list the row offers, and a folder
    /// is not previewed: a column that offers the preview for a folder
    /// offers a key that fails.
    #[test]
    fn the_view_shows_the_actions_of_the_selected_row() {
        let mut state = empty();
        state
            .app
            .set_results(vec![Row::new("C:/a/foto.png".to_string(), 0)]);
        let shown = view(&state);
        assert!(!shown.menu.contains(&Action::Preview), "{:?}", shown.menu);
        state
            .app
            .set_results(vec![Row::new("C:/a/notas.txt".to_string(), 0)]);
        let shown = view(&state);
        assert!(shown.menu.contains(&Action::Preview), "{:?}", shown.menu);
    }

    /// The preview key on a folder says why.
    ///
    /// The key used to open a preview of a folder, and the preview is the
    /// head of a file as text — a folder has no head. The refusal is in
    /// the line under the field, and no panel opens.
    #[test]
    fn the_preview_key_on_a_folder_says_why() {
        let mut state = empty();
        state.app.set_results(vec![
            Row::new("C:/a/pasta".to_string(), 0).with_folder(true),
        ]);
        let _ = on_key(&mut state, Key::CtrlP);
        let line = status_line(&state);
        assert!(line.contains("folder"), "{line}");
        assert!(line.contains("no preview"), "{line}");
        assert!(state.app.preview().is_none(), "no preview was opened");
        assert!(
            state.preview_pending.is_none(),
            "and no file was sent to be read"
        );
    }
}
