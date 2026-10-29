// SPDX-License-Identifier: Apache-2.0

//! Which layer the question is about, and what each layer is asked.
//!
//! The launcher has two layers, and the difference between them is the
//! difference between *which files are called this* and *which files say
//! this*:
//!
//! * **names** — the first layer, and the default. The engine ranks the
//!   names in the index, and the answer costs a re-rank.
//! * **content** — the second layer. The files the first layer chose are
//!   read, and the question is what is inside them.
//!
//! # The split
//!
//! The content layer is a **prefix**, and the prefix **splits** the query
//! in two:
//!
//! ```text
//! invoice                 → names: "invoice",         content: —
//! grep:total              → names: "",                content: "grep:total"
//! invoice grep:total      → names: "invoice",         content: "grep:total"
//! ```
//!
//! The split is at the **leftmost** prefix, and not at the first prefix
//! the list of words names: `sym:foo grep:bar` opens with `sym:`, so the
//! whole of it is the content layer's question. A query that opened with
//! `sym:` and was handed to the engine as a name query would ask it for
//! symbols and get nothing back.
//!
//! The part **before** the prefix is the name query, and it is the
//! **scope**: the files the second layer reads are the files the first
//! layer chose. The part **from the prefix onwards** is the pattern.
//!
//! A prefix with nothing before it is the whole tree: "which files say
//! *total*", with no narrowing. That is the honest reading of the words,
//! and it is the one the daemon's own content engine answers fastest —
//! the pack store when there is one, the trigram index when there is not.
//!
//! A prefix with nothing after it is a question that has not been asked
//! yet: the caller keeps the results it has and waits.
//!
//! The prefix is read on a word boundary: `grepx:total` is not a prefix,
//! it is a word the engine does not know.
//!
//! # The vocabulary
//!
//! The content prefixes are the **engine's** words, and they are read in
//! one place: the table of [`CONTENT_PREFIXES`] says what each word asks
//! for, and the split, the mode and the pattern all come from it. The
//! other half of the launcher — the byte grep that answers when the
//! daemon cannot — asks the same table what it can answer, so a prefix
//! that only one half knows is a prefix the user gets two different
//! answers for.
//!
//! # The wire
//!
//! What goes to the daemon is the **engine's** spelling of the query: the
//! prefix is the table's own word, in lower case, and the pattern is
//! **one unit**.
//!
//! Both halves of that are the engine's rules and not the launcher's:
//! the filter names are exact, and their case is part of the name —
//! `Grep:` is not `grep:` — so a word the engine does not know fails
//! closed: `Grep:total` sent as typed is an error, not a search; and a
//! filter takes **one word** of the query — `grep:total due` sent as
//! typed is a filter whose value is `total`, and the daemon answers a
//! broader question than the user asked. A **quoted run** is one word, so
//! the pattern goes on the wire quoted.
//!
//! [`engine_content`] is that rewrite, and [`engine_pattern`] is what the
//! engine reads back out of it: the daemon takes the **whole** of the
//! rewrite — the prefix and the quoted pattern — and the byte grep takes
//! the pattern out of it, so the two answer the same question.

use alloc::string::String;

use crate::query::Query;

/// What a content prefix asks the engine for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentMode {
    /// `grep:` and `content:` — the bytes, literally.
    Literal,
    /// `regex:` — the engine's regular expression.
    Regex,
    /// `sym:` — a symbol the file defines.
    Symbol,
}

/// One content prefix: the word the user types and what it asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prefix {
    /// The word, with its colon.
    pub word: &'static str,
    /// What the word asks the engine for.
    pub mode: ContentMode,
}

/// The words that put the content part of the query in the second layer,
/// and what each one asks the engine for.
///
/// These are the **engine's own** content words, and they are the same
/// ones: `grep:` is a literal pattern, `regex:` is a regular expression,
/// `sym:` is a symbol, and `content:` is the plain word for `grep:`. The
/// launcher does not have a pattern language of its own — the engine's is
/// the one the user already knows, and a second spelling would be a
/// second thing to learn.
///
/// The table is the vocabulary, and it is the **only** one: the split
/// reads it, the mode comes from it, and the fallback asks it what it
/// can answer.
pub const CONTENT_PREFIXES: &[Prefix] = &[
    Prefix {
        word: "grep:",
        mode: ContentMode::Literal,
    },
    Prefix {
        word: "content:",
        mode: ContentMode::Literal,
    },
    Prefix {
        word: "regex:",
        mode: ContentMode::Regex,
    },
    Prefix {
        word: "sym:",
        mode: ContentMode::Symbol,
    },
];

