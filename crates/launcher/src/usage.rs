// SPDX-License-Identifier: Apache-2.0

//! What the user used, and what that does to the order.
//!
//! The daemon ranks by relevance and by fuzziness, and it is right to:
//! the ranking is the product. What the daemon cannot know is that the
//! user opened the same file yesterday — and a launcher that forgets it
//! makes the user pick the same file again every morning.
//!
//! So the history is folded into the order, and the fold is a **pure
//! function**: the daemon's order in, the order the user sees out. The
//! history itself is a file, and the file is the binary's business —
//! this crate does not touch the machine.
//!
//! # The policy
//!
//! * The daemon's order is the order under the history: a result the user
//!   used inside the recent window comes up **at most three places**, and
//!   a result the history does not raise ends where the daemon put it or
//!   lower.
//! * A use counts as recent for a month. A file opened last year is not
//!   what the user is working on now.
//!
//! # Why the rise is small
//!
//! The history is a hint, and a hint that takes over is a lie: the user
//! asked for the best match and gets the file they opened in March. The
//! rise is a nudge that says "this one, recently" — it moves a result up
//! three places at the most, and the daemon's order is still the order
//! under it.

use alloc::string::String;
use alloc::string::ToString;
use alloc::vec::Vec;

use crate::source::Answer;

/// How many places a used result may rise.
pub const MAX_RISE: usize = 3;

/// How long a use counts as recent, in seconds.
pub const RECENT_SECONDS: u64 = 30 * 24 * 60 * 60;

/// How many paths the history keeps.
///
/// The history is a memory and not an archive: the oldest uses are the
/// first to go, because they are the least likely to be what the user is
/// working on now.
pub const MAX_ENTRIES: usize = 512;

/// One path's history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The path, as the daemon spelled it.
    pub path: String,
    /// How many times the user opened it.
    pub uses: u32,
    /// When it was last opened, in seconds since the Unix epoch.
    pub last: u64,
}

/// What the user used, and when.
///
/// The history is the launcher's memory, and it is small: a list of the
/// paths the user opened, how many times, and when. The lookup is a scan,
/// because the list is short enough for a scan to be the cheapest thing
/// that can be right.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct History {
    entries: Vec<Entry>,
}

impl History {
    /// A history with nothing in it.
    pub fn new() -> History {
        History {
            entries: Vec::new(),
        }
    }

    /// How many paths the history knows.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the history knows nothing.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// What the history knows.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The history of one path.
    pub fn get(&self, path: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.path == path)
    }

    /// Record that the user opened `path`, now.
    pub fn record(&mut self, path: &str, now: u64) {
        match self.entries.iter_mut().find(|entry| entry.path == path) {
            Some(entry) => {
                entry.uses = entry.uses.saturating_add(1);
                entry.last = now;
            }
            None => self.insert(path, 1, now),
        }
        self.trim();
    }

    /// Put a path in the history, with the numbers the store carried.
    ///
    /// This is what the file says, not what the user did just now: a
    /// store that was read back has to land as it was written.
    pub fn insert(&mut self, path: &str, uses: u32, last: u64) {
        match self.entries.iter_mut().find(|entry| entry.path == path) {
            Some(entry) => {
                entry.uses = uses;
                entry.last = last;
            }
            None => self.entries.push(Entry {
                path: String::from(path),
                uses,
                last,
            }),
        }
        self.trim();
    }

    /// How many places the path may rise, now.
    ///
    /// Zero for a path the history does not know, and zero for one whose
    /// last use is old: the rise is a nudge about *now*, and an old use
    /// says nothing about now.
    pub fn rise(&self, path: &str, now: u64) -> usize {
        let Some(entry) = self.get(path) else {
            return 0;
        };
        if !is_recent(entry, now) {
            return 0;
        }
        match entry.uses {
            0 => 0,
            1 => 1,
            2..=4 => 2,
            _ => MAX_RISE,
        }
    }

    /// The order the history asks for: the indexes of `paths`, in the
    /// order the user should see them.
    ///
    /// The daemon's order is the order under the history, and the history
    /// only ever **claims places**: a result takes at most its own rise,
    /// and the results it passes are pushed down, never out.
    pub fn order(&self, paths: &[String], now: u64) -> Vec<usize> {
        let mut order: Vec<usize> = Vec::with_capacity(paths.len());
        for (index, path) in paths.iter().enumerate() {
            let rise = self.rise(path, now);
            // The claim is bounded by the rise, so no result ever moves up
            // more than its budget — and a result with no budget moves up
            // not at all.
            let at = index.saturating_sub(rise);
            order.insert(at, index);
        }
        order
    }

    /// The answer, with the history folded into its order.
    ///
    /// The spans, the contexts and the folder flags travel with the paths:
    /// a row that shows the matched characters of the wrong file is worse
    /// than a row with no highlight at all, and a folder that lost its
    /// flag is a folder with a file's icon.
    pub fn reorder(&self, answer: Answer, now: u64) -> Answer {
        let order = self.order(&answer.paths, now);
        if order.iter().enumerate().all(|(at, &index)| at == index) {
            // The history has nothing to say about these results.
            return answer;
        }
        let paths = order
            .iter()
            .map(|&index| answer.paths[index].clone())
            .collect();
        let spans = order
            .iter()
            .map(|&index| answer.span(index).to_vec())
            .collect();
        let contexts = order
            .iter()
            .map(|&index| answer.context(index).to_string())
            .collect();
        let folders = order.iter().map(|&index| answer.folder(index)).collect();
        Answer {
            paths,
            spans,
            contexts,
            folders,
        }
    }

    /// The oldest uses go first.
    fn trim(&mut self) {
        while self.entries.len() > MAX_ENTRIES {
            let oldest = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.last)
                .map(|(index, _)| index);
            match oldest {
                Some(index) => {
                    self.entries.remove(index);
                }
                None => break,
            }
        }
    }
}

