// SPDX-License-Identifier: Apache-2.0

//! The daemon's answers.
//!
//! The launcher is a **client**: the index lives in the daemon, and a
//! keystroke is a question sent down a pipe. This is the half of the
//! launcher that asks.
//!
//! # The two layers
//!
//! | layer | what the daemon is asked | what it costs |
//! |---|---|---|
//! | names | `search` | a re-rank of a warm index |
//! | content | `grep-scoped`, over the names' paths | reading the files that were chosen |
//!
//! The content layer's **scope** is the name layer's answer: the files are
//! already chosen, and the question is what is inside them. That is the
//! whole performance story of the second layer — a content search over the
//! tree reads the tree, and a content search over two hundred chosen files
//! reads two hundred files.
//!
//! # The connection
//!
//! The client is opened once and kept. A daemon that is not there yet is
//! not a failure: the launcher is a resident program, and the daemon may
//! start a second later — so the connection is made on demand and retried
//! on the next keystroke.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use rgy_launcher::text::{Lang, Text};
use rgy_launcher::{Answer, Layer, Query, Source};
use rgy_proto::client::Client;
use rgy_proto::endpoint::Endpoint;

/// How many names the content layer reads.
///
/// The scope is a cap, not a promise: the content layer reads the files
/// the name layer chose, and a hundred of them is already more than a
/// person is going to look at.
const SCOPE_LIMIT: usize = 64;

/// The window the launcher gives the daemon to answer in.
///
/// The client's own window is thirty seconds — generous for a program
/// that waits for an answer in the background, and far too long for a
/// search box: thirty seconds of silence is a window that stopped
/// answering the keyboard. Two seconds is long enough for the daemon to
/// answer a warm index (which is microseconds) and for a cold one to be
/// worth waiting for, and short enough that the launcher is still a
/// launcher when the daemon is slow or stuck.
///
/// The launcher asks on a thread of its own, so the window never waits
/// for this budget at all: the budget is what the **worker** gives up.
pub const LAUNCHER_WINDOW: Duration = Duration::from_secs(2);

/// The window the count is asked in.
///
/// The count is a fact about the daemon's index and not the question the
/// user asked, so it gets a window of its own: a daemon that does not
/// answer it in a quarter of a second leaves the count unknown, and an
/// unknown count is a thing the header can say — which is better than a
/// zero that claims an index nobody saw.
const COUNT_WINDOW: Duration = Duration::from_millis(250);

/// What the launcher shows about the last answer.
///
/// It is shared with the window through a cell, because the window reads
/// it *after* the search is over — the answer and the words about the
/// answer are two different moments.
#[derive(Debug, Clone, Default)]
pub struct Status {
    /// The warnings the last answer carried.
    pub warnings: Vec<String>,
    /// How long the daemon took, in microseconds.
    pub took_us: u64,
    /// Which layer answered: the names, or the content.
    ///
    /// `None` when nothing has answered yet: the line under the field
    /// names the layer of the last answer, and a layer of an answer that
    /// never came is a layer the user did not ask about.
    pub layer: Option<Layer>,
    /// Where the content layer's answers come from.
    pub backend: String,
    /// Whether the daemon is there.
    pub connected: bool,
    /// Whether the daemon has been asked at all.
    ///
    /// The line says "no connection to the daemon" about a daemon that is not
    /// there, and not about a daemon nobody has asked about yet.
    pub asked: bool,
    /// Whether the daemon has a pack store (`--tv-dir`).
    pub tv: bool,
    /// How many entries the daemon's index holds.
    ///
    /// It is the daemon's own number — the one its status carries — and
    /// not a count the client made: the client does not walk the tree
    /// and cannot know the size of an index it does not have.
    ///
    /// `None` when the daemon has not said: a daemon that is not there
    /// has no count, and a zero the client invented for it would claim
    /// an empty index nobody ever saw.
    pub indexed: Option<usize>,
    /// How many results there are.
    pub matches: usize,
}

/// Whether the daemon is there.
///
/// The three are three different facts about the machine: a daemon that
/// could not be reached is gone, a daemon that took the connection and
/// said nothing in the window is up and slow, and a daemon that took
/// the connection and answered badly is up — it answered, and what it
/// said was wrong. The line that says "no connection to the daemon" under
/// a daemon that is up is a lie.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Presence {
    /// The daemon is not there at all.
    Gone,
    /// The daemon is there and did not answer in the window.
    Slow,
    /// The daemon is there and answered badly.
    Answered,
}

/// Why there is no answer.
///
/// The sentence is what the user reads, and the presence says what the
/// failure is: a daemon that did not answer in the window is up and
/// slow, a daemon that answered badly is up, and a daemon that is not
/// there is not there.
struct Failure {
    /// The sentence for the user.
    reason: String,
    /// Whether the daemon is there.
    presence: Presence,
}

impl Failure {
    /// The question the daemon did not answer.
    ///
    /// The daemon's own sentence is kept: it says what happened, in the
    /// daemon's words.
    fn of(reason: &str, e: &std::io::Error) -> Failure {
        Failure {
            reason: format!("{reason}: {e}"),
            presence: presence(e),
        }
    }

    /// The daemon is not there.
    fn gone(reason: String) -> Failure {
        Failure {
            reason,
            presence: Presence::Gone,
        }
    }

    /// The window ran out before the answer did.
    ///
    /// The launcher's own budget is gone, and the daemon may well be there
    /// and busy. Only a daemon that could not be **reached** is gone, and
    /// the window running out is not the daemon being gone: a launcher that
    /// says "no connection to the daemon" about a daemon that is up is
    /// saying a lie, and it drops a connection that is still good.
    fn slow(reason: String) -> Failure {
        Failure {
            reason,
            presence: Presence::Slow,
        }
    }
}

/// Whether the daemon is there, told by what went wrong.
///
/// Only a daemon that could not be **reached** is gone: the pipe that
/// is not there, and the pipe that was closed. Everything else — the
/// daemon's own error, a line that is not an answer, an answer that
/// stops in the middle — is a daemon that IS there and answered badly,
/// and the line that says "no connection to the daemon" about it is a lie.
fn presence(e: &std::io::Error) -> Presence {
    match e.kind() {
        std::io::ErrorKind::TimedOut => Presence::Slow,
        // The daemon that is not there: the pipe that is not there, the
        // pipe that was closed, and the socket that refuses the
        // connection because nobody is listening on it. A socket file
        // left behind by a daemon that died is the common case, and a
        // launcher that calls it "connected" is a launcher that hides
        // the daemon the user has to start.
        std::io::ErrorKind::BrokenPipe
        | std::io::ErrorKind::NotFound
        | std::io::ErrorKind::ConnectionRefused
        | std::io::ErrorKind::ConnectionReset
        | std::io::ErrorKind::ConnectionAborted => Presence::Gone,
        _ => Presence::Answered,
    }
}

/// The accessors the window's worker needs.
///
/// The window is the only reader: the platforms that have no window do
/// not compile the module that reads them, and a method with no reader
/// is a warning the gate fails on.
#[cfg(windows)]
impl DaemonSource {
    /// Where the daemon is asked.
    ///
    /// The window's worker builds the source that answers, and it needs
    /// to know where to ask: the source the caller made is only a
    /// description of the daemon.
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// The `rgx` binary, when the machine has one.
    pub fn rgx(&self) -> Option<&str> {
        self.rgx.as_deref()
    }

    /// The status, as a copy.
    ///
    /// The copy is what travels to the thread that draws the window:
    /// the status is about the answer, and the answer and the words
    /// about it are two different moments.
    pub fn snapshot(&self) -> Status {
        (*self.status.borrow()).clone()
    }
}

/// The answers, from the daemon.
pub struct DaemonSource {
    /// The connection, when there is one.
    client: Option<Client>,
    /// Where the daemon listens.
    endpoint: Endpoint,
    /// The session the daemon knows this window by.
    session: Option<u64>,
    /// The last answer, kept so an empty pattern does not empty the list.
    last: Answer,
    /// The paths of the last name answer.
    ///
    /// This is the scope the `rgx` fallback searches: the files that were
    /// on the screen when the daemon went away. It is never the tree.
    last_scope: Vec<String>,
    /// What the window shows about the answer.
    status: Rc<RefCell<Status>>,
    /// The `rgx` binary, when the machine has one.
    ///
    /// It is the fallback for the content layer: the daemon carries the
    /// same adapters, and when the daemon is not there the binary is
    /// what is left.
    rgx: Option<String>,
    /// The window the caller gives the daemon to answer in.
    ///
    /// The window is the **caller's** and not the source's: the window
    /// that draws cannot wait the client's thirty seconds, and the
    /// command line — which waits in the background — can.
    window: Duration,
    /// The language the launcher is speaking.
    ///
    /// The source says sentences — to the window and to the terminal — and
    /// a sentence is said in the language the user reads.
    lang: Lang,
}

