// SPDX-License-Identifier: Apache-2.0

//! What the user typed, and the engine's version of it.
//!
//! The launcher has a small vocabulary of its own — `file:`, `folder:`,
//! `doc:`, `pic:`, `video:`, `audio:` — and the engine has a larger one.
//! The launcher rewrites its words into the engine's and sends that; the
//! user never sees the rewrite.
//!
//! # Why the rewrite and not new filters
//!
//! The engine already has a `folder:` and it means the folder **axis** —
//! "the name matches in one of the path's folders" — and it is exercised
//! by a dozen tests and by the manual. A second `folder:` that meant
//! "folders only" would either break that or need a rule about which one
//! wins, and a rule that depends on the value is a rule a reader cannot
//! check. So the short words are the launcher's, and the launcher is the
//! only thing that has to know them.
//!
//! The launcher's `folder:` therefore means **a directory**: it is a type
//! filter, and it becomes the engine's `type:dir`. The engine's own
//! `folder:` axis — "the folder the file is in matches" — is a different
//! question, and it is not reachable from the launcher: the word is
//! rewritten before the engine sees the query.
//!
//! The rewrite is a whole-word substitution, and it only fires on the bare
//! word: `pic:` is the filter, and `pic:png` is not — that is an unknown
//! prefix, and the engine fails it closed rather than guessing.
//!
//! # The caret
//!
//! The caret is counted in **characters**, not bytes: the caret is between
//! characters, and deleting half of an emoji is not a thing a person can
//! undo. The byte offset is computed when the text is touched, so the hot
//! path — typing — is one push and one offset.

use alloc::string::String;
use alloc::vec::Vec;

/// The short filters the launcher offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortFilter {
    /// `file:` — files, and not folders.
    File,
    /// `folder:` — folders, and not files.
    ///
    /// In the launcher `folder:` is a **type filter**: it means "a
    /// directory", and it is rewritten into the engine's `type:dir`
    /// before the engine ever sees the query. The engine has a `folder:`
    /// of its own, and there it is an **axis** — "the folder the file is
    /// in matches" — which is a different question. The launcher's word
    /// wins here, and the engine's axis is not reachable from the
    /// launcher: a query that meant it would have to keep the word
    /// whole, and the launcher rewrites it.
    Folder,
    /// `doc:` — the document extension set.
    Doc,
    /// `pic:` — the image extension set.
    Pic,
    /// `video:` — the video extension set.
    Video,
    /// `audio:` — the audio extension set.
    Audio,
}

impl ShortFilter {
    /// The short word, with its colon.
    pub fn word(&self) -> &'static str {
        match self {
            ShortFilter::File => "file:",
            ShortFilter::Folder => "folder:",
            ShortFilter::Doc => "doc:",
            ShortFilter::Pic => "pic:",
            ShortFilter::Video => "video:",
            ShortFilter::Audio => "audio:",
        }
    }

    /// The engine's word for the same thing.
    ///
    /// The names the engine's `type:` filter knows: `file` and `dir` are
    /// kinds, and the rest are the extension sets.
    pub fn engine_word(&self) -> &'static str {
        match self {
            ShortFilter::File => "type:file",
            ShortFilter::Folder => "type:dir",
            ShortFilter::Doc => "type:doc",
            ShortFilter::Pic => "type:image",
            ShortFilter::Video => "type:video",
            ShortFilter::Audio => "type:audio",
        }
    }

    /// The filter a bare word names, if it names one.
    pub fn of_word(word: &str) -> Option<ShortFilter> {
        match word {
            "file:" => Some(ShortFilter::File),
            "folder:" => Some(ShortFilter::Folder),
            "doc:" => Some(ShortFilter::Doc),
            "pic:" => Some(ShortFilter::Pic),
            "video:" => Some(ShortFilter::Video),
            "audio:" => Some(ShortFilter::Audio),
            _ => None,
        }
    }

    /// The filter a word starts with, if it starts with one.
    ///
    /// The short word is a **prefix**, not a whole word: `doc:invoice` is
    /// the filter *and* a name — "the documents called invoice" — and the
    /// engine reads the two as separate words.
    ///
    /// The prefix has to be the whole head of the word: `files:` is not
    /// `file:`, and the engine fails it closed, which is the right answer
    /// for a word it does not know.
    pub fn of_prefix(word: &str) -> Option<(ShortFilter, &str)> {
        for filter in ShortFilter::all() {
            if let Some(rest) = word.strip_prefix(filter.word()) {
                return Some((*filter, rest));
            }
        }
        None
    }

    /// Every short filter, in the order the launcher offers them.
    pub fn all() -> &'static [ShortFilter] {
        &[
            ShortFilter::File,
            ShortFilter::Folder,
            ShortFilter::Doc,
            ShortFilter::Pic,
            ShortFilter::Video,
            ShortFilter::Audio,
        ]
    }
}