/// Which layer a query is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    /// The names in the index.
    Names,
    /// The bytes inside the files the names chose.
    Content,
}

/// The two parts of a query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Split<'a> {
    /// The name query: what the engine ranks, and the scope of the
    /// content layer.
    pub names: &'a str,
    /// The content part, **with its prefix**, when the query carries one.
    ///
    /// `None` when it does not — the query is a name query and nothing
    /// else. `Some("grep:")` when the prefix is there and the pattern is
    /// not: the user is on their way to typing one.
    ///
    /// The prefix stays because the engine reads it: `grep:` is a literal
    /// pattern, `regex:` is a regular expression, and the launcher does
    /// not have a pattern language of its own. What each prefix means is
    /// [`content_mode`].
    pub content: Option<&'a str>,
}

impl<'a> Split<'a> {
    /// The split of a query.
    ///
    /// The content part starts at the **leftmost** content prefix: the
    /// query reads from the left to the right, and the prefix that opens
    /// it is the one that says what the content layer is being asked
    /// for. `sym:foo grep:bar` is one question about symbols, and the
    /// prefix on the left is `sym:` — splitting it at `grep:` would make
    /// the first half a name query, and a name query that starts with
    /// `sym:` asks the engine for symbols and gets nothing.
    pub fn of(query: &'a str) -> Split<'a> {
        let trimmed = query.trim();
        // The prefix is read the way a person types it, and the lower
        // copy keeps the byte offsets: ASCII lowercasing does not move a
        // byte, and the accents are not letters of the prefix anyway.
        let lowered = trimmed.to_ascii_lowercase();
        let mut split_at: Option<usize> = None;
        for prefix in CONTENT_PREFIXES {
            let word = prefix.word;
            let mut from = 0;
            while let Some(offset) = lowered[from..].find(word) {
                let at = from + offset;
                // The prefix is a **word**: what is before it is the end
                // of a word or the start of the query. `grepx:total` is a
                // word the engine does not know, not a content search.
                let before = at == 0 || trimmed.as_bytes()[at - 1].is_ascii_whitespace();
                if before && trimmed.is_char_boundary(at) {
                    // The leftmost prefix wins: the ones on the right of
                    // it are part of the pattern, not a second question.
                    split_at = Some(match split_at {
                        Some(current) if current <= at => current,
                        _ => at,
                    });
                    break;
                }
                from = at + word.len();
            }
        }
        match split_at {
            Some(at) => Split {
                names: trimmed[..at].trim(),
                // The content part keeps its prefix: the engine reads
                // it, and the launcher does not have a pattern language
                // of its own.
                content: Some(trimmed[at..].trim()),
            },
            None => Split {
                names: trimmed,
                content: None,
            },
        }
    }

    /// The layer the query is about.
    pub fn layer(&self) -> Layer {
        match self.content {
            None => Layer::Names,
            Some(_) => Layer::Content,
        }
    }

    /// Whether the query asks for the content layer.
    pub fn is_content(&self) -> bool {
        self.content.is_some()
    }

    /// The content pattern, without its prefix.
    ///
    /// The prefix says the **mode** — literal, regular expression,
    /// symbol — and the rest is the pattern itself. The mode comes from
    /// [`content_mode`], which is where the vocabulary lives; the pattern
    /// is what that call leaves.
    pub fn pattern(&self) -> &'a str {
        match self.content {
            Some(content) => match content_mode(content) {
                Some((_, pattern)) => pattern,
                // A content part with no prefix is not a content search,
                // and there is nothing to take off it.
                None => content,
            },
            None => "",
        }
    }
}

/// The two questions a query is.
///
/// The engine is asked the **name** part in the engine's own words, and
/// the content layer is asked the **content** part as the user typed it.
/// One query, two questions — and the split between them belongs to the
/// model, so the window and the daemon's source cannot answer two
/// different questions for the same words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Halves<'a> {
    /// The split of the query: the names and the content.
    pub split: Split<'a>,
}