impl DaemonSource {
    /// A source that asks the daemon at `endpoint`.
    ///
    /// The window is the **launcher's**: two seconds, which is what a
    /// search box — the window or the terminal interface — can afford to
    /// wait. The command line asks for the client's own window with
    /// [`DaemonSource::with_window`].
    pub fn new(endpoint: Endpoint, rgx: Option<String>, lang: Lang) -> DaemonSource {
        DaemonSource::with_window(endpoint, rgx, LAUNCHER_WINDOW, lang)
    }

    /// The same, with the window the caller asks for.
    ///
    /// The window belongs to the caller: the launcher gives the daemon
    /// two seconds and the command line gives it the client's thirty,
    /// because a program that waits in the background can afford to and
    /// a search box cannot.
    pub fn with_window(
        endpoint: Endpoint,
        rgx: Option<String>,
        window: Duration,
        lang: Lang,
    ) -> DaemonSource {
        DaemonSource {
            client: None,
            endpoint,
            session: None,
            last: Answer::empty(),
            last_scope: Vec::new(),
            status: Rc::new(RefCell::new(Status::default())),
            rgx,
            window,
            lang,
        }
    }

    /// The language the launcher is speaking.
    ///
    /// The source says sentences, and the caller that draws them needs to
    /// know in which language they are said.
    ///
    /// The caller is the window, and the window is Windows-only for now:
    /// everywhere else the terminal interface is the launcher, and it is
    /// given the language by the hand that read it. The accessor is
    /// compiled everywhere and called where the window is.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn lang(&self) -> Lang {
        self.lang
    }

    /// The window this source gives the daemon to answer in.
    ///
    /// The window is a property of the caller, and the caller that sets
    /// it is the one that reads it.
    ///
    /// The wait for a daemon that is still starting reads it on every
    /// platform: a wait that does not know what its own budget is cannot
    /// give the daemon what is left of it.
    pub fn window(&self) -> Duration {
        self.window
    }

    /// What the window shows about the last answer.
    pub fn status(&self) -> Rc<RefCell<Status>> {
        Rc::clone(&self.status)
    }

    /// Why the content layer has no answer when the daemon is not there.
    ///
    /// The `rgx` binary is the content layer's fallback, and the fallback
    /// needs a **scope**: the files the daemon's name answer chose. With
    /// no daemon there was no name answer, so there is no scope and the
    /// fallback has nothing to read — the sentence says that instead of
    /// promising a search that cannot happen.
    fn no_content_reason(&self) -> &'static str {
        let text = Text::for_lang(self.lang);
        if self.rgx.is_some() {
            text.rgx_needs_a_name_scope
        } else {
            text.content_needs_the_daemon
        }
    }

    /// The sentence the host shows when there is no daemon.
    ///
    /// The launcher is resident and the daemon may come up later, so the
    /// host says where the daemon is expected — and what the content layer
    /// can do without it, which is: nothing yet. A message that promises
    /// the `rgx` fallback promises a search that needs a scope, and
    /// without the daemon there is no scope to have.
    pub fn no_daemon_message(&self) -> String {
        rgy_launcher::text::fill(
            Text::for_lang(self.lang).no_daemon_in,
            &[&self.endpoint.describe(), self.no_content_reason()],
        )
    }

    /// Use this session id from now on.
    ///
    /// A new window is a new session: the previous window's result set is
    /// not the answer to a new question.
    #[cfg(windows)]
    pub fn set_session(&mut self, id: u64) {
        self.session = Some(id);
        if let Some(client) = self.client.as_mut() {
            client.set_session(Some(id));
        }
    }

    /// Connect, and say what the daemon says about itself.
    ///
    /// The status is what tells the window whether there is a pack store:
    /// the content layer answers differently with one, and the user is
    /// told which.
    ///
    /// The window is the **caller's**: a daemon that takes the connection
    /// and then says nothing must not hold the window off the screen.
    ///
    /// `false` when the daemon is not there at all — the connection
    /// failed. A daemon that is there and slow is not a daemon that is
    /// gone, and the answer says so.
    pub fn connect(&mut self) -> bool {
        let client = match Client::connect(&self.endpoint) {
            Ok(c) => c,
            Err(e) => {
                let mut st = self.status.borrow_mut();
                st.asked = true;
                st.connected = false;
                st.warnings = vec![format!("{}: {e}", rgy_launcher::text::no_daemon(self.lang))];
                return false;
            }
        };
        self.client = Some(client);
        if let Some(client) = self.client.as_mut() {
            client.set_session(self.session);
        }
        // The status is a separate question: a daemon that answers it is
        // a daemon that is up.
        let status = match self.client.as_mut() {
            Some(client) => client.status_within(self.window),
            None => return false,
        };
        match status {
            Ok(status) => {
                let mut st = self.status.borrow_mut();
                st.asked = true;
                st.connected = true;
                st.tv = status.tv;
                // The count is the daemon's own: how many entries its
                // index holds. The terminal's header shows it, and the
                // header would say "0" about an index of a thousand
                // entries if the client answered for a daemon that did
                // not.
                st.indexed = Some(status.docs);
                // The warning of the failure that is over: a daemon that
                // is there does not carry the sentence that said it was
                // not.
                st.warnings.clear();
                // The daemon says whether the tree is still followed. A
                // feed that died leaves it answering on a snapshot, and a
                // window that says "the daemon is there" is telling the
                // user that a live tree is behind it.
                if let Some(why) = feed_warning(status.feed, self.lang) {
                    st.warnings.push(why);
                }
                // The status answer carries two more facts — where the
                // journal is, and whether the index caught up by
                // replaying the journal or by walking the tree again —
                // and the window does not draw them.
                //
                // It does not draw them because there is no word for them:
                // the vocabulary is the launcher's
                // (`crates/launcher/src/text.rs`), it speaks two languages,
                // and a sentence written here would be English-only in a
                // launcher that is not. A number with no word to sit in is
                // a number nobody sees, so the facts are read by the client
                // and are on the wire for whoever asks for them; the window
                // draws them when the vocabulary has the words — and the
                // count is drawn, because the vocabulary has its words
                // now.
                //
                // The feed is the exception that proves the rule: it has a
                // word — "the tree is no longer followed by the daemon" —
                // and it is drawn, because a daemon on a dead feed is a
                // thing the user has to know about.
            }
            Err(e) => {
                // The connection is dropped: the answer may still be on
                // its way, and a late answer would be read as the answer
                // to the next question.
                self.client = None;
                let mut st = self.status.borrow_mut();
                st.asked = true;
                // Three facts and not two. The daemon that took the
                // connection and said nothing in the window is up and
                // slow, and the daemon whose connection broke is gone: a
                // launcher that says "up and slow" about a daemon that is
                // not there hides the daemon the user has to start.
                let presence = presence(&e);
                st.connected = presence != Presence::Gone;
                let text = Text::for_lang(self.lang);
                st.warnings = vec![match presence {
                    Presence::Gone => {
                        format!("{}: {e}", rgy_launcher::text::no_daemon(self.lang))
                    }
                    Presence::Slow => text.daemon_slow.replace("{e}", &e.to_string()),
                    Presence::Answered => text.daemon_bad.replace("{e}", &e.to_string()),
                }];
            }
        }
        true
    }

    /// Ask the daemon who it is, and say what it said.
    ///
    /// The sentences are the ones the caller shows the user: the
    /// daemon's own when it is not there, and what the daemon said when
    /// it is there but slow. Nothing when the daemon is there and
    /// answering.
    pub fn ask_the_daemon(&mut self) -> Vec<String> {
        if self.connect() {
            self.status.borrow().warnings.clone()
        } else {
            vec![self.no_daemon_message()]
        }
    }

    /// The window, for the searches that follow.
    ///
    /// The caller's own budget: a wait that has less than a window left
    /// gives the daemon what is left of it, so the wait is bounded by the
    /// deadline and not by the deadline plus one more search.
    pub(crate) fn set_window(&mut self, window: Duration) {
        self.window = window;
    }

    /// The connection, made on demand, inside the caller's window.
    ///
    /// The deadline is the budget of the **whole** answer, and the connect
    /// is part of the answer: a pipe that stayed busy for the whole window
    /// is a window spent before the question was asked. So the connect is
    /// measured against the deadline, and so is the count question that
    /// follows it -- the count is a fact about the daemon's index and not
    /// the question the user asked, and it does not get a window of its own
    /// when the caller's window is the one that is left.
    fn client_within(&mut self, deadline: Option<Instant>) -> Option<&mut Client> {
        if self.client.is_none() {
            let client = match Client::connect_within(&self.endpoint, deadline) {
                Ok(client) => client,
                Err(e) => {
                    self.report_connect_failure(&e);
                    return None;
                }
            };
            self.client = Some(client);
            if let Some(client) = self.client.as_mut() {
                client.set_session(self.session);
            }
            // The daemon is here, so it can be asked how big its index
            // is. The question is asked once — when the connection opens
            // — and in a window of its own: a daemon that is not there
            // when the launcher starts can be there by the first
            // keystroke, and the header has to learn the count then.
            self.learn_the_count(deadline);
            let mut st = self.status.borrow_mut();
            st.asked = true;
            st.connected = true;
        }
        self.client.as_mut()
    }

    /// The connect failed, and what that says about the daemon.
    ///
    /// A connect that failed is a daemon that could not be **reached**,
    /// and that is one fact: the line says "no connection to the daemon",
    /// and the flag is cleared. The one exception is a connect that ran
    /// out of time: the pipe was busy, which means the daemon is there and
    /// the window was spent waiting for it — so the daemon is up and slow,
    /// and a launcher that says "no connection to the daemon" about it is
    /// saying a lie.
    ///
    /// A daemon that answered badly cannot be a fact a connect knows: it
    /// never got to answer.
    fn report_connect_failure(&mut self, e: &std::io::Error) {
        let mut st = self.status.borrow_mut();
        st.asked = true;
        let slow = e.kind() == std::io::ErrorKind::TimedOut;
        st.connected = slow;
        let text = Text::for_lang(self.lang);
        st.warnings = vec![if slow {
            text.daemon_slow.replace("{e}", &e.to_string())
        } else {
            format!("{}: {e}", rgy_launcher::text::no_daemon(self.lang))
        }];
    }

    /// Ask the daemon how many entries its index holds.
    ///
    /// The number is the daemon's own answer: the client does not walk
    /// the tree and cannot know the size of an index it does not have.
    ///
    /// A daemon that does not answer in the window leaves the count
    /// unknown, and the unknown count stays unknown — it is not a zero,
    /// and it is not a number the client made up.
    ///
    /// The connection is NOT dropped when the count question fails. The
    /// question the user asked is still worth asking, and the connection
    /// is the only way to ask it: a launcher that throws the connection
    /// away after a question about the daemon's index has to open a new
    /// one, and a daemon that is up and slow may not take a second
    /// connection while it holds the first. The client knows when the
    /// connection is spent, and it refuses the questions after it — and
    /// that refusal is a timeout, which is what the daemon is.
    fn learn_the_count(&mut self, deadline: Option<Instant>) {
        // The count gets the window it is given, and no more: the caller's
        // window is the budget of the whole answer, and the count is not
        // the question the user asked.
        let window = match deadline {
            Some(deadline) => {
                let left = left(deadline);
                if left.is_zero() {
                    return;
                }
                if left < COUNT_WINDOW {
                    left
                } else {
                    COUNT_WINDOW
                }
            }
            None => COUNT_WINDOW,
        };
        let answered = match self.client.as_mut() {
            Some(client) => client.status_within(window).ok(),
            None => None,
        };
        if let Some(status) = answered {
            self.status.borrow_mut().indexed = Some(status.docs);
        }
    }

    /// The answer failed: the connection is dropped, and the sentence is
    /// kept for the window.
    ///
    /// A daemon that did not answer in the window is **up and slow**, not
    /// gone: the line that says "no connection to the daemon" under a daemon
    /// that is up is a lie, and the count, the layer and the time of the
    /// last answer still belong to the line.
    fn fail(&mut self, failure: &Failure) {
        self.client = None;
        let mut st = self.status.borrow_mut();
        st.asked = true;
        // A daemon that answered badly is there: only the one that
        // could not be reached is gone.
        st.connected = failure.presence != Presence::Gone;
        st.warnings = vec![failure.reason.clone()];
    }

    /// The names the content layer reads.
    ///
    /// The search is made **without** the session: the session belongs to
    /// the window's own question, and a scope search that shared it would
    /// make the daemon re-rank the window's results against the scope.
    ///
    /// The argument is the query in the **engine's** words — the caller
    /// asks the model for it, and the model does the rewrite. The
    /// engine fails closed on a word it does not know, so a scope sent as
    /// `doc:` comes back empty and the content layer answers "nothing
    /// found" for a file that is on the disk.
    ///
    /// The window is the **caller's**: a scope that takes thirty seconds
    /// is a window that stopped answering the keyboard.
    ///
    /// `Err` is the daemon's reason. A daemon that stopped answering is
    /// not a daemon that chose nothing, and the caller has to say which
    /// one it was.
    fn scope_paths(
        &mut self,
        engine_query: &str,
        window: Duration,
    ) -> Result<(Vec<String>, Vec<String>), Failure> {
        // The session that is saved is the one the daemon named, and the
        // daemon names one when the request does not carry one. The client
        // keeps the name it was given, and the daemon takes it back — the
        // ids the daemon hands out are unguessable, so the name is a
        // capability and not a claim on somebody else's answer. Saving the
        // window's own id instead would throw that name away — the
        // window's id is None on the platforms where the launcher does not
        // choose one — and the next keystroke would start a new session
        // instead of re-ranking this answer.
        let saved = self.client.as_ref().and_then(|c| c.session());
        if let Some(client) = self.client.as_mut() {
            client.set_session(None);
        }
        let answer = match self.client.as_mut() {
            Some(client) => client
                .search_within(engine_query, SCOPE_LIMIT, window)
                .map(|out| {
                    // The warnings come back with the scope. The scope was
                    // chosen with the **name** half of the question, and a
                    // word the engine does not know is said here and
                    // nowhere else: the content answer is about the other
                    // half and cannot say it.
                    //
                    // The scope's own cap is not a warning: the scope is a
                    // selection, not an answer the user reads, and the
                    // answer that is on the screen is the content one —
                    // which says whether it is whole.
                    (
                        out.hits
                            .into_iter()
                            .map(|h| h.path)
                            .collect::<Vec<String>>(),
                        out.warnings,
                    )
                })
                .map_err(|e| Failure::of(Text::for_lang(self.lang).name_search_failed, &e)),
            None => Err(Failure::gone(String::from(rgy_launcher::text::no_daemon(
                self.lang,
            )))),
        };
        if let Some(client) = self.client.as_mut() {
            client.set_session(saved);
        }
        answer
    }

    /// The names, from the daemon.
    fn search_names(&mut self, query: &str, limit: usize) -> Answer {
        let window = self.window;
        // The deadline is taken before the connect: the connect is part of
        // the answer, and a window that starts after it is a window that
        // stops answering for longer than the window it was given.
        let deadline = Instant::now() + window;
        let Some(client) = self.client_within(Some(deadline)) else {
            return Answer::empty();
        };
        match client.search_within(query, limit, left(deadline)) {
            Ok(outcome) => {
                let answer = Answer {
                    paths: outcome.hits.iter().map(|h| h.path.clone()).collect(),
                    spans: outcome.hits.iter().map(|h| h.span.clone()).collect(),
                    contexts: Vec::new(),
                    // The daemon says which hits are folders; the path
                    // does not, and a folder shown with a file's icon is
                    // the wrong picture of the thing.
                    folders: outcome.hits.iter().map(|h| h.folder).collect(),
                };
                {
                    let mut st = self.status.borrow_mut();
                    st.connected = true;
                    // The daemon's own warnings, and the sentence for an
                    // answer that is not whole: the list on the screen is
                    // the whole answer, or it is a page of a longer one,
                    // and the window has to say which.
                    st.warnings = name_warnings(&outcome);
                    st.took_us = outcome.took_us;
                    st.layer = Some(Layer::Names);
                    st.matches = answer.len();
                }
                // The scope the fallback will use if the daemon goes away.
                self.last_scope = answer.paths.clone();
                self.last = answer.clone();
                answer
            }
            Err(e) => {
                self.fail(&Failure::of(
                    Text::for_lang(self.lang).name_search_failed,
                    &e,
                ));
                Answer::empty()
            }
        }
    }

    /// The contents, from the daemon, over the scope the names chose.
    ///
    /// The daemon first, and the `rgx` binary second: the daemon's index
    /// is warm and its scope is two hundred files, while the binary has to
    /// be started and has to walk what it is given.
    ///
    /// `engine_names` is the scope: the **name** part of the query in the
    /// **engine's** words, and empty means the **whole tree** — the
    /// daemon's own content engine answers that one, the pack store when
    /// there is one and the trigram index when there is not.
    ///
    /// The rewrite is the model's: the launcher's short words become the
    /// engine's, and the engine fails closed on a word it does not know —
    /// so a scope sent as `pic:` comes back empty and the content layer
    /// answers "nothing found" for a file that is on the disk.
    ///
    /// `content` is the content part as the user typed it. It is rebuilt
    /// here, in the engine's words, before anything goes on the wire.
    ///
    /// The window is the budget of the **whole** answer: the scope, the
    /// files and the fallback share it, so the window never stops
    /// answering for longer than the window it was given.
    fn search_content(&mut self, engine_names: &str, content: &str, limit: usize) -> Answer {
        // The daemon is asked in the **engine's** words: the canonical
        // prefix and the pattern as one unit. The engine's filter names
        // are exact and a word it does not know fails closed, and a
        // filter takes one word of the query — so the launcher's own
        // spelling and the launcher's own pattern have to be rebuilt
        // before they go on the wire.
        //
        // The fallback reads the same string, so the byte grep and the
        // daemon answer the same question.
        let content = rgy_launcher::layer::engine_content(content);
        // The window is the budget of the whole answer: the scope, the
        // files and the fallback share it. A window that answered the
        // scope in two seconds and then waited two more for the files
        // is a window that stopped answering for four.
        let deadline = Instant::now() + self.window;
        // The daemon first, connected on demand: the launcher is resident
        // and the daemon may have come up since the last keystroke. The
        // connect is inside the window: it is part of the answer.
        let connected = self.client_within(Some(deadline)).is_some();
        // The scope is the names part of the query, in the **engine's**
        // words: the model did the rewrite, and the engine fails closed
        // on a word it does not know.
        //
        // The failure, when there is one, is the reason the content layer
        // has no answer: a daemon that stopped answering is not a daemon
        // that chose nothing.
        let mut failure: Option<Failure> = None;
        let mut scope = Vec::new();
        // The scope that failed is not a scope with nothing in it: the
        // daemon died while choosing it, and the fallback has the last
        // one to work on.
        let mut scope_failed = false;
        // What the scope's own answer said. The scope is chosen with the
        // name half of the question, so a word the engine does not know
        // is said here and nowhere else.
        let mut scope_warnings: Vec<String> = Vec::new();
        if connected && !engine_names.is_empty() {
            match self.scope_paths(engine_names, left(deadline)) {
                Ok((paths, warnings)) => {
                    scope = paths;
                    scope_warnings = warnings;
                }
                Err(reason) => {
                    self.fail(&reason);
                    failure = Some(reason);
                    scope_failed = true;
                }
            }
        }
        if connected {
            if engine_names.is_empty() {
                // The whole tree: the daemon's own content engine.
                match self.whole_tree_content(&content, limit, left(deadline)) {
                    Ok(answer) => return answer,
                    Err(reason) => {
                        self.fail(&reason);
                        failure = Some(reason);
                    }
                }
            } else if !scope.is_empty() {
                match self.daemon_content(&content, &scope, &scope_warnings, limit, left(deadline))
                {
                    Ok(answer) => return answer,
                    Err(reason) => {
                        // The daemon answered with a failure: the fallback
                        // below may still answer, and a launcher that says
                        // "no results" when the machine can answer is a
                        // launcher that lies.
                        self.fail(&reason);
                        failure = Some(reason);
                    }
                }
            }
        }
        // The fallback: the `rgx` binary, over the scope the names chose.
        //
        // The scope is the one **this** query chose. The last one — the
        // files that were on the screen when the daemon went away — is
        // only used when the daemon is not there at all: a launcher that
        // answers the previous question when the current one has no scope
        // is a launcher that lies.
        //
        // It is not the whole tree either: a content search over a tree is
        // a walk, and a launcher that blocks the window for minutes is a
        // launcher nobody uses.
        let scope = if connected && !scope_failed {
            scope
        } else {
            self.last_scope.clone()
        };
        if !scope.is_empty()
            && let Some(binary) = self.rgx.clone()
        {
            let window = left(deadline);
            if window.is_zero() {
                // The window is the budget of the whole answer and it is
                // gone: the fallback would be a process started with no
                // time to answer, and the sentence says what happened.
                //
                // The daemon is not gone, though: the launcher's own budget
                // ran out, and the daemon may well be there and busy. Only a
                // daemon that could not be reached is gone, so the flag
                // survives and the connection is not thrown away.
                let reason = Failure::slow(String::from(Text::for_lang(self.lang).no_time_for_rgx));
                self.fail(&reason);
                failure = Some(reason);
            } else {
                return self.rgx_content(&binary, &content, &scope, limit, window);
            }
        }
        // Nothing answered, and the status says why. The reason is one of
        // three, and they are three different facts about the machine: the
        // daemon failed while answering, the daemon is not there, or the
        // daemon is there and the name part chose nothing to read.
        let daemon_up = self.status.borrow().connected;
        let (connected, warning) = match failure {
            // The daemon's own sentence is the reason: it failed, and this
            // function did not. A daemon that is slow is not a daemon that
            // is gone, and the line says which one it was.
            Some(reason) => (reason.presence != Presence::Gone, reason.reason),
            // The daemon is up and answering: the name part chose nothing,
            // so there was nothing to read. Saying the daemon is down here
            // would be a lie — it is answering.
            None if daemon_up => (
                true,
                String::from(Text::for_lang(self.lang).no_files_to_read),
            ),
            // The daemon is not there. Its own reason is kept — it says
            // what went wrong — and the content layer's reason is added
            // when there is none: the fallback needs a scope, and a scope
            // comes from the daemon's name answer.
            None => {
                let warning = match self.status.borrow().warnings.first() {
                    Some(reason) => reason.clone(),
                    None => self.no_content_reason().to_string(),
                };
                (false, warning)
            }
        };
        let mut st = self.status.borrow_mut();
        st.layer = Some(Layer::Content);
        st.matches = 0;
        st.connected = connected;
        // The scope's warnings first: they say why the scope was empty,
        // and the reason below says what the content layer did about it.
        let mut warnings = scope_warnings;
        warnings.push(warning);
        st.warnings = warnings;
        Answer::empty()
    }

    /// The content answer over the **whole tree**.
    ///
    /// The daemon's own content engine answers it: the pack store when
    /// the daemon was started with one, and the trigram index when it was
    /// not. It is the answer to "which files say this", with no scope.
    ///
    /// The window is the **caller's**: the answer reads the tree, and a
    /// window that waits thirty seconds for it is a window that stopped
    /// answering the keyboard.
    ///
    /// `Err` is the daemon's reason: the caller drops the connection and
    /// says it.
    fn whole_tree_content(
        &mut self,
        content: &str,
        limit: usize,
        window: Duration,
    ) -> Result<Answer, Failure> {
        // The connect is inside the window the caller gave: the window is
        // the budget of the whole answer, and the connect is part of it.
        let deadline = Instant::now() + window;
        let Some(client) = self.client_within(Some(deadline)) else {
            return Err(Failure::gone(String::from(rgy_launcher::text::no_daemon(
                self.lang,
            ))));
        };
        let outcome = client
            .grep_within(content, limit, window)
            .map_err(|e| Failure::of(Text::for_lang(self.lang).content_search_failed, &e))?;
        let mut paths = Vec::new();
        let mut spans = Vec::new();
        let mut contexts = Vec::new();
        for file in &outcome.files {
            paths.push(file.path.clone());
            spans.push(Vec::new());
            contexts.push(content_line(file));
        }
        let answer = Answer {
            paths,
            spans,
            contexts,
            // A content answer is a list of files: the content layer reads
            // files, and a folder has no lines to show.
            folders: Vec::new(),
        };
        let backend = backend_label(rgy_launcher::ContentBackend::Daemon, self.lang);
        {
            let mut st = self.status.borrow_mut();
            st.connected = true;
            st.warnings = content_warnings(&outcome);
            st.took_us = outcome.took_us;
            st.layer = Some(Layer::Content);
            st.backend = backend;
            st.matches = answer.len();
        }
        self.last = answer.clone();
        Ok(answer)
    }

    /// The content answer, from the daemon.
    ///
    /// The window is the **launcher's**, as everywhere else on the content
    /// path: the answer reads the files of the scope, and the window that
    /// waits for it is the one that draws.
    ///
    /// `Err` is the daemon's reason: the caller drops the connection, says
    /// it, and tries the fallback.
    fn daemon_content(
        &mut self,
        content: &str,
        scope: &[String],
        scope_warnings: &[String],
        limit: usize,
        window: Duration,
    ) -> Result<Answer, Failure> {
        // The connect is inside the window the caller gave: the window is
        // the budget of the whole answer, and the connect is part of it.
        let deadline = Instant::now() + window;
        let Some(client) = self.client_within(Some(deadline)) else {
            return Err(Failure::gone(String::from(rgy_launcher::text::no_daemon(
                self.lang,
            ))));
        };
        let outcome = client
            .content_within(content, scope, limit, window)
            .map_err(|e| Failure::of(Text::for_lang(self.lang).content_search_failed, &e))?;
        let mut paths = Vec::new();
        let mut spans = Vec::new();
        let mut contexts = Vec::new();
        for file in &outcome.files {
            paths.push(file.path.clone());
            spans.push(Vec::new());
            contexts.push(content_line(file));
        }
        let answer = Answer {
            paths,
            spans,
            contexts,
            // A content answer is a list of files: the content layer reads
            // files, and a folder has no lines to show.
            folders: Vec::new(),
        };
        let backend = backend_label(rgy_launcher::ContentBackend::Daemon, self.lang);
        {
            let mut st = self.status.borrow_mut();
            st.connected = true;
            // The scope's warnings and the content answer's: the scope was
            // chosen with the name half of the question and the answer
            // comes from the content half, and the user asked both.
            let mut warnings = scope_warnings.to_vec();
            warnings.extend(content_warnings(&outcome));
            st.warnings = warnings;
            st.took_us = outcome.took_us;
            st.layer = Some(Layer::Content);
            st.backend = backend;
            st.matches = answer.len();
        }
        self.last = answer.clone();
        Ok(answer)
    }

    /// The content answer, from the `rgx` binary.
    ///
    /// The scope is the files the names chose — never the tree. The binary
    /// is a fallback for a daemon that went away, and the point of a
    /// fallback is to answer the question that was asked, not a bigger one.
    fn rgx_content(
        &mut self,
        binary: &str,
        content: &str,
        scope: &[String],
        limit: usize,
        window: Duration,
    ) -> Answer {
        match crate::rgx::grep(binary, content, scope, window, self.lang) {
            Ok(files) => {
                let mut paths = Vec::new();
                let mut spans = Vec::new();
                let mut contexts = Vec::new();
                for file in files.iter().take(limit) {
                    paths.push(file.path.clone());
                    spans.push(Vec::new());
                    contexts.push(content_line(file));
                }
                let answer = Answer {
                    paths,
                    spans,
                    contexts,
                    // A content answer is a list of files: the content layer
                    // reads files, and a folder has no lines to show.
                    folders: Vec::new(),
                };
                let backend = backend_label(rgy_launcher::ContentBackend::Rgx, self.lang);
                {
                    let mut st = self.status.borrow_mut();
                    st.warnings = Vec::new();
                    // The fallback does not say how long it took, and the
                    // time of the last answer is not its time.
                    st.took_us = 0;
                    st.layer = Some(Layer::Content);
                    st.backend = backend;
                    st.matches = answer.len();
                }
                self.last = answer.clone();
                answer
            }
            Err(reason) => {
                self.status.borrow_mut().warnings = vec![reason];
                Answer::empty()
            }
        }
    }
}