/// What the user typed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    text: String,
    /// The caret, counted in characters from the start of the text.
    caret: usize,
}

impl Query {
    /// An empty query.
    pub fn new() -> Query {
        Query {
            text: String::new(),
            caret: 0,
        }
    }

    /// The query, as the user typed it, from a text.
    ///
    /// The caret goes to the end: the text arrived from somewhere else —
    /// a command line, a daemon's question — and there is no position in
    /// it to be remembered.
    pub fn of(text: &str) -> Query {
        let mut query = Query::new();
        query.set_text(String::from(text));
        query
    }

    /// The text, as the user typed it.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The caret, counted in characters from the start.
    pub fn caret(&self) -> usize {
        self.caret
    }

    /// Replace the text.
    ///
    /// The caret goes to the end: the text arrived from somewhere else —
    /// a paste, a command line — and there is no position in it to be
    /// remembered.
    pub fn set_text(&mut self, text: String) {
        self.caret = text.chars().count();
        self.text = text;
    }

    /// Type one character.
    ///
    /// The character goes **at the caret**, not at the end: a caret that
    /// cannot be in the middle is a caret that is a decoration.
    pub fn insert(&mut self, ch: char) {
        let at = self.byte_offset(self.caret);
        self.text.insert(at, ch);
        self.caret += 1;
    }

    /// Type a whole string, the way a paste arrives.
    ///
    /// The text goes **at the caret** and the caret walks with it: a paste
    /// is a hundred keystrokes that arrived at once, and a caret that
    /// stays where it was is a caret that lies about where the text is.
    ///
    /// It goes in **one** piece. Typing it character by character moves
    /// the whole text once per character, and a paste of a hundred
    /// kilobytes is a hundred thousand moves of a hundred kilobytes: a
    /// launcher that spends two seconds on a paste is a launcher that
    /// looks hung, and one that spends minutes on a megabyte never comes
    /// back.
    pub fn insert_str(&mut self, text: &str) {
        let at = self.byte_offset(self.caret);
        self.text.insert_str(at, text);
        // The caret counts **characters**: the text may be longer in bytes
        // than in characters, and a caret that counts bytes walks into
        // the middle of an accent.
        self.caret += text.chars().count();
    }

    /// Delete the character before the caret.
    ///
    /// False when there was nothing to delete.
    pub fn backspace(&mut self) -> bool {
        if self.caret == 0 {
            return false;
        }
        let at = self.byte_offset(self.caret - 1);
        // A character, not a byte: the caret is between characters and
        // deleting half of an emoji is not a thing a person can undo.
        let mut cut = at;
        while cut > 0 && !self.text.is_char_boundary(cut) {
            cut -= 1;
        }
        self.text.remove(cut);
        self.caret -= 1;
        true
    }

    /// Delete the character at the caret.
    ///
    /// The **forward** deletion: `Delete` takes the character under the
    /// caret and `Backspace` the one before it. The caret stays where it
    /// is — the character that came after it took the place, and the caret
    /// is still between the two.
    ///
    /// False when there was nothing to delete.
    pub fn delete_forward(&mut self) -> bool {
        let at = self.byte_offset(self.caret);
        if at >= self.text.len() {
            return false;
        }
        // The whole character: `remove` takes the one the caret is on, and
        // half of an emoji is not a thing a person can undo.
        self.text.remove(at);
        true
    }

