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

    /// The daemon this source talks to.
    ///
    /// The window shows it in its status line: a launcher that answers
    /// and does not say where it is answering from is a launcher that
    /// cannot be debugged.
    #[cfg(windows)]
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// The `rgx` binary, when the machine has one.
    #[cfg(windows)]
    pub fn rgx(&self) -> Option<&str> {
        self.rgx.as_deref()
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
    fn fail(&mut self, reason: String) {
        self.client = None;
        let mut st = self.status.borrow_mut();
        st.connected = false;
        st.warnings = vec![reason];
    }

    /// The names the content layer reads.
    ///
    /// The search is made **without** the session: the session belongs to
    /// the window's own question, and a scope search that shared it would
    /// make the daemon re-rank the window's results against the scope.
    fn scope_paths(&mut self, pattern: &str) -> Vec<String> {
        let saved = self.session;
        if let Some(client) = self.client.as_mut() {
            client.set_session(None);
        }
        let answer = self.client_mut().and_then(|client| {
            client.search(pattern, SCOPE_LIMIT).ok().map(|out| {
                out.hits
                    .into_iter()
                    .map(|h| h.path)
                    .collect::<Vec<String>>()
            })
        });
        if let Some(client) = self.client.as_mut() {
            client.set_session(saved);
        }
        answer.unwrap_or_default()
    }

    /// The names, from the daemon.
    fn search_names(&mut self, query: &str, limit: usize) -> Answer {
        let Some(client) = self.client_mut() else {
            return Answer::empty();
        };
        match client.search(query, limit) {
            Ok(outcome) => {
                let answer = Answer {
                    paths: outcome.hits.iter().map(|h| h.path.clone()).collect(),
                    spans: outcome.hits.iter().map(|h| h.span.clone()).collect(),
                    contexts: Vec::new(),
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
                self.fail(format!("a busca de nomes falhou: {e}"));
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
    fn search_content(&mut self, names: &str, pattern: &str, limit: usize) -> Answer {
        // The daemon first, connected on demand: the launcher is resident
        // and the daemon may have come up since the last keystroke.
        let connected = self.client_mut().is_some();
        if connected {
            let scope = if names.is_empty() {
                // No scope: the whole tree.
                Vec::new()
            } else {
                self.scope_paths(names)
            };
            if names.is_empty() {
                if let Some(answer) = self.whole_tree_content(pattern, limit) {
                    return answer;
                }
            } else if !scope.is_empty()
                && let Some(answer) = self.daemon_content(pattern, &scope, limit)
            {
                // The daemon answered with a failure: the fallback below
                // may still answer, and a launcher that says "no results"
                // when the machine can answer is a launcher that lies.
                return answer;
            }
        }
        // The fallback: the `rgx` binary, over the scope the names chose.
        //
        // The scope is the **last** one — the files that were on the
        // screen when the daemon went away. It is not the whole tree: a
        // content search over a tree is a walk, and a launcher that
        // blocks the window for minutes is a launcher nobody uses.
        let scope = self.last_scope.clone();
        if !scope.is_empty()
            && let Some(binary) = self.rgx.clone()
        {
            return self.rgx_content(&binary, pattern, &scope, limit);
        }
        let mut st = self.status.borrow_mut();
        st.layer = "conteúdo";
        st.matches = 0;
        st.connected = false;
        st.warnings = vec![rgy_launcher::tools::content_unavailable(false, self.rgx.is_some())];
        Answer::empty()
    }

    /// The content answer over the **whole tree**.
    ///
    /// The daemon's own content engine answers it: the pack store when
    /// the daemon was started with one, and the trigram index when it was
    /// not. It is the answer to "which files say this", with no scope.
    fn whole_tree_content(&mut self, pattern: &str, limit: usize) -> Option<Answer> {
        let client = self.client_mut()?;
        let outcome = match client.grep(pattern, limit) {
            Ok(outcome) => outcome,
            Err(e) => {
                self.fail(format!("a busca de conteúdo falhou: {e}"));
                return None;
            }
        };
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
        Some(answer)
    }

    /// The content answer, from the daemon.
    ///
    /// `None` when the daemon could not answer: the caller then tries the
    /// fallback.
    fn daemon_content(&mut self, pattern: &str, scope: &[String], limit: usize) -> Option<Answer> {
        let client = self.client_mut()?;
        let outcome = match client.content(pattern, scope, limit) {
            Ok(outcome) => outcome,
            Err(e) => {
                self.fail(format!("a busca de conteúdo falhou: {e}"));
                return None;
            }
        };
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
        Some(answer)
    }

    /// The content answer, from the `rgx` binary.
    ///
    /// The scope is the files the names chose — never the tree. The binary
    /// is a fallback for a daemon that went away, and the point of a
    /// fallback is to answer the question that was asked, not a bigger one.
    fn rgx_content(
        &mut self,
        binary: &str,
        pattern: &str,
        scope: &[String],
        limit: usize,
    ) -> Answer {
        match crate::rgx::grep(binary, pattern, scope) {
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
        self.search_content(split.names, split.content.unwrap_or(""), limit)
    }
}

#[cfg(test)]
mod tests {
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
}