/// The warnings of a name answer.
///
/// The daemon's own warnings, and the sentence for an answer that is not
/// whole. The content path says the same thing about the same answer, and
/// a window that shows a short list without saying so shows an answer that
/// looks whole.
fn name_warnings(outcome: &rgy_proto::client::SearchOutcome) -> Vec<String> {
    let mut out = outcome.warnings.clone();
    out.extend(outcome.partial());
    out
}

/// The warnings of a content answer.
///
/// The daemon's own warnings, and the sentence for an answer that did not
/// read everything. The command line says the same thing about the same
/// answer, and a window that shows a short list without saying so shows
/// an answer that looks whole.
///
/// The reasons are the daemon's own words: a file that was not read because
/// it is a cloud placeholder and a file the budget never reached are two
/// different things, and a count that does not say which of them it was
/// leaves the user guessing. The sentence is the protocol's, so the window
/// says what the daemon said and not what the window thinks of it.
fn content_warnings(outcome: &rgy_proto::client::GrepOutcome) -> Vec<String> {
    let mut out = outcome.warnings.clone();
    out.extend(outcome.partial());
    out.extend(outcome.skip_reasons());
    out
}

/// The sentence for a feed that is not live, when it is not live.
///
/// `gone` and `none` are two different facts and they get two different
/// sentences: a feed that died was there and is not, and a daemon that
/// never had one answers on a snapshot from the start. `live` is not a
/// warning at all.
///
/// The feed is the protocol's own vocabulary and not a string: a reader
/// that compared strings would have to spell the spelling again, and a
/// spelling that drifts is a warning that stops being said.
fn feed_warning(feed: rgy_proto::Feed, lang: Lang) -> Option<String> {
    let text = Text::for_lang(lang);
    match feed {
        rgy_proto::Feed::Gone => Some(text.tree_not_followed.to_string()),
        rgy_proto::Feed::None => Some(text.tree_never_followed.to_string()),
        rgy_proto::Feed::Live => None,
    }
}