    /// Delete the word before the caret.
    ///
    /// The caret stays where the deletion happened: the word's start, which
    /// is where the text before it ends now. A caret that jumps to the end
    /// of the text is a caret that answers a question the user did not
    /// ask — they are editing in the middle of the line, and the line
    /// keeps going after the caret.
    pub fn delete_word(&mut self) -> bool {
        let head_len = self.byte_offset(self.caret);
        let head = &self.text[..head_len];
        let trimmed = head.trim_end();
        let cut = trimmed.rfind(' ').map(|i| i + 1).unwrap_or(0);
        if cut >= head.len() {
            return false;
        }
        // The word goes, and the text **after** the caret stays: the caret
        // is in the middle of the text and the user is editing there.
        self.text.replace_range(cut..head_len, "");
        // The cut is a boundary in the text that is left: the deletion was
        // after it, and the part in front of it did not move.
        self.caret = self.text[..cut].chars().count();
        true
    }

    /// Empty the query.
    pub fn clear(&mut self) {
        self.text.clear();
        self.caret = 0;
    }

    /// Move the caret one character to the left.
    pub fn left(&mut self) -> bool {
        if self.caret == 0 {
            return false;
        }
        self.caret -= 1;
        true
    }

    /// Move the caret one character to the right.
    pub fn right(&mut self) -> bool {
        if self.caret >= self.text.chars().count() {
            return false;
        }
        self.caret += 1;
        true
    }

    /// Move the caret to the start of the text.
    pub fn home(&mut self) -> bool {
        if self.caret == 0 {
            return false;
        }
        self.caret = 0;
        true
    }

    /// Move the caret to the end of the text.
    pub fn end(&mut self) -> bool {
        let last = self.text.chars().count();
        if self.caret == last {
            return false;
        }
        self.caret = last;
        true
    }

    /// The byte offset of a character offset.
    ///
    /// The offset is clamped to the text: a caret that points past the end
    /// is a caret that was set before the text shrank, and a launcher that
    /// panics on that is a launcher that dies on a backspace.
    fn byte_offset(&self, caret: usize) -> usize {
        self.text
            .char_indices()
            .nth(caret)
            .map(|(i, _)| i)
            .unwrap_or(self.text.len())
    }

