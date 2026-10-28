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
//! invoice                 → names: "invoice",             content: —
//! grep:total              → names: —,                    content: "total"
//! invoice grep:total      → names: "invoice",             content: "total"
//! ```
//!
//! The part **before** the prefix is the name query, and it is the
//! **scope**: the files the second layer reads are the files the first
//! layer chose. The part **after** it is the pattern.
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

use alloc::string::String;

/// The words that put the content part of the query in the second layer.
///
/// These are the **engine's own** content words, and they are the same
/// ones: `grep:` is a literal pattern, `regex:` is a regular expression,
/// `sym:` is a symbol, and `content:` is the plain word for `grep:`. The
/// launcher does not have a pattern language of its own — the engine's is
/// the one the user already knows, and a second spelling would be a
/// second thing to learn.
pub const CONTENT_PREFIXES: &[&str] = &["grep:", "content:", "regex:", "sym:"];

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
    /// not have a pattern language of its own.
    pub content: Option<&'a str>,
}

impl<'a> Split<'a> {
    /// The split of a query.
    pub fn of(query: &'a str) -> Split<'a> {
        let trimmed = query.trim();
        // The prefix is read the way a person types it, and the lower
        // copy keeps the byte offsets: ASCII lowercasing does not move a
        // byte, and the accents are not letters of the prefix anyway.
        let lowered = trimmed.to_ascii_lowercase();
        for prefix in CONTENT_PREFIXES {
            let mut from = 0;
            while let Some(offset) = lowered[from..].find(prefix) {
                let at = from + offset;
                // The prefix is a **word**: what is before it is the end
                // of a word or the start of the query. `grepx:total` is a
                // word the engine does not know, not a content search.
                let before = at == 0 || trimmed.as_bytes()[at - 1].is_ascii_whitespace();
                if before && trimmed.is_char_boundary(at) {
                    let names = trimmed[..at].trim();
                    // The content part keeps its prefix: the engine reads
                    // it, and the launcher does not have a pattern language
                    // of its own.
                    let content = trimmed[at..].trim();
                    return Split {
                        names,
                        content: Some(content),
                    };
                }
                from = at + prefix.len();
            }
        }
        Split {
            names: trimmed,
            content: None,
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
    /// symbol — and the rest is the pattern itself.
    pub fn pattern(&self) -> &'a str {
        let content = self.content.unwrap_or("");
        for prefix in CONTENT_PREFIXES {
            if let Some(rest) = content.strip_prefix(prefix) {
                return rest.trim();
            }
            // The prefix is read the way a person types it.
            if content.len() >= prefix.len()
                && content.is_char_boundary(prefix.len())
                && content[..prefix.len()].eq_ignore_ascii_case(prefix)
            {
                return content[prefix.len()..].trim();
            }
        }
        content
    }
}

/// The name query that scopes a content search.
///
/// The scope is the first layer's answer, and the pattern that produced
/// it is the same pattern the content layer looks for: a user who writes
/// `grep:invoice` is asking for the files whose **content** says *invoice*,
/// and the scope — when there is one — is what the names before the prefix
/// chose.
pub fn scope_query(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    out.push_str(pattern);
    out
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

    #[test]
    fn the_scope_is_the_pattern_itself() {
        assert_eq!(scope_query("invoice"), "invoice".to_string());
    }
}