impl<'a> Halves<'a> {
    /// The engine's question: the name part, in the engine's words.
    ///
    /// The launcher's short words become the engine's words — `pic:` is
    /// the launcher's and `type:image` is the engine's — and the content
    /// part never reaches the engine: it is the second layer's question.
    pub fn engine(&self) -> String {
        scope_query(self.split.names)
    }

    /// The content layer's question: the content part, as it was typed.
    ///
    /// The prefix stays: the engine reads it, and the launcher does not
    /// have a pattern language of its own.
    pub fn content(&self) -> Option<&'a str> {
        self.split.content
    }

    /// The layer the query is about.
    pub fn layer(&self) -> Layer {
        self.split.layer()
    }

    /// Whether the query asks for the content layer.
    pub fn is_content(&self) -> bool {
        self.split.is_content()
    }

    /// The content pattern, without its prefix.
    ///
    /// The prefix says the **mode** — literal, regular expression,
    /// symbol — and the rest is the pattern itself. Empty while the user
    /// is still typing it: the question has not been asked yet.
    pub fn pattern(&self) -> &'a str {
        self.split.pattern()
    }
}

/// The prefix of a content part, and the pattern that follows it.
///
/// The table is the vocabulary, and this is the one place it is read:
/// the split, the mode, the wire and the fallback all come from here, so
/// a prefix that only one half knows cannot happen.
///
/// The prefix is read the way a person types it — `Grep:` is `grep:` —
/// and it is a whole word: `grepx:total` is a word the engine does not
/// know, and the case of the **pattern** is the user's.
fn prefix_of(content: &str) -> Option<(Prefix, &str)> {
    let trimmed = content.trim();
    // The lower copy keeps the byte offsets: ASCII lowercasing does not
    // move a byte, and the accents are not letters of the prefix anyway.
    let lowered = trimmed.to_ascii_lowercase();
    for prefix in CONTENT_PREFIXES {
        if lowered.starts_with(prefix.word) {
            return Some((*prefix, trimmed[prefix.word.len()..].trim()));
        }
    }
    None
}

/// The mode and the pattern of a content part.
///
/// `None` when the part carries no prefix: it is not a content search,
/// and what a bare word means is the caller's business — the byte grep
/// reads one as a literal, and the engine reads one as a pattern.
pub fn content_mode(content: &str) -> Option<(ContentMode, &str)> {
    prefix_of(content).map(|(prefix, pattern)| (prefix.mode, pattern))
}

/// The content part, in the engine's words.
///
/// The prefix is the **canonical** one — the table's own word, in lower
/// case — and the pattern is **one unit**.
///
/// The prefix is canonical because the engine's filter names are exact,
/// their case is part of the name, and a word it does not know fails
/// closed: `Grep:total` sent as typed is an error, and the launcher would
/// blame the daemon for a typo it made itself.
///
/// The pattern is one unit because the engine gives a filter **one word**
/// of the query: `grep:total due` is a filter whose value is `total`,
/// and the daemon answers a broader question than the user asked. A
/// **quoted run** is one word, so the pattern goes on the wire quoted.
///
/// A part with no prefix is not a content search, and there is nothing to
/// rewrite: the engine reads it as it is.
pub fn engine_content(content: &str) -> String {
    let Some((prefix, pattern)) = prefix_of(content) else {
        return String::from(content);
    };
    let mut out = String::from(prefix.word);
    out.push_str(&quoted_pattern(pattern, prefix.mode));
    out
}

/// The pattern the engine reads out of a content part.
///
/// The engine gives a filter **one word** of the query, and a quoted run
/// is one word: the quotes are the mark that says the word is a phrase,
/// and they are not part of it. So the value of `grep:"total due"` is
/// `total due`, and the value of `grep:total due` is `total`.
///
/// The string this reads is the one [`engine_content`] built, and not the
/// text the user typed: the daemon takes the **whole** of it — the prefix
/// and the quoted pattern — and the byte grep takes the pattern out of
/// it. The two answer the same question because they read the same
/// string, and the string is the rewrite.
pub fn engine_pattern(content: &str) -> &str {
    match prefix_of(content) {
        Some((_, pattern)) => first_word(pattern),
        // A part with no prefix is not a content search: the whole of it
        // is the pattern.
        None => content.trim(),
    }
}