/// Whether a use still counts as recent, now.
pub fn is_recent(entry: &Entry, now: u64) -> bool {
    if entry.uses == 0 {
        return false;
    }
    // The clock can go backwards, and a clock that went backwards must
    // not make a use look old.
    now.saturating_sub(entry.last) <= RECENT_SECONDS
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;

    fn paths(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn no_history_keeps_the_order() {
        let history = History::new();
        let list = paths(&["a.txt", "b.txt", "c.txt"]);
        assert_eq!(history.order(&list, 1_000), vec![0, 1, 2]);
    }

    #[test]
    fn a_history_of_another_path_keeps_the_order() {
        // The history is about the paths it knows: a file the user opened
        // last week says nothing about the results in front of them.
        let mut history = History::new();
        history.record("z.txt", 1_000);
        let list = paths(&["a.txt", "b.txt", "c.txt"]);
        assert_eq!(history.order(&list, 1_000), vec![0, 1, 2]);
    }

    #[test]
    fn a_recently_used_result_rises() {
        let mut history = History::new();
        history.record("b.txt", 1_000);
        let list = paths(&["a.txt", "b.txt", "c.txt"]);
        assert_eq!(history.order(&list, 1_000), vec![1, 0, 2]);
    }

    #[test]
    fn a_result_rises_by_at_most_three_places() {
        let mut history = History::new();
        for _ in 0..5 {
            history.record("h.txt", 1_000);
        }
        let names = [
            "a.txt", "b.txt", "c.txt", "d.txt", "e.txt", "f.txt", "g.txt", "h.txt",
        ];
        let list = paths(&names);
        let order = history.order(&list, 1_000);
        let at = order.iter().position(|&index| index == 7);
        // The last of eight, three places up: it did not reach the top,
        // because a use is a nudge and not a takeover.
        assert_eq!(at, Some(4), "three places up and no more: {order:?}");
    }

    #[test]
    fn a_riser_comes_up_three_places_and_what_it_passes_goes_down() {
        let mut history = History::new();
        for _ in 0..5 {
            history.record("e.txt", 1_000);
        }
        let list = paths(&["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"]);
        let order = history.order(&list, 1_000);
        let at = |name: &str| {
            order
                .iter()
                .position(|&index| list[index] == name)
                .expect("the result is in the order")
        };
        // The daemon put e.txt fifth; the history put it second.
        assert_eq!(at("e.txt"), 1, "three places up from fifth: {order:?}");
        // The three results it passed each went down one place, and the
        // result the daemon put first kept its place: the rise is bounded
        // by the number of results above it.
        assert_eq!(at("a.txt"), 0, "the daemon's first result stays: {order:?}");
        assert_eq!(at("b.txt"), 2, "one place down: {order:?}");
        assert_eq!(at("c.txt"), 3, "one place down: {order:?}");
        assert_eq!(at("d.txt"), 4, "one place down: {order:?}");
    }

    #[test]
    fn a_riser_that_reaches_the_top_pushes_the_daemons_best_match_down() {
        let mut history = History::new();
        for _ in 0..5 {
            history.record("d.txt", 1_000);
        }
        let list = paths(&["a.txt", "b.txt", "c.txt", "d.txt"]);
        let order = history.order(&list, 1_000);
        let shown: Vec<&str> = order.iter().map(|&index| list[index].as_str()).collect();
        // The daemon's best match went down one place: the history is a
        // nudge on top of the daemon's order, and a nudge that took the
        // first place would be a takeover.
        assert_eq!(shown, vec!["d.txt", "a.txt", "b.txt", "c.txt"]);
    }

    #[test]
    fn a_result_used_long_ago_does_not_rise() {
        let mut history = History::new();
        history.record("b.txt", 1_000);
        let list = paths(&["a.txt", "b.txt", "c.txt"]);
        let now = 1_000 + RECENT_SECONDS + 1;
        assert_eq!(history.order(&list, now), vec![0, 1, 2]);
    }

    #[test]
    fn a_use_at_the_edge_of_the_window_still_rises() {
        let mut history = History::new();
        history.record("b.txt", 1_000);
        let list = paths(&["a.txt", "b.txt", "c.txt"]);
        let now = 1_000 + RECENT_SECONDS;
        assert_eq!(history.order(&list, now), vec![1, 0, 2]);
    }

    #[test]
    fn the_rise_grows_with_the_number_of_uses() {
        let mut history = History::new();
        history.record("b.txt", 1_000);
        assert_eq!(history.rise("b.txt", 1_000), 1);
        history.record("b.txt", 1_000);
        assert_eq!(history.rise("b.txt", 1_000), 2);
        for _ in 0..3 {
            history.record("b.txt", 1_000);
        }
        assert_eq!(history.rise("b.txt", 1_000), MAX_RISE);
    }

    #[test]
    fn a_path_the_history_does_not_know_does_not_rise() {
        let history = History::new();
        assert_eq!(history.rise("a.txt", 1_000), 0);
    }

    #[test]
    fn the_history_is_the_store_that_was_written() {
        let mut history = History::new();
        history.record("a.txt", 1_000);
        history.record("a.txt", 2_000);
        let entry = history.get("a.txt").expect("the path is there");
        assert_eq!(entry.uses, 2);
        assert_eq!(entry.last, 2_000);
    }

    #[test]
    fn the_history_forgets_the_oldest_when_it_is_full() {
        let mut history = History::new();
        for i in 0..(MAX_ENTRIES + 10) {
            history.insert(&alloc::format!("/a/file{i}.txt"), 1, i as u64);
        }
        assert_eq!(history.len(), MAX_ENTRIES);
        assert!(
            history.get("/a/file0.txt").is_none(),
            "the oldest use is the first to go"
        );
        assert!(history.get("/a/file521.txt").is_some());
    }

    #[test]
    fn the_answer_keeps_its_spans_with_its_paths() {
        let mut history = History::new();
        history.record("b.txt", 1_000);
        let answer = Answer {
            paths: paths(&["a.txt", "b.txt"]),
            spans: vec![vec![0], vec![1, 2]],
            contexts: Vec::new(),
            folders: vec![false, true],
        };
        let reordered = history.reorder(answer, 1_000);
        assert_eq!(reordered.paths, paths(&["b.txt", "a.txt"]));
        assert_eq!(reordered.spans, vec![vec![1, 2], vec![0]]);
        // The folder flag travels too: a folder that lost it is a folder
        // with a file's icon.
        assert_eq!(reordered.folders, vec![true, false]);
    }

    #[test]
    fn an_answer_the_history_has_nothing_to_say_about_is_untouched() {
        let history = History::new();
        let answer = Answer {
            paths: paths(&["a.txt", "b.txt"]),
            spans: vec![vec![0], vec![1]],
            contexts: Vec::new(),
            folders: vec![false, false],
        };
        let reordered = history.reorder(answer.clone(), 1_000);
        assert_eq!(reordered, answer);
    }
}