    /// The two parts of the query: the names and the content.
    pub fn split(&self) -> crate::layer::Split<'_> {
        crate::layer::Split::of(&self.text)
    }

    /// The two questions the query is: the engine's and the content
    /// layer's.
    ///
    /// The engine is asked the **name** part in the engine's own words,
    /// and the content layer is asked the **content** part as the user
    /// typed it. The split is the model's — one place to be wrong, and the
    /// window and the daemon's source cannot answer two different
    /// questions for the same words.
    pub fn halves(&self) -> crate::layer::Halves<'_> {
        crate::layer::Halves {
            split: self.split(),
        }
    }

    /// The short filters the text carries, in the order they appear.
    pub fn filters(&self) -> Vec<ShortFilter> {
        let mut out = Vec::new();
        for word in self.text.split_whitespace() {
            if let Some((filter, _)) = ShortFilter::of_prefix(word) {
                out.push(filter);
            }
        }
        out
    }

    /// The engine's version of the text.
    ///
    /// The short words become the engine's words; everything else is left
    /// exactly as the user typed it, because the engine's own vocabulary —
    /// `ext:`, `type:`, `re:`, the `|` alternatives, the markers — is the
    /// user's and the launcher has no business rewriting it.
    ///
    /// The short word is a **prefix**, and the name that follows it
    /// becomes a word of its own: the engine splits on spaces, and
    /// `type:docinvoice` is not a thing it knows.
    pub fn engine_text(&self) -> String {
        let mut out = String::with_capacity(self.text.len());
        for (i, word) in self.text.split_whitespace().enumerate() {
            if i > 0 {
                out.push(' ');
            }
            match ShortFilter::of_prefix(word) {
                Some((filter, rest)) => {
                    out.push_str(filter.engine_word());
                    if !rest.is_empty() {
                        out.push(' ');
                        out.push_str(rest);
                    }
                }
                None => out.push_str(word),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::string::ToString;
    use alloc::vec;

    /// A paste lands at the caret, and the caret walks with it.
    ///
    /// A paste is a hundred keystrokes that arrived at once: the text
    /// goes where the caret is, and the caret ends up after it.
    #[test]
    fn a_paste_lands_at_the_caret() {
        let mut q = Query::new();
        q.insert_str("invoice");
        assert_eq!(q.text(), "invoice");
        assert_eq!(q.caret(), 7, "the caret walked with the text");
        // The caret in the middle: what was there stays.
        q.left();
        q.left();
        q.insert_str("X");
        assert_eq!(q.text(), "invoiXce");
        assert_eq!(q.caret(), 6);
    }

    /// A paste of nothing is nothing: the query does not grow a caret
    /// that points at nothing.
    #[test]
    fn a_paste_of_nothing_changes_nothing() {
        let mut q = Query::new();
        q.insert_str("");
        assert_eq!(q.text(), "");
        assert_eq!(q.caret(), 0);
    }

    /// A paste of a hundred kilobytes lands whole, and the caret is at its
    /// end.
    ///
    /// The paste goes in as **one** piece: typing it a character at a time
    /// moves the whole text once per character, and a hundred thousand
    /// moves of a hundred kilobytes is a launcher that stops answering the
    /// keyboard for seconds. The time is not measured — a test that
    /// measures time is a test that fails on a busy machine — so what is
    /// checked is the result: the text is whole, and the caret is at its
    /// end.
    #[test]
    fn a_large_paste_lands_whole() {
        let mut q = Query::new();
        let paste = "x".repeat(100_000);
        q.insert_str(&paste);
        assert_eq!(q.text().len(), 100_000, "the paste landed whole");
        assert!(
            q.text().bytes().all(|b| b == b'x'),
            "the paste landed whole, and in order"
        );
        assert_eq!(q.caret(), 100_000, "the caret is at the end of it");
    }

    /// A paste of accented text lands whole, and the caret counts the
    /// characters and not the bytes.
    ///
    /// The cut is in bytes and the caret is in characters: a caret that
    /// counted the bytes would sit inside the accent, and the next
    /// keystroke would land in the middle of it.
    #[test]
    fn a_paste_of_accented_text_lands_whole() {
        let mut q = Query::new();
        q.insert_str("faturação");
        assert_eq!(q.text(), "faturação");
        assert_eq!(q.caret(), 9, "nine characters, not eleven bytes");
        // And in the middle: the text after the caret stays, and the paste
        // lands whole.
        q.left();
        q.left();
        q.insert_str("ç");
        assert_eq!(q.text(), "faturaçção");
        assert_eq!(q.caret(), 8, "the caret walked with the text");
    }

    #[test]
    fn an_empty_query_is_empty() {
        let q = Query::new();
        assert_eq!(q.text(), "");
        assert_eq!(q.caret(), 0);
        assert!(q.filters().is_empty());
    }

    #[test]
    fn the_short_words_become_the_engines_words() {
        let mut q = Query::new();
        q.set_text("pic: invoice".to_string());
        assert_eq!(q.engine_text(), "type:image invoice");
        assert_eq!(q.filters(), vec![ShortFilter::Pic]);
    }

    #[test]
    fn every_short_word_has_an_engine_word() {
        for f in ShortFilter::all() {
            let mut q = Query::new();
            q.set_text(format!("{} report", f.word()));
            assert_eq!(q.filters(), vec![*f]);
            assert!(
                q.engine_text().starts_with(f.engine_word()),
                "{:?} rewrites to {}",
                f,
                q.engine_text()
            );
        }
    }
    #[test]
    fn a_word_with_a_value_is_the_filter_and_the_name() {
        // `doc:invoice` is "the documents called invoice": the filter is
        // the head of the word and the name is the tail, and the engine
        // reads the two as separate words.
        let mut q = Query::new();
        q.set_text("doc:invoice".to_string());
        assert_eq!(q.filters(), vec![ShortFilter::Doc]);
        assert_eq!(q.engine_text(), "type:doc invoice");
    }

    #[test]
    fn a_word_that_only_looks_like_the_filter_is_not_it() {
        // `files:` is not `file:`, and the engine fails it closed — which
        // is the right answer for a word it does not know.
        let mut q = Query::new();
        q.set_text("files:invoice".to_string());
        assert!(q.filters().is_empty());
        assert_eq!(q.engine_text(), "files:invoice");
    }

    #[test]
    fn the_engines_own_vocabulary_is_left_alone() {
        let mut q = Query::new();
        q.set_text("ext:pdf type:doc re:^inv | invoice".to_string());
        assert_eq!(q.engine_text(), "ext:pdf type:doc re:^inv | invoice");
    }

    #[test]
    fn a_backspace_deletes_a_character_and_not_a_byte() {
        let mut q = Query::new();
        q.set_text("a\u{1f600}".to_string());
        assert!(q.backspace());
        assert_eq!(q.text(), "a");
        assert!(q.backspace());
        assert_eq!(q.text(), "");
        assert!(!q.backspace());
    }

    #[test]
    fn a_word_is_deleted_whole() {
        let mut q = Query::new();
        q.set_text("one two three".to_string());
        assert!(q.delete_word());
        assert_eq!(q.text(), "one two ");
        assert_eq!(q.caret(), 8, "the caret is where the word was");
        assert!(q.delete_word());
        assert_eq!(q.text(), "one ");
        assert_eq!(q.caret(), 4, "the caret is where the word was");
        assert!(q.delete_word());
        assert_eq!(q.text(), "");
        assert_eq!(q.caret(), 0);
        assert!(!q.delete_word());
    }

    #[test]
    fn the_caret_stays_where_the_deleted_word_was() {
        let mut q = Query::new();
        q.set_text("one two three".to_string());
        // The caret goes after "two ", in the middle of the text.
        for _ in 0..5 {
            q.left();
        }
        assert_eq!(q.caret(), 8);
        assert!(q.delete_word());
        assert_eq!(q.text(), "one three");
        assert_eq!(
            q.caret(),
            4,
            "the caret is at the start of the word that went"
        );
        // And the text after the caret did not move: the user is editing
        // in the middle of the line, and the line keeps going.
        assert_eq!(
            q.text()
                .chars()
                .skip(q.caret())
                .collect::<alloc::string::String>(),
            "three"
        );
    }

    #[test]
    fn the_launchers_folder_is_a_directory_and_not_the_engines_axis() {
        // The launcher owns its word: `folder:` is a type filter and
        // becomes `type:dir`. The engine's own `folder:` axis — the folder
        // the file is in — is a different question, and it does not come
        // through here.
        let mut q = Query::new();
        q.set_text("folder:2024".to_string());
        assert_eq!(q.engine_text(), "type:dir 2024");
        assert_eq!(q.filters(), vec![ShortFilter::Folder]);
        assert!(
            !q.engine_text().contains("folder:"),
            "the engine's axis is not reachable from the launcher"
        );
    }

    #[test]
    fn the_filters_are_in_the_order_they_were_typed() {
        let mut q = Query::new();
        q.set_text("doc: pic: video:".to_string());
        assert_eq!(
            q.filters(),
            vec![ShortFilter::Doc, ShortFilter::Pic, ShortFilter::Video]
        );
    }

    #[test]
    fn a_character_goes_in_at_the_caret() {
        let mut q = Query::new();
        q.set_text("ac".to_string());
        q.home();
        q.insert('b');
        assert_eq!(q.text(), "bac");
        assert_eq!(q.caret(), 1);
    }

    #[test]
    fn the_caret_moves_and_stops_at_the_ends() {
        let mut q = Query::new();
        q.set_text("abc".to_string());
        assert_eq!(q.caret(), 3);
        assert!(q.left());
        assert_eq!(q.caret(), 2);
        assert!(q.right());
        assert_eq!(q.caret(), 3);
        assert!(!q.right(), "the end is the end");
        assert!(q.home());
        assert_eq!(q.caret(), 0);
        assert!(!q.left(), "the start is the start");
        assert!(q.end());
        assert_eq!(q.caret(), 3);
    }

    #[test]
    fn a_backspace_deletes_before_the_caret() {
        let mut q = Query::new();
        q.set_text("abc".to_string());
        q.left();
        assert!(q.backspace());
        assert_eq!(q.text(), "ac");
        assert_eq!(q.caret(), 1);
    }

    #[test]
    fn a_delete_deletes_at_the_caret() {
        let mut q = Query::new();
        q.set_text("abc".to_string());
        q.home();
        q.right();
        assert!(q.delete_forward());
        assert_eq!(q.text(), "ac");
        assert_eq!(q.caret(), 1, "the caret stayed where it was");
    }

    #[test]
    fn a_delete_at_the_end_of_the_text_deletes_nothing() {
        let mut q = Query::new();
        q.set_text("abc".to_string());
        assert!(
            !q.delete_forward(),
            "the end of the text has nothing to delete"
        );
        assert_eq!(q.text(), "abc");
    }

    #[test]
    fn a_delete_removes_a_character_and_not_a_byte() {
        let mut q = Query::new();
        q.set_text("a\u{1f600}b".to_string());
        q.home();
        q.right();
        assert!(q.delete_forward());
        assert_eq!(q.text(), "ab");
        assert_eq!(q.caret(), 1);
    }

    /// The two questions the query is: the engine's and the content's.
    ///
    /// The engine is asked the **name** part in the engine's own words,
    /// and the content layer is asked the **content** part as the user
    /// typed it.
    #[test]
    fn the_two_halves_are_the_engines_words_and_the_content_part() {
        let mut q = Query::new();
        q.set_text("pic: invoice grep:total".to_string());
        let halves = q.halves();
        assert_eq!(halves.engine(), "type:image invoice");
        assert_eq!(halves.content(), Some("grep:total"));
        assert_eq!(halves.layer(), crate::layer::Layer::Content);
        assert!(halves.is_content());
        assert_eq!(halves.pattern(), "total");
    }

    /// A query without a prefix is the engine's alone.
    #[test]
    fn a_query_without_a_prefix_is_all_the_engines() {
        let mut q = Query::new();
        q.set_text("doc: invoice".to_string());
        let halves = q.halves();
        assert_eq!(halves.engine(), "type:doc invoice");
        assert_eq!(halves.content(), None);
        assert_eq!(halves.layer(), crate::layer::Layer::Names);
        assert!(!halves.is_content());
    }

    #[test]
    fn a_word_is_deleted_before_the_caret() {
        let mut q = Query::new();
        q.set_text("one two three".to_string());
        q.home();
        q.right();
        q.right();
        q.right();
        // The caret is after "one", so the word before it is "one".
        assert!(q.delete_word());
        assert_eq!(q.text(), " two three");
    }

    #[test]
    fn a_caret_past_the_end_does_not_panic() {
        // The text can shrink under a caret that was set before.
        let mut q = Query::new();
        q.set_text("abc".to_string());
        q.insert('d');
        assert_eq!(q.text(), "abcd");
        q.set_text("x".to_string());
        assert_eq!(q.caret(), 1);
        q.insert('y');
        assert_eq!(q.text(), "xy");
    }

    #[test]
    fn an_accented_character_is_one_caret_step() {
        let mut q = Query::new();
        q.set_text("aé".to_string());
        assert_eq!(q.caret(), 2, "two characters, not three bytes");
        assert!(q.left());
        assert_eq!(q.caret(), 1);
        assert!(q.right());
        assert_eq!(q.caret(), 2);
        assert!(q.backspace());
        assert_eq!(q.text(), "a", "the whole character went, not half of it");
        assert_eq!(q.caret(), 1);
    }
}