/// The pattern, as ONE unit on the wire.
///
/// The engine keeps a quoted run together, so a pattern with more than
/// one word is quoted: `grep:total due` goes as `grep:"total due"`.
///
/// The quoting stops where the pattern stops. The `|` is the engine's
/// alternative separator — except in a regular expression, where it
/// belongs to the pattern — and a quote that spans it would take the
/// separator's own words with it.
///
/// The user's own quotes are left alone: a second pair around them would
/// make the engine read the quotes as part of the pattern.
fn quoted_pattern(pattern: &str, mode: ContentMode) -> String {
    // The user's own quoting is the user's: a second pair of quotes
    // around it would make the engine read the quotes as part of the
    // pattern.
    if pattern.is_empty() || pattern.contains('"') {
        return String::from(pattern);
    }
    // A pattern with one word is one word already.
    if !pattern.contains(' ') {
        return String::from(pattern);
    }
    // The `|` is the engine's alternative separator — except in a regular
    // expression, where it belongs to the pattern. The quote stops where
    // the pattern stops, so the separator keeps its own meaning.
    let (head, tail) = match mode {
        ContentMode::Regex => (pattern, ""),
        _ => match pattern.split_once('|') {
            Some((before, _)) => {
                // The head ends where the word ends: the space before the
                // separator goes with the separator, and the engine reads
                // the two halves apart.
                let head = before.trim_end();
                let at = head.len();
                (head, &pattern[at..])
            }
            None => (pattern, ""),
        },
    };
    // The words are after the separator: they are not the pattern.
    if !head.contains(' ') {
        return String::from(pattern);
    }
    let mut out = String::with_capacity(pattern.len() + 2);
    out.push('"');
    out.push_str(head);
    out.push('"');
    out.push_str(tail);
    out
}

/// The first word of a text, the way the engine's splitter reads it.
///
/// A quoted run is one word — `total due` between quotes is one word —
/// and the quotes are not part of it.
fn first_word(text: &str) -> &str {
    let mut quoted = false;
    let mut end = text.len();
    for (at, ch) in text.char_indices() {
        match ch {
            '"' => quoted = !quoted,
            ' ' if !quoted => {
                end = at;
                break;
            }
            _ => {}
        }
    }
    let word = text[..end].trim();
    // The quotes are the mark that says the word is a phrase: they are
    // not part of it.
    let word = word.strip_prefix('"').unwrap_or(word);
    word.strip_suffix('"').unwrap_or(word)
}

