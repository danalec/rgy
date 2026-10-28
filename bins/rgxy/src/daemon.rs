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

use rgy_launcher::{Answer, Query, Source, Split};
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
/// The launcher asks from the thread that draws the window, and the
/// client's own window is thirty seconds — generous for a program that
/// waits for an answer in the background, and far too long for a search
/// box: thirty seconds of silence is a window that stopped answering the
/// keyboard. Two seconds is long enough for the daemon to answer a warm
/// index (which is microseconds) and for a cold one to be worth waiting
/// for, and short enough that the launcher is still a launcher when the
/// daemon is slow or stuck.
const LAUNCHER_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);

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
    /// Which layer answered: `nomes` or `conteúdo`.
    pub layer: &'static str,
    /// Where the content layer's answers come from.
    pub backend: String,
    /// Whether the daemon is there.
    pub connected: bool,
    /// Whether the daemon has a pack store (`--tv-dir`).
    pub tv: bool,
    /// How many results there are.
    pub matches: usize,
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
}

impl DaemonSource {
    /// A source that asks the daemon at `endpoint`.
    pub fn new(endpoint: Endpoint, rgx: Option<String>) -> DaemonSource {
        DaemonSource {
            client: None,
            endpoint,
            session: None,
            last: Answer::empty(),
            last_scope: Vec::new(),
            status: Rc::new(RefCell::new(Status::default())),
            rgx,
        }
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
        if self.rgx.is_some() {
            "o rgx só responde sobre o escopo de uma busca de nomes, e sem o daemon não houve nenhuma"
        } else {
            "a camada de conteúdo precisa do daemon (searchd) ou do rgx no PATH"
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
        format!(
            "sem daemon em {}; {}",
            self.endpoint.describe(),
            self.no_content_reason()
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
    pub fn connect(&mut self) -> bool {
        let client = match Client::connect(&self.endpoint) {
            Ok(c) => c,
            Err(e) => {
                let mut st = self.status.borrow_mut();
                st.connected = false;
                st.warnings = vec![format!("{}: {e}", rgy_launcher::tools::NO_DAEMON)];
                return false;
            }
        };
        self.client = Some(client);
        if let Some(client) = self.client.as_mut() {
            client.set_session(self.session);
        }
        // The status is a separate question: a daemon that answers it is
        // a daemon that is up.
        if let Some(client) = self.client.as_mut()
            && let Ok(status) = client.status()
        {
            let mut st = self.status.borrow_mut();
            st.connected = true;
            st.tv = status.tv;
        }
        true
    }

    /// The connection, made on demand.
    ///
    /// A client that gives up when the daemon is not there yet would fail
    /// for good; the launcher is resident, and the daemon may come later.
    fn client_mut(&mut self) -> Option<&mut Client> {
        if self.client.is_none() {
            let client = match Client::connect(&self.endpoint) {
                Ok(client) => client,
                Err(e) => {
                    let mut st = self.status.borrow_mut();
                    st.connected = false;
                    st.warnings = vec![format!("{}: {e}", rgy_launcher::tools::NO_DAEMON)];
                    return None;
                }
            };
            self.client = Some(client);
            if let Some(client) = self.client.as_mut() {
                client.set_session(self.session);
            }
            self.status.borrow_mut().connected = true;
        }
        self.client.as_mut()
    }

    /// The answer failed: the connection is dropped, and the sentence is
    /// kept for the window.
    fn fail(&mut self, reason: &str) {
        self.client = None;
        let mut st = self.status.borrow_mut();
        st.connected = false;
        st.warnings = vec![reason.to_string()];
    }

    /// The names the content layer reads.
    ///
    /// The search is made **without** the session: the session belongs to
    /// the window's own question, and a scope search that shared it would
    /// make the daemon re-rank the window's results against the scope.
    ///
    /// The argument is the query in the **engine's** words — the caller
    /// rewrites it with `rgy_launcher::layer::scope_query` first. The
    /// engine fails closed on a word it does not know, so a scope sent as
    /// `doc:` comes back empty and the content layer answers "nothing
    /// found" for a file that is on the disk.
    ///
    /// The window is the **launcher's**: the scope is asked on the thread
    /// that draws the window, and a scope that takes thirty seconds is a
    /// window that stopped answering the keyboard.
    ///
    /// `Err` is the daemon's reason. A daemon that stopped answering is
    /// not a daemon that chose nothing, and the caller has to say which
    /// one it was.
    fn scope_paths(&mut self, engine_query: &str) -> Result<Vec<String>, String> {
        let saved = self.session;
        if let Some(client) = self.client.as_mut() {
            client.set_session(None);
        }
        let answer = match self.client.as_mut() {
            Some(client) => client
                .search_within(engine_query, SCOPE_LIMIT, LAUNCHER_WINDOW)
                .map(|out| {
                    out.hits
                        .into_iter()
                        .map(|h| h.path)
                        .collect::<Vec<String>>()
                })
                .map_err(|e| format!("a busca de nomes falhou: {e}")),
            None => Err(String::from(rgy_launcher::tools::NO_DAEMON)),
        };
        if let Some(client) = self.client.as_mut() {
            client.set_session(saved);
        }
        answer
    }

    /// The names, from the daemon.
    fn search_names(&mut self, query: &str, limit: usize) -> Answer {
        let Some(client) = self.client_mut() else {
            return Answer::empty();
        };
        match client.search_within(query, limit, LAUNCHER_WINDOW) {
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
                    st.warnings = outcome.warnings.clone();
                    st.took_us = outcome.took_us;
                    st.layer = "nomes";
                    st.matches = answer.len();
                }
                // The scope the fallback will use if the daemon goes away.
                self.last_scope = answer.paths.clone();
                self.last = answer.clone();
                answer
            }
            Err(e) => {
                self.fail(&format!("a busca de nomes falhou: {e}"));
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
    /// `names` is the scope: empty means the **whole tree**, and the
    /// daemon's own content engine answers that one — the pack store when
    /// there is one, the trigram index when there is not.
    ///
    /// `content` is the content part as the user typed it. It is rebuilt
    /// here, in the engine's words, before anything goes on the wire.
    fn search_content(&mut self, names: &str, content: &str, limit: usize) -> Answer {
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
        // The daemon first, connected on demand: the launcher is resident
        // and the daemon may have come up since the last keystroke.
        let connected = self.client_mut().is_some();
        // The scope is the names part of the query, in the **engine's**
        // words. `doc:` is the launcher's word and `type:doc` is the
        // engine's, and the engine fails closed on a word it does not
        // know — so a scope sent as the user typed it comes back empty
        // and the content layer answers "nothing found" for a file that
        // is on the disk.
        //
        // The failure, when there is one, is the reason the content layer
        // has no answer: a daemon that stopped answering is not a daemon
        // that chose nothing.
        let mut failure: Option<String> = None;
        let mut scope = Vec::new();
        if connected && !names.is_empty() {
            match self.scope_paths(&rgy_launcher::layer::scope_query(names)) {
                Ok(paths) => scope = paths,
                Err(reason) => {
                    self.fail(&reason);
                    failure = Some(reason);
                }
            }
        }
        if connected {
            if names.is_empty() {
                // The whole tree: the daemon's own content engine.
                match self.whole_tree_content(&content, limit) {
                    Ok(answer) => return answer,
                    Err(reason) => {
                        self.fail(&reason);
                        failure = Some(reason);
                    }
                }
            } else if !scope.is_empty() {
                match self.daemon_content(&content, &scope, limit) {
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
        let scope = if connected {
            scope
        } else {
            self.last_scope.clone()
        };
        if !scope.is_empty()
            && let Some(binary) = self.rgx.clone()
        {
            return self.rgx_content(&binary, &content, &scope, limit);
        }
        // Nothing answered, and the status says why. The reason is one of
        // three, and they are three different facts about the machine: the
        // daemon failed while answering, the daemon is not there, or the
        // daemon is there and the name part chose nothing to read.
        let daemon_up = self.status.borrow().connected;
        let (connected, warning) = match failure {
            // The daemon's own sentence is the reason: it failed, and this
            // function did not.
            Some(reason) => (false, reason),
            // The daemon is up and answering: the name part chose nothing,
            // so there was nothing to read. Saying the daemon is down here
            // would be a lie — it is answering.
            None if daemon_up => (
                true,
                String::from("a parte dos nomes deu nenhum resultado; não há ficheiros para ler"),
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
        st.layer = "conteúdo";
        st.matches = 0;
        st.connected = connected;
        st.warnings = vec![warning];
        Answer::empty()
    }

    /// The content answer over the **whole tree**.
    ///
    /// The daemon's own content engine answers it: the pack store when
    /// the daemon was started with one, and the trigram index when it was
    /// not. It is the answer to "which files say this", with no scope.
    ///
    /// The window is the **launcher's**: the answer reads the tree, and a
    /// window that waits thirty seconds for it is a window that stopped
    /// answering the keyboard.
    ///
    /// `Err` is the daemon's reason: the caller drops the connection and
    /// says it.
    fn whole_tree_content(&mut self, content: &str, limit: usize) -> Result<Answer, String> {
        let Some(client) = self.client_mut() else {
            return Err(String::from(rgy_launcher::tools::NO_DAEMON));
        };
        let outcome = client
            .grep_within(content, limit, LAUNCHER_WINDOW)
            .map_err(|e| format!("a busca de conteúdo falhou: {e}"))?;
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
        let backend = self.backend_label();
        {
            let mut st = self.status.borrow_mut();
            st.connected = true;
            st.warnings = outcome.warnings.clone();
            st.took_us = outcome.took_us;
            st.layer = "conteúdo";
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
        limit: usize,
    ) -> Result<Answer, String> {
        let Some(client) = self.client_mut() else {
            return Err(String::from(rgy_launcher::tools::NO_DAEMON));
        };
        let outcome = client
            .content_within(content, scope, limit, LAUNCHER_WINDOW)
            .map_err(|e| format!("a busca de conteúdo falhou: {e}"))?;
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
        // The label is read before the status is written: the status is
        // one cell, and a cell that is borrowed twice at once is a panic.
        let backend = self.backend_label();
        {
            let mut st = self.status.borrow_mut();
            st.connected = true;
            st.warnings = outcome.warnings.clone();
            st.took_us = outcome.took_us;
            st.layer = "conteúdo";
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
    ) -> Answer {
        match crate::rgx::grep(binary, content, scope) {
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
                let backend = self.backend_label();
                {
                    let mut st = self.status.borrow_mut();
                    st.warnings = Vec::new();
                    st.layer = "conteúdo";
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

    /// The word the status line shows for the content layer's backend.
    fn backend_label(&self) -> String {
        let daemon_up = self.status.borrow().connected;
        let backend = rgy_launcher::content_backend(daemon_up, self.rgx.is_some());
        backend.label().to_string()
    }
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
    fn search(&mut self, query: &str, limit: usize) -> Answer {
        let split = Split::of(query);
        if !split.is_content() {
            // The launcher's own words become the engine's words.
            let mut q = Query::new();
            q.set_text(split.names.to_string());
            return self.search_names(&q.engine_text(), limit);
        }
        if split.pattern().is_empty() {
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
        self.search_content(split.names, split.content.unwrap_or(""), limit)
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
        );
        let answer = source.search("invoice", 10);
        assert!(answer.is_empty());
        let st = source.status();
        assert!(!st.borrow().connected);
        assert!(!st.borrow().warnings.is_empty());
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

    /// A daemon that takes one client and does what the test says.
    #[cfg(unix)]
    fn serve(script: fn(Peer)) -> Daemon {
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
    fn serve(script: fn(Peer)) -> Daemon {
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

    /// A daemon that chooses nothing is not a daemon that is down.
    ///
    /// The name part of `invoice grep:zebra` matched nothing, so there was
    /// nothing to read: the daemon is up and it answered. The status line
    /// that says "no daemon" under a running daemon is a lie, and it is
    /// the lie this test pins.
    #[test]
    fn a_daemon_that_chooses_nothing_is_not_a_daemon_that_is_down() {
        let daemon = serve(answers_with_no_names);
        let mut source = DaemonSource::new(daemon.endpoint.clone(), None);
        assert!(source.connect(), "the daemon is there");
        let answer = source.search("invoice grep:zebra", 10);
        assert!(answer.is_empty(), "there was nothing to read");
        let st = source.status();
        assert!(st.borrow().connected, "the daemon is answering");
        let warnings = st.borrow().warnings.clone();
        assert!(
            !warnings.iter().any(|w| w.contains("sem ligação ao daemon")),
            "the daemon is up: {warnings:?}"
        );
        assert!(
            warnings.iter().any(|w| w.contains("nenhum resultado")),
            "the status says the names chose nothing: {warnings:?}"
        );
        // The daemon waits for the client to go.
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
        );
        let message = source.no_daemon_message();
        assert!(message.contains("sem daemon"), "{message}");
        assert!(
            !message.contains("usará o rgx"),
            "the message does not promise a search that cannot happen: {message}"
        );
        assert!(
            message.contains("escopo"),
            "the scope is what is missing: {message}"
        );
    }
}