/// The word the status line shows for the content layer's backend.
///
/// The label belongs to the **answer**: the daemon's answers are the
/// adapters' answers and the fallback's are the binary's. Deciding it
/// from "is the daemon up" would say "adaptadores" for an answer the
/// `rgx` binary produced while the daemon was away.
fn backend_label(backend: rgy_launcher::ContentBackend, lang: Lang) -> String {
    backend.label(lang).to_string()
}

/// What is left of the window.
///
/// The window is the budget of the whole answer, and every step of it
/// gets what is left: a window that answered the scope in two seconds
/// and then waited two more for the files is a window that stopped
/// answering for four.
fn left(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

/// The line under the name of a content result.
///
/// The first line that matched, and how many more there are: the user is
/// asking *where* in the file the word is, and the count is what tells
/// them whether there is more to see.
fn content_line(file: &rgy_proto::GrepFile) -> String {
    match file.lines.first() {
        Some((number, text)) => {
            if file.lines.len() > 1 {
                format!("{number}: {text}  (+{})", file.lines.len() - 1)
            } else {
                format!("{number}: {text}")
            }
        }
        None => String::new(),
    }
}

impl Source for DaemonSource {
    /// How many entries the daemon's index holds.
    ///
    /// It is the daemon's own number — the one its status carries — and
    /// not a count the client made: the client does not walk the tree
    /// and cannot know the size of an index it does not have.
    ///
    /// `None` when the daemon has not said: a daemon that is not there
    /// has no count, and a zero the client made up for it would claim an
    /// empty index nobody ever saw.
    fn indexed(&self) -> Option<usize> {
        self.status.borrow().indexed
    }

    fn search(&mut self, query: &str, limit: usize) -> Answer {
        // The model owns the split: the source asks it for the two
        // halves — the engine's question and the content layer's — and
        // does not split the query a second time. A second split is a
        // second place to be wrong, and the two places would answer
        // different questions for the same words.
        let model = Query::of(query);
        let halves = model.halves();
        if !halves.is_content() {
            // The launcher's own words become the engine's words.
            return self.search_names(&halves.engine(), limit);
        }
        if halves.pattern().is_empty() {
            // The prefix is being typed: the question has not been asked
            // yet, and the list keeps what it has.
            return self.last.clone();
        }
        // The content part goes to the daemon **with its prefix**: the
        // engine reads `grep:` as a literal pattern and `regex:` as a
        // regular expression, and a bare word would be a name filter.
        // The prefix and the pattern are put in the engine's words on
        // the way down — the engine's filter names are exact, and a
        // filter takes one word of the query.
        self.search_content(&halves.engine(), halves.content().unwrap_or(""), limit)
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    /// The launcher's window is the client's window shortened.
    ///
    /// The client waits thirty seconds, which is right for a program that
    /// waits in the background and wrong for a search box: thirty seconds
    /// of silence is a window that stopped answering the keyboard.
    #[test]
    fn the_launcher_asks_in_a_window_it_can_survive() {
        assert!(
            LAUNCHER_WINDOW < rgy_proto::client::read_timeout(),
            "the launcher does not wait as long as the client"
        );
        assert!(
            LAUNCHER_WINDOW >= std::time::Duration::from_millis(250),
            "the launcher waits long enough for an answer to arrive"
        );
    }
    use super::*;

    /// The window says the answer is partial.
    ///
    /// The daemon says what it did not read, and the command line says
    /// it. The window said nothing: a content search that was cut short
    /// showed a short list with nothing saying the list was short.
    #[test]
    fn the_window_says_the_answer_is_partial() {
        let outcome = rgy_proto::client::GrepOutcome {
            complete: false,
            skipped: 3,
            ..Default::default()
        };
        let warnings = content_warnings(&outcome);
        assert!(
            warnings.iter().any(|w| w.contains("3 file(s)")),
            "the window does not say what the daemon did not read: {warnings:?}"
        );
        assert!(
            warnings.iter().any(|w| w.contains("partial")),
            "the window does not say the answer is partial: {warnings:?}"
        );
    }

    /// The window says a name answer is not whole.
    ///
    /// The name layer could not say it before: the list on the screen looked
    /// like the whole answer, and a list that reached the limit was the
    /// same list as one that did not. The window says the words the
    /// content path says, because it is the same kind of answer.
    #[test]
    fn the_window_says_a_name_answer_is_not_whole() {
        let cut = rgy_proto::client::SearchOutcome {
            complete: false,
            ..Default::default()
        };
        let warnings = name_warnings(&cut);
        assert!(
            warnings.iter().any(|w| w.contains("limit")),
            "the window does not say what cut the answer: {warnings:?}"
        );

        // The same words the content path says: the two layers cut their
        // answers at the same place, and the window says one thing about
        // it.
        let content = rgy_proto::client::GrepOutcome {
            complete: false,
            skipped: 0,
            ..Default::default()
        };
        assert_eq!(
            warnings,
            content_warnings(&content),
            "the two layers say the same words about the same answer"
        );

        // A whole answer says nothing about being whole.
        let whole = rgy_proto::client::SearchOutcome {
            complete: true,
            ..Default::default()
        };
        assert!(
            name_warnings(&whole).is_empty(),
            "a whole answer carries no sentence"
        );
    }

    /// An answer that stopped at the limit says that, and names no files.
    ///
    /// The daemon read what it was asked to read and the answer is still
    /// not whole: the limit cut it. A window that said "did not read 0
    /// file(s)" would be naming a number that says nothing and sounds
    /// like it does.
    #[test]
    fn an_answer_cut_short_by_the_limit_names_no_files() {
        let outcome = rgy_proto::client::GrepOutcome {
            complete: false,
            skipped: 0,
            ..Default::default()
        };
        let warnings = content_warnings(&outcome);
        assert!(
            !warnings.iter().any(|w| w.contains("0 file(s)")),
            "the window names files it did not read: {warnings:?}"
        );
        assert!(
            warnings.iter().any(|w| w.contains("limit")),
            "the window does not say what cut the answer: {warnings:?}"
        );
    }

    /// A whole answer says nothing about being whole.
    #[test]
    fn a_whole_answer_carries_no_partial_sentence() {
        let outcome = rgy_proto::client::GrepOutcome {
            complete: true,
            ..Default::default()
        };
        assert!(
            content_warnings(&outcome).is_empty(),
            "a whole answer was called partial"
        );
    }

    /// A daemon that does not follow the tree says so, both ways.
    ///
    /// A feed that died and a daemon that never had one are two facts,
    /// and the window that says "the daemon is there" is telling the
    /// user that a live tree is behind it when there is none.
    #[test]
    fn a_daemon_that_does_not_follow_the_tree_says_so() {
        let live = feed_warning(rgy_proto::Feed::Live, Lang::English);
        assert!(live.is_none(), "a live feed is not a warning: {live:?}");
        for feed in [rgy_proto::Feed::Gone, rgy_proto::Feed::None] {
            let warning = feed_warning(feed, Lang::English);
            assert!(
                warning.as_deref().is_some_and(|w| !w.is_empty()),
                "the feed {feed:?} is not warned about"
            );
        }
        // And in the user's language.
        let none = rgy_proto::Feed::None;
        let portuguese = feed_warning(none, Lang::Portuguese).unwrap_or_default();
        assert!(
            portuguese.contains("não segue"),
            "the sentence is not in the user's language: {portuguese:?}"
        );
    }

    /// A daemon that answered badly is a daemon that is there.
    ///
    /// The daemon's own error, a line that is not an answer and an
    /// answer that stops in the middle are three ways of saying the
    /// same thing: the daemon took the connection and said something
    /// wrong. Only the daemon that could not be reached is gone.
    #[test]
    fn a_daemon_that_answered_badly_is_a_daemon_that_is_there() {
        // The daemon's own error.
        assert_eq!(
            presence(&std::io::Error::other(
                "o índice está em modo somente-leitura"
            )),
            Presence::Answered
        );
        // A line that is not an answer.
        assert_eq!(
            presence(&std::io::Error::from(std::io::ErrorKind::InvalidData)),
            Presence::Answered
        );
        // An answer that stops in the middle.
        assert_eq!(
            presence(&std::io::Error::from(std::io::ErrorKind::UnexpectedEof)),
            Presence::Answered
        );
        // A daemon that is not there.
        assert_eq!(
            presence(&std::io::Error::from(std::io::ErrorKind::NotFound)),
            Presence::Gone
        );
        assert_eq!(
            presence(&std::io::Error::from(std::io::ErrorKind::BrokenPipe)),
            Presence::Gone
        );
        // The socket file of a daemon that died: the connection is
        // refused, and the daemon is not there.
        assert_eq!(
            presence(&std::io::Error::from(std::io::ErrorKind::ConnectionRefused)),
            Presence::Gone
        );
        // A daemon that took the connection and said nothing.
        assert_eq!(
            presence(&std::io::Error::from(std::io::ErrorKind::TimedOut)),
            Presence::Slow
        );
    }

    #[test]
    fn a_content_line_says_where_the_word_is() {
        let file = rgy_proto::GrepFile {
            path: "/a/notes.txt".to_string(),
            lines: vec![(42, "o total do invoice".to_string())],
        };
        assert_eq!(content_line(&file), "42: o total do invoice");
    }

    #[test]
    fn a_content_line_with_more_lines_says_how_many() {
        let file = rgy_proto::GrepFile {
            path: "/a/notes.txt".to_string(),
            lines: vec![
                (1, "primeira".to_string()),
                (2, "segunda".to_string()),
                (3, "terceira".to_string()),
            ],
        };
        assert_eq!(content_line(&file), "1: primeira  (+2)");
    }

    #[test]
    fn a_content_line_with_no_lines_is_empty() {
        let file = rgy_proto::GrepFile {
            path: "/a/notes.txt".to_string(),
            lines: Vec::new(),
        };
        assert_eq!(content_line(&file), "");
    }

    #[test]
    fn a_source_without_a_daemon_answers_nothing_and_says_why() {
        // The endpoint is a socket that is not there: the launcher must
        // not panic, and must say what is wrong.
        let mut source = DaemonSource::new(
            Endpoint::Socket(std::path::PathBuf::from("/tmp/rgy-rgxy-nope.sock")),
            None,
            Lang::English,
        );
        let answer = source.search("invoice", 10);
        assert!(answer.is_empty());
        let st = source.status();
        assert!(!st.borrow().connected);
        assert!(!st.borrow().warnings.is_empty());
    }

    /// The count is the daemon's own number.
    ///
    /// The daemon's status carries how many entries its index holds, and
    /// that is the number the terminal's header shows: the client does
    /// not walk the tree and cannot know the size of an index it does
    /// not have.
    #[test]
    fn the_count_is_the_daemons_own_number() {
        let daemon = serve(answers_with_a_count);
        let mut source = DaemonSource::new(daemon.endpoint.clone(), None, Lang::English);
        assert!(source.connect(), "the daemon is there");
        assert_eq!(source.indexed(), Some(1226));
        drop(source);
        daemon.join();
    }

    /// A source that has not heard from the daemon does not know the count.
    ///
    /// There is no daemon to ask, so there is no count — and a zero the
    /// client made up for it would claim an empty index nobody ever saw.
    #[test]
    fn a_source_without_a_daemon_does_not_know_the_count() {
        let mut source = DaemonSource::new(
            Endpoint::Socket(std::path::PathBuf::from("/tmp/rgy-rgxy-no-count.sock")),
            None,
            Lang::English,
        );
        assert_eq!(source.indexed(), None);
        let _ = source.search("invoice", 10);
        assert_eq!(source.indexed(), None);
    }

    #[test]
    fn the_daemon_is_asked_in_the_engines_own_words() {
        // The content part goes down the pipe in the engine's spelling,
        // and the launcher's own table is what builds it: the canonical
        // prefix and the pattern as one unit.
        //
        // The engine's filter names are exact, their case is part of the
        // name, and a word it does not know fails closed — `Grep:total`
        // sent as typed is an error and not a search, and the launcher
        // would blame the daemon for a typo it made itself.
        assert_eq!(
            rgy_launcher::layer::engine_content("Grep:total"),
            "grep:total"
        );
        assert_eq!(
            rgy_launcher::layer::engine_content("grep:total"),
            "grep:total"
        );
        // And the engine gives a filter one word of the query: the
        // pattern is one unit, or the daemon answers a broader question
        // than the user asked.
        assert_eq!(
            rgy_launcher::layer::engine_content("grep:total due"),
            "grep:\"total due\""
        );
    }

    /// The test's own end of the transport.
    ///
    /// The client speaks a socket on unix and a named pipe on Windows, and
    /// the test speaks the same one: a stand-in that spoke something else
    /// would prove nothing about the transport the launcher really uses.
    enum Peer {
        #[cfg(unix)]
        Socket(std::os::unix::net::UnixStream),
        #[cfg(windows)]
        Pipe(std::fs::File),
    }

    impl Peer {
        /// Send bytes to the client.
        fn send(&mut self, bytes: &[u8]) -> std::io::Result<()> {
            match self {
                #[cfg(unix)]
                Peer::Socket(s) => s.write_all(bytes),
                #[cfg(windows)]
                Peer::Pipe(f) => f.write_all(bytes),
            }
        }

        /// Read the client's request, one line, without its newline.
        ///
        /// A byte at a time: the request is one short line, and a reader
        /// that waits for a newline it may never get is a reader that
        /// hangs the test.
        fn read_line(&mut self) -> std::io::Result<String> {
            let mut line = Vec::new();
            loop {
                let mut byte = [0u8; 1];
                let got = match self {
                    #[cfg(unix)]
                    Peer::Socket(s) => s.read(&mut byte)?,
                    #[cfg(windows)]
                    Peer::Pipe(f) => f.read(&mut byte)?,
                };
                if got == 0 || byte[0] == b'\n' {
                    break;
                }
                line.push(byte[0]);
            }
            Ok(String::from_utf8_lossy(&line).into_owned())
        }
    }

    /// A daemon of the test's own, on an endpoint of the test's own.
    struct Daemon {
        endpoint: Endpoint,
        thread: std::thread::JoinHandle<()>,
    }

    impl Daemon {
        /// Wait for the daemon to be done.
        fn join(self) {
            let _ = self.thread.join();
        }
    }

    /// A name that is the test's own.
    fn unique() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        format!(
            "rgy-rgxy-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        )
    }

    /// A daemon that takes the connection and answers: the status, and a
    /// search with nothing in it.
    ///
    /// The name search comes back **empty**, and that is the case under
    /// test: the daemon is there, it answered, and it chose nothing. The
    /// daemon answers what it is asked, in the order it is asked, and it
    /// stays until the client goes — a daemon that hangs up in the middle
    /// of an answer is a different case, and one the suite has already.
    fn answers_with_no_names(peer: Peer) {
        let mut peer = peer;
        while let Ok(request) = peer.read_line() {
            if request.is_empty() {
                break;
            }
            if request.contains("\"op\":\"status\"") {
                let _ = peer.send(
                    b"{\"op\":\"status\",\"docs\":0,\"event_id\":0,\"catch_up\":\"replayed\",\"tv\":false}\n",
                );
            } else {
                let _ = peer.send(b"{\"op\":\"hits\",\"hits\":[],\"took_us\":1}\n");
                let _ = peer.send(b"{\"op\":\"done\",\"count\":0}\n");
            }
        }
    }

    /// A daemon that says how many entries its index holds.
    ///
    /// The number is the daemon's own, and it is the number the terminal's
    /// header shows: a client that counted the entries itself would be
    /// counting an index it does not have.
    fn answers_with_a_count(peer: Peer) {
        let mut peer = peer;
        while let Ok(request) = peer.read_line() {
            if request.is_empty() {
                break;
            }
            if request.contains("\"op\":\"status\"") {
                let _ = peer.send(
                    b"{\"op\":\"status\",\"docs\":1226,\"event_id\":0,\"catch_up\":\"replayed\",\"tv\":false}\n",
                );
            } else {
                let _ = peer.send(b"{\"op\":\"hits\",\"hits\":[],\"took_us\":1}\n");
                let _ = peer.send(b"{\"op\":\"done\",\"count\":0}\n");
            }
        }
    }

    /// A daemon that answers with an error.
    ///
    /// The daemon is there and it says that it could not do what it was
    /// asked: the sentence is its own, and it is what the user is told.
    fn says_an_error(peer: Peer) {
        let mut peer = peer;
        while let Ok(request) = peer.read_line() {
            if request.is_empty() {
                break;
            }
            if request.contains("\"op\":\"status\"") {
                let _ = peer.send(
                    b"{\"op\":\"status\",\"docs\":0,\"event_id\":0,\"catch_up\":\"replayed\",\"tv\":false}\n",
                );
            } else {
                // The daemon's own sentence, in the daemon's language:
                // the launcher shows what the daemon said and does not
                // translate it.
                let _ = peer.send(
                    "{\"op\":\"error\",\"message\":\"o índice está em modo somente-leitura\"}\n"
                        .as_bytes(),
                );
            }
        }
    }

    /// A daemon that takes the connection and says nothing.
    ///
    /// The client is there and the daemon is silent: the case the
    /// launcher's window is about. The daemon reads what it is asked and
    /// answers none of it, and it stays until the client goes.
    fn says_nothing(peer: Peer) {
        let mut peer = peer;
        while let Ok(request) = peer.read_line() {
            if request.is_empty() {
                break;
            }
        }
    }

    /// A daemon that records what it was asked, and answers it.
    ///
    /// The names come back with one file in them, so the content layer
    /// has a scope to read: the question that reaches the daemon is the
    /// question the test is about.
    fn records_the_requests(peer: Peer, requests: std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let mut peer = peer;
        while let Ok(request) = peer.read_line() {
            if request.is_empty() {
                break;
            }
            requests.lock().unwrap().push(request.clone());
            if request.contains("\"op\":\"status\"") {
                let _ = peer.send(
                    b"{\"op\":\"status\",\"docs\":0,\"event_id\":0,\"catch_up\":\"replayed\",\"tv\":false}\n",
                );
            } else if request.contains("\"op\":\"grep-scoped\"") {
                let _ = peer.send(
                    b"{\"op\":\"grep-files\",\"files\":[{\"path\":\"/a/notes.txt\",\"lines\":[[1,\"total\"]]}],\"took_us\":1}\n",
                );
                let _ = peer.send(b"{\"op\":\"done\",\"count\":1}\n");
            } else {
                let _ = peer.send(
                    b"{\"op\":\"hits\",\"hits\":[{\"path\":\"/a/notes.txt\",\"score\":1}],\"took_us\":1}\n",
                );
                let _ = peer.send(b"{\"op\":\"done\",\"count\":1}\n");
            }
        }
    }

    /// A daemon that takes one client and does what the test says.
    #[cfg(unix)]
    fn serve(script: impl FnOnce(Peer) + Send + 'static) -> Daemon {
        let path = std::env::temp_dir().join(format!("{}.sock", unique()));
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("the socket binds");
        let socket = path.clone();
        let thread = std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                script(Peer::Socket(stream));
            }
            let _ = std::fs::remove_file(&socket);
        });
        Daemon {
            endpoint: Endpoint::Socket(path),
            thread,
        }
    }

    /// A daemon that takes one client and does what the test says.
    #[cfg(windows)]
    fn serve(script: impl FnOnce(Peer) + Send + 'static) -> Daemon {
        use std::os::windows::io::AsRawHandle;
        unsafe extern "system" {
            fn ConnectNamedPipe(
                pipe: *mut core::ffi::c_void,
                overlapped: *mut core::ffi::c_void,
            ) -> i32;
        }
        let name = unique();
        let file = create_pipe(&name);
        let thread = std::thread::spawn(move || {
            // The instance is already there: the only thing left to wait
            // for is the client.
            unsafe { ConnectNamedPipe(file.as_raw_handle(), core::ptr::null_mut()) };
            script(Peer::Pipe(file));
        });
        Daemon {
            endpoint: Endpoint::Pipe(name),
            thread,
        }
    }

    /// The pipe instance the test's daemon waits on.
    #[cfg(windows)]
    fn create_pipe(name: &str) -> std::fs::File {
        use std::os::windows::io::FromRawHandle;
        unsafe extern "system" {
            fn CreateNamedPipeW(
                name: *const u16,
                open_mode: u32,
                pipe_mode: u32,
                max_instances: u32,
                out_size: u32,
                in_size: u32,
                timeout: u32,
                sec: *mut core::ffi::c_void,
            ) -> *mut core::ffi::c_void;
        }
        const PIPE_ACCESS_DUPLEX: u32 = 0x3;
        const PIPE_TYPE_BYTE: u32 = 0x0;
        const PIPE_UNLIMITED_INSTANCES: u32 = 255;
        let path = format!(r"\\.\pipe\{name}");
        let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        let handle = unsafe {
            CreateNamedPipeW(
                wide.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE,
                PIPE_UNLIMITED_INSTANCES,
                64 * 1024,
                64 * 1024,
                0,
                core::ptr::null_mut(),
            )
        };
        assert!(!handle.is_null(), "the pipe instance was created");
        // The file owns the handle: dropping it closes the pipe, which is
        // what tells the client the stream is over.
        unsafe { std::fs::File::from_raw_handle(handle) }
    }

    /// The source asks the model for the two halves.
    ///
    /// The split is the model's: the source asks it for the engine's
    /// question and the content layer's, and does not split the query on
    /// its own. A source that split it again would be a second place to
    /// be wrong, and the two would answer different questions for the
    /// same words.
    #[test]
    fn the_source_asks_the_model_for_the_two_halves() {
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = std::sync::Arc::clone(&requests);
        let daemon = serve(move |peer| records_the_requests(peer, recorded));
        let mut source = DaemonSource::new(daemon.endpoint.clone(), None, Lang::English);
        assert!(source.connect(), "the daemon is there");
        let _ = source.search("pic: invoice grep:total", 10);

        // What the model says the two questions are.
        let model = rgy_launcher::Query::of("pic: invoice grep:total");
        let halves = model.halves();
        assert_eq!(halves.engine(), "type:image invoice");
        assert_eq!(halves.content(), Some("grep:total"));

        // And what the daemon was asked.
        let asked = requests.lock().unwrap().clone();
        assert!(
            asked
                .iter()
                .any(|r| r.contains(r#""q":"type:image invoice""#)),
            "the engine was asked in its own words: {asked:?}"
        );
        assert!(
            asked.iter().any(|r| r.contains(r#""q":"grep:total""#)),
            "the content layer was asked with the content part: {asked:?}"
        );
    }

    /// A daemon that chooses nothing is not a daemon that is down.
    ///
    /// The name part of `invoice grep:zebra` matched nothing, so there was
    /// nothing to read: the daemon is up and it answered. The status line
    /// that says "no daemon" under a running daemon is a lie, and it is
    /// the lie this test pins.
    #[test]
    fn a_daemon_that_chooses_nothing_is_not_a_daemon_that_is_down() {
        let daemon = serve(answers_with_no_names);
        let mut source = DaemonSource::new(daemon.endpoint.clone(), None, Lang::English);
        assert!(source.connect(), "the daemon is there");
        let answer = source.search("invoice grep:zebra", 10);
        assert!(answer.is_empty(), "there was nothing to read");
        let st = source.status();
        assert!(st.borrow().connected, "the daemon is answering");
        let warnings = st.borrow().warnings.clone();
        assert!(
            !warnings
                .iter()
                .any(|w| w.contains("no connection to the daemon")),
            "the daemon is up: {warnings:?}"
        );
        assert!(
            warnings.iter().any(|w| w.contains("no result")),
            "the status says the names chose nothing: {warnings:?}"
        );
        // The daemon waits for the client to go.
        drop(source);
        daemon.join();
    }

    /// The scope's warnings are said.
    ///
    /// The scope is chosen with the name half of the question and the
    /// content answer comes from the other half, so a warning about the
    /// name half -- a word the engine does not know -- was lost. A window
    /// that says "no files to read" without saying why the scope was
    /// empty leaves the user guessing.
    #[test]
    fn the_scope_search_says_what_it_warned_about() {
        let daemon = serve(|peer| {
            let mut peer = peer;
            while let Ok(request) = peer.read_line() {
                if request.is_empty() {
                    break;
                }
                if request.contains("\"op\":\"status\"") {
                    let _ = peer.send(
                        b"{\"op\":\"status\",\"docs\":0,\"event_id\":0,\"catch_up\":\"replayed\",\"tv\":false}\n",
                    );
                } else {
                    let _ =
                        peer.send(b"{\"op\":\"warning\",\"message\":\"unknown type: banana\"}\n");
                    let _ = peer.send(b"{\"op\":\"hits\",\"hits\":[],\"took_us\":1}\n");
                    let _ = peer.send(b"{\"op\":\"done\",\"count\":0}\n");
                }
            }
        });
        let mut source = DaemonSource::new(daemon.endpoint.clone(), None, Lang::English);
        assert!(source.connect(), "the daemon is there");
        let _ = source.search("banana grep:zebra", 10);
        let st = source.status();
        let warnings = st.borrow().warnings.clone();
        assert!(
            warnings.iter().any(|w| w.contains("unknown type: banana")),
            "the scope's warning is lost: {warnings:?}"
        );
        drop(source);
        daemon.join();
    }

    /// A daemon that takes the connection and says nothing does not hold
    /// the launcher.
    ///
    /// The window is the **launcher's**: the client waits thirty seconds,
    /// and a window that waits thirty seconds is a window that stopped
    /// answering the keyboard.
    #[test]
    fn a_daemon_that_says_nothing_does_not_hold_the_launcher() {
        let daemon = serve(says_nothing);
        let mut source = DaemonSource::new(daemon.endpoint.clone(), None, Lang::English);
        let started = std::time::Instant::now();
        let _ = source.connect();
        let waited = started.elapsed();
        assert!(
            waited < std::time::Duration::from_secs(5),
            "the launcher waited for the client's window: {waited:?}"
        );
        // The daemon took the connection and said nothing: it is up and
        // slow, and the status says that.
        let st = source.status();
        assert!(st.borrow().connected, "the daemon is there");
        let warnings = st.borrow().warnings.clone();
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("did not answer in time")),
            "the status says the daemon did not answer in time: {warnings:?}"
        );
        drop(source);
        daemon.join();
    }

    /// A daemon that answers with an error is not a daemon that is down.
    ///
    /// The daemon is there — it took the connection and it answered —
    /// and what it said is what the user is told. The line that says
    /// "no connection to the daemon" under it is a lie, and the daemon's
    /// own sentence is the one that belongs there.
    #[test]
    fn a_daemon_that_answers_with_an_error_is_not_a_daemon_that_is_down() {
        let daemon = serve(says_an_error);
        let mut source = DaemonSource::new(daemon.endpoint.clone(), None, Lang::English);
        let answer = source.search("invoice", 10);
        assert!(answer.is_empty(), "there was no answer");
        let st = source.status();
        assert!(st.borrow().connected, "the daemon is there: it answered");
        let warnings = st.borrow().warnings.clone();
        assert!(
            !warnings
                .iter()
                .any(|w| w.contains("no connection to the daemon")),
            "the daemon is not down: {warnings:?}"
        );
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("o índice está em modo somente-leitura")),
            "the daemon's own sentence is kept: {warnings:?}"
        );
        drop(source);
        daemon.join();
    }

    /// A daemon that is too slow is not a daemon that is down.
    ///
    /// The daemon took the connection and did not answer in the window:
    /// it is up and slow. The line that says "no connection to the daemon"
    /// under a daemon that is up is a lie, and the count, the layer and
    /// the time of the last answer still belong to the line.
    #[test]
    fn a_daemon_that_is_too_slow_is_not_a_daemon_that_is_down() {
        let daemon = serve(says_nothing);
        let mut source = DaemonSource::new(daemon.endpoint.clone(), None, Lang::English);
        let answer = source.search("invoice", 10);
        assert!(answer.is_empty(), "there was no answer");
        let st = source.status();
        assert!(
            st.borrow().connected,
            "the daemon took the connection: it is up and slow"
        );
        let warnings = st.borrow().warnings.clone();
        assert!(
            !warnings
                .iter()
                .any(|w| w.contains("no connection to the daemon")),
            "the daemon is not down: {warnings:?}"
        );
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("did not answer in time")),
            "the status says the daemon did not answer in time: {warnings:?}"
        );
        drop(source);
        daemon.join();
    }

    /// The message the host shows without a daemon does not promise the
    /// `rgx` fallback.
    ///
    /// The fallback needs a **scope**, and a scope comes from the daemon's
    /// name answer: without the daemon there was no name answer, so there
    /// is no scope, and the message says that instead of promising a
    /// search that cannot happen.
    #[test]
    fn the_message_without_a_daemon_does_not_promise_the_fallback() {
        let source = DaemonSource::new(
            Endpoint::Socket(std::path::PathBuf::from("/tmp/rgy-rgxy-nope.sock")),
            Some(String::from("/usr/bin/rgx")),
            Lang::English,
        );
        let message = source.no_daemon_message();
        assert!(message.contains("no daemon"), "{message}");
        assert!(
            !message.contains("will use rgx"),
            "the message does not promise a search that cannot happen: {message}"
        );
        assert!(
            message.contains("scope"),
            "the scope is what is missing: {message}"
        );
    }
}