/// The engine's query for the scope of a content search.
///
/// The argument is the **names** part of a content query — what stands in
/// front of the content prefix — and the answer is the query the engine is
/// asked for the scope. The launcher's short words become the engine's
/// words: `pic:` is the launcher's word, `type:image` is the engine's, and
/// the engine fails closed on a word it does not know — so a scope sent
/// as `pic:` comes back empty and the content layer answers "nothing
/// found" for a file that is on the disk.
///
/// The rewrite is the one [`Query::engine_text`] does, so the scope and
/// the query the user asked cannot drift apart.
pub fn scope_query(names: &str) -> String {
    let mut query = Query::new();
    query.set_text(String::from(names));
    query.engine_text()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn a_plain_query_is_all_names() {
        let split = Split::of("invoice");
        assert_eq!(split.layer(), Layer::Names);
        assert_eq!(split.names, "invoice");
        assert_eq!(split.content, None);
    }

    #[test]
    fn a_prefix_with_nothing_before_it_is_the_whole_tree() {
        let split = Split::of("grep:total");
        assert_eq!(split.layer(), Layer::Content);
        assert_eq!(split.names, "");
        assert_eq!(split.pattern(), "total");
        // The prefix stays: the engine reads it.
        assert_eq!(split.content, Some("grep:total"));
    }

    #[test]
    fn what_comes_before_the_prefix_is_the_scope() {
        let split = Split::of("invoice grep:total");
        assert_eq!(split.layer(), Layer::Content);
        assert_eq!(split.names, "invoice");
        assert_eq!(split.pattern(), "total");
        assert_eq!(split.content, Some("grep:total"));
    }

    #[test]
    fn the_content_prefixes_are_the_engines_own() {
        // `grep:` is a literal pattern, `regex:` a regular expression and
        // `sym:` a symbol: the launcher does not have a language of its
        // own, and the engine's is the one the user already knows.
        assert_eq!(Split::of("regex:^inv").pattern(), "^inv");
        assert_eq!(Split::of("sym:foo").pattern(), "foo");
        assert_eq!(Split::of("content:total").pattern(), "total");
        assert_eq!(Split::of("invoice regex:^inv").names, "invoice");
    }

    #[test]
    fn both_spellings_of_the_prefix_are_the_same_layer() {
        assert_eq!(Split::of("content:total").pattern(), "total");
        assert_eq!(Split::of("invoice content:total").names, "invoice");
    }

    #[test]
    fn the_prefix_is_read_the_way_a_person_types_it() {
        assert_eq!(Split::of("Grep:total").pattern(), "total");
        assert_eq!(Split::of("CONTENT:total").pattern(), "total");
    }

    #[test]
    fn a_prefix_with_nothing_after_it_is_a_question_not_asked_yet() {
        let split = Split::of("grep:");
        assert_eq!(split.layer(), Layer::Content);
        assert_eq!(split.pattern(), "");
        assert_eq!(split.names, "");
    }

    #[test]
    fn the_space_around_the_prefix_is_not_part_of_it() {
        let split = Split::of("  invoice   grep:  total  ");
        assert_eq!(split.names, "invoice");
        assert_eq!(split.pattern(), "total");
    }

    #[test]
    fn the_split_is_at_the_leftmost_prefix() {
        // `sym:` is a content filter too, and it is the one on the left:
        // the query is one question about symbols, and the prefix that
        // opens it is the one that says what is being asked for.
        let split = Split::of("sym:foo grep:bar");
        assert_eq!(split.layer(), Layer::Content);
        assert_eq!(split.names, "", "nothing is in front of the prefix");
        assert_eq!(split.content, Some("sym:foo grep:bar"));
        assert_eq!(split.pattern(), "foo grep:bar");
    }

    #[test]
    fn the_order_of_the_words_does_not_choose_the_prefix() {
        // The list of prefixes is a list of words, not a priority: the
        // prefix that opens the query is the one that is read.
        let split = Split::of("grep:bar sym:foo");
        assert_eq!(split.names, "");
        assert_eq!(split.content, Some("grep:bar sym:foo"));
        assert_eq!(split.pattern(), "bar sym:foo");
    }

    #[test]
    fn a_prefix_on_the_right_is_part_of_the_pattern() {
        let split = Split::of("invoice grep:total sym:foo");
        assert_eq!(split.names, "invoice");
        assert_eq!(split.content, Some("grep:total sym:foo"));
    }

    #[test]
    fn the_leftmost_prefix_is_read_the_way_a_person_types_it() {
        let split = Split::of("Sym:foo Grep:bar");
        assert_eq!(split.names, "");
        assert_eq!(split.content, Some("Sym:foo Grep:bar"));
        assert_eq!(split.pattern(), "foo Grep:bar");
    }

    #[test]
    fn a_word_that_merely_starts_like_the_prefix_is_not_the_prefix() {
        let split = Split::of("grepx:total");
        assert_eq!(split.layer(), Layer::Names);
        assert_eq!(split.names, "grepx:total");
        let split = Split::of("contents:total");
        assert_eq!(split.layer(), Layer::Names);
    }

    #[test]
    fn the_short_filters_stay_in_the_name_part() {
        // The filters are the engine's words, and the engine only sees
        // the name part.
        let split = Split::of("pic: grep:total");
        assert_eq!(split.names, "pic:");
        assert_eq!(split.pattern(), "total");
    }

    #[test]
    fn an_accented_first_character_does_not_panic() {
        let split = Split::of("ção");
        assert_eq!(split.layer(), Layer::Names);
        assert_eq!(split.names, "ção");
    }

    /// The two questions, in one answer.
    ///
    /// The engine's is the name part in the engine's words, and the
    /// content layer's is the content part as it was typed.
    #[test]
    fn the_two_halves_are_the_engine_and_the_content() {
        let halves = Halves {
            split: Split::of("pic: invoice grep:total"),
        };
        assert_eq!(halves.engine(), "type:image invoice");
        assert_eq!(halves.content(), Some("grep:total"));
        assert_eq!(halves.layer(), Layer::Content);
        assert!(halves.is_content());
        assert_eq!(halves.pattern(), "total");
    }

    #[test]
    fn the_scope_is_the_engines_query() {
        // The launcher's words become the engine's: `pic:` is the
        // launcher's word and the engine has never heard of it.
        assert_eq!(scope_query("pic:"), "type:image".to_string());
        assert_eq!(scope_query("doc: invoice"), "type:doc invoice".to_string());
        assert_eq!(scope_query("folder:2024"), "type:dir 2024".to_string());
        // A plain word is the user's and is left alone.
        assert_eq!(scope_query("invoice"), "invoice".to_string());
    }

    #[test]
    fn the_scope_of_a_content_query_is_a_query_the_engine_accepts() {
        // The whole point: `pic: grep:total` is a documented combination,
        // and the scope the daemon is asked for is one the engine knows.
        let split = Split::of("pic: grep:total");
        assert_eq!(split.names, "pic:");
        assert_eq!(scope_query(split.names), "type:image");
    }

    #[test]
    fn every_content_prefix_has_a_mode() {
        // The table is the vocabulary, and a word in it without a mode is
        // a word the two halves of the launcher would answer differently.
        for prefix in CONTENT_PREFIXES {
            assert!(
                content_mode(prefix.word).is_some(),
                "{} has no mode",
                prefix.word
            );
        }
    }

    #[test]
    fn the_content_prefixes_and_what_they_ask_for() {
        assert_eq!(
            content_mode("grep:total"),
            Some((ContentMode::Literal, "total"))
        );
        assert_eq!(
            content_mode("content:total"),
            Some((ContentMode::Literal, "total"))
        );
        assert_eq!(
            content_mode("regex:^inv"),
            Some((ContentMode::Regex, "^inv"))
        );
        assert_eq!(content_mode("sym:foo"), Some((ContentMode::Symbol, "foo")));
    }

    #[test]
    fn grep_and_content_are_the_same_mode() {
        // `content:` is the plain word for `grep:`.
        assert_eq!(
            content_mode("grep:total").map(|(mode, _)| mode),
            Some(ContentMode::Literal)
        );
        assert_eq!(
            content_mode("content:total").map(|(mode, _)| mode),
            content_mode("grep:total").map(|(mode, _)| mode)
        );
    }

    #[test]
    fn a_symbol_is_a_mode_of_its_own() {
        // The byte grep has no notion of a symbol: it reads bytes and it
        // finds text. The model still knows the word, so the fallback can
        // say that it cannot answer this one instead of grepping for the
        // word `sym:`.
        assert_eq!(
            content_mode("sym:total"),
            Some((ContentMode::Symbol, "total"))
        );
    }

    #[test]
    fn the_mode_is_read_the_way_a_person_types_it() {
        assert_eq!(
            content_mode("Grep:total"),
            Some((ContentMode::Literal, "total"))
        );
        assert_eq!(
            content_mode("CONTENT:total"),
            Some((ContentMode::Literal, "total"))
        );
        assert_eq!(
            content_mode("REGEX:^inv"),
            Some((ContentMode::Regex, "^inv"))
        );
        assert_eq!(content_mode("Sym:foo"), Some((ContentMode::Symbol, "foo")));
        // And the case of the pattern is the user's.
        assert_eq!(
            content_mode("grep:Total"),
            Some((ContentMode::Literal, "Total"))
        );
    }

    #[test]
    fn a_word_that_merely_starts_like_a_prefix_is_not_one() {
        // `grepx:total` is a word the engine does not know, and a part
        // with no prefix is not a content search.
        assert_eq!(content_mode("grepx:total"), None);
        assert_eq!(content_mode("contents:total"), None);
        assert_eq!(content_mode("invoice"), None);
        assert_eq!(content_mode(""), None);
    }

    #[test]
    fn the_pattern_agrees_with_the_mode() {
        // The split and the mode read the same table, so what the split
        // leaves is what the mode says is the pattern.
        for (query, names, pattern) in [
            ("grep:total", "", "total"),
            ("content:total", "", "total"),
            ("regex:^inv", "", "^inv"),
            ("sym:foo", "", "foo"),
            ("Grep:Total", "", "Total"),
            ("invoice grep:total", "invoice", "total"),
        ] {
            let split = Split::of(query);
            assert_eq!(split.names, names, "{query}");
            let content = split.content.expect("the query carries a prefix");
            let (_, from_the_mode) = content_mode(content).expect("the content carries a mode");
            assert_eq!(split.pattern(), pattern, "{query}");
            assert_eq!(split.pattern(), from_the_mode, "{query}");
        }
    }

    #[test]
    fn the_prefix_on_the_wire_is_the_engines_own() {
        // The engine's filter names are exact, their case is part of the
        // name, and a word it does not know fails closed: `Grep:total`
        // sent as typed is an error and not a search, and the launcher
        // would blame the daemon for a typo it made itself.
        assert_eq!(engine_content("Grep:total"), "grep:total");
        assert_eq!(engine_content("grep:total"), "grep:total");
        // The two spellings are the same query on the wire.
        assert_eq!(engine_content("Grep:total"), engine_content("grep:total"));
        // And the table's own word is the one that goes on it.
        assert_eq!(engine_content("CONTENT:total"), "content:total");
        assert_eq!(engine_content("REGEX:^inv"), "regex:^inv");
        assert_eq!(engine_content("Sym:foo"), "sym:foo");
        // A part with no prefix is not a content search, and there is
        // nothing to rewrite.
        assert_eq!(engine_content("invoice"), "invoice");
    }

    #[test]
    fn the_pattern_goes_on_the_wire_as_one_unit() {
        // The engine gives a filter one word of the query: `grep:total
        // due` is a filter whose value is `total`, and the daemon would
        // answer a broader question than the user asked. A quoted run is
        // one word.
        assert_eq!(engine_content("grep:total due"), "grep:\"total due\"");
        // The user's own quotes are not doubled: the engine would read
        // the second pair as part of the pattern.
        assert_eq!(engine_content("grep:\"total due\""), "grep:\"total due\"");
        assert_eq!(
            engine_content("grep:\"total due\" extra"),
            "grep:\"total due\" extra"
        );
        // A pattern with one word is one word already.
        assert_eq!(engine_content("grep:total"), "grep:total");
        // And the case of the pattern is the user's.
        assert_eq!(engine_content("grep:Total due"), "grep:\"Total due\"");
    }

    #[test]
    fn the_pattern_the_engine_reads_is_the_whole_phrase() {
        // The engine's value of a filter is one word, and a quoted run is
        // one word: the quotes are the mark that says the word is a
        // phrase, and they are not part of it.
        assert_eq!(engine_pattern("grep:\"total due\""), "total due");
        assert_eq!(engine_pattern("grep:total due"), "total");
        assert_eq!(engine_pattern("grep:total"), "total");
        // The quotes are the mark that says the word is a phrase: they
        // are not part of it.
        assert_eq!(engine_pattern("grep:\"Total due\""), "Total due");
    }

    #[test]
    fn the_daemon_and_the_byte_grep_read_the_same_rewritten_string() {
        // The string production hands the two is the **rewrite**, and not
        // the text the user typed: the daemon takes the whole of it — the
        // prefix and the quoted pattern — and the byte grep takes the
        // pattern out of it. Both look for `total due` and not for
        // `total`, because the rewrite quoted the phrase.
        let rewritten = engine_content("grep:total due");
        assert_eq!(rewritten, "grep:\"total due\"");
        assert_eq!(engine_pattern(&rewritten), "total due");
    }

    #[test]
    fn the_alternative_separator_keeps_its_own_meaning() {
        // The `|` is the engine's alternative separator, and the quote
        // stops where the pattern stops: a quote that spanned it would
        // take the separator's own words with it.
        assert_eq!(
            engine_content("grep:total due | doc:x"),
            "grep:\"total due\" | doc:x"
        );
        // In a regular expression the `|` belongs to the pattern.
        assert_eq!(engine_content("regex:a|b"), "regex:a|b");
        assert_eq!(engine_content("regex:a b|c"), "regex:\"a b|c\"");
    }
}
