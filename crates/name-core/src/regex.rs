// SPDX-License-Identifier: Apache-2.0

//! Path regexes for `re:` query terms.
//!
//! Wrapper over the rgx-core engine (`syntax` → `nfa` → `vm`): a byte-mode
//! Thompson NFA run by a Pike VM, with the parser supporting literals,
//! classes, alternation, groups, `* + ? {m,n}` in greedy and lazy forms, and
//! the anchors `^ $ \A \z \b \B`.
//!
//! The wrapper supplies exactly what the query layer needs: compile a
//! pattern (ASCII case-insensitive, because paths are compared that way),
//! ask "does this match anywhere in the path", and fail closed with
//! [`Regex::never`] when a pattern does not compile. The vendored copy that
//! used to sit here is deleted (§4.4): this calls into rgx-core directly.

use std::fmt;

use rgx_core::nfa;
use rgx_core::syntax;
use rgx_core::vm;

/// A compiled pattern. See the module docs for what the engine supports.
pub struct Regex {
    /// The pattern as typed, for error messages and equality.
    pattern: String,
    /// `None` is [`Regex::never`]: matches nothing, not even empty input.
    prog: Option<nfa::Program>,
}

impl Regex {
    /// Compile `pattern`, ASCII case-insensitively.
    pub fn new(pattern: &str) -> Result<Self, String> {
        let parsed = syntax::parse_with(pattern.as_bytes(), syntax::Flags { case_insensitive: true, ..syntax::Flags::default() })
            .map_err(|e| e.to_string())?;
        let prog = nfa::compile(&parsed.ast, parsed.groups);
        Ok(Regex { pattern: pattern.to_string(), prog: Some(prog) })
    }

    /// A pattern that never matches. Used to fail closed when a user's
    /// pattern does not compile: a positive `re:` filter then rejects
    /// everything rather than silently admitting everything.
    pub fn never() -> Self {
        Regex { pattern: String::new(), prog: None }
    }

    /// True when the pattern matches anywhere in `text`.
    ///
    /// Unanchored — `^` / `$` still work and mean the ends of `text`.
    /// The text is matched as raw bytes, which for UTF-8 paths is exactly
    /// the path's own encoding; case folding is ASCII-only.
    pub fn is_match(&self, text: &str) -> bool {
        match &self.prog {
            Some(prog) => vm::matches_anywhere(prog, text.as_bytes()),
            None => false,
        }
    }
}

impl PartialEq for Regex {
    fn eq(&self, other: &Self) -> bool {
        self.pattern == other.pattern
    }
}
impl Eq for Regex {}

impl Clone for Regex {
    fn clone(&self) -> Self {
        Regex { pattern: self.pattern.clone(), prog: self.prog.clone() }
    }
}

impl fmt::Debug for Regex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Regex({:?})", self.pattern)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pattern: &str, text: &str) -> bool {
        Regex::new(pattern).expect("pattern must compile").is_match(text)
    }

    #[test]
    fn literals_match_anywhere_case_insensitively() {
        assert!(m("main", "C:\\src\\main.rs"));
        assert!(m("MAIN", "C:\\src\\main.rs"));
        assert!(m("src", "C:\\src\\main.rs"));
        assert!(!m("test", "C:\\src\\main.rs"));
    }

    #[test]
    fn dot_star_plus_question() {
        assert!(m("ma.n", "C:\\src\\main.rs")); // `.` is any one byte
        assert!(!m("ma.n", "C:\\src\\man.rs"));
        assert!(m("main.*rs", "C:\\src\\main.rs"));
        assert!(m("main.*", "C:\\src\\main"));
        assert!(m("colou?r", "color"));
        assert!(m("colou?r", "colour"));
        assert!(m("a+", "caaat"));
        assert!(!m("a+", "ctt"));
    }

    #[test]
    fn lazy_quantifiers_prefer_shorter_matches() {
        // `.*?x` must stop at the first x, not the last: matching the whole
        // string then backtracking is the greedy reading and would fail.
        assert!(m("<.*?>", "<a><b>"));
        assert!(m("a+?", "aaa"));
    }

    #[test]
    fn anchors_pin_the_match_to_the_ends() {
        assert!(m("^C:", "C:\\src"));
        assert!(!m("^src", "C:\\src"));
        assert!(m("rs$", "C:\\src\\main.rs"));
        assert!(!m("rs$", "C:\\src\\main.rs.bak"));
        // Path separators must be written `\\` — `\s` is the whitespace class.
        assert!(m(r"^C:\\src\\main\.rs$", "C:\\src\\main.rs"));
        assert!(!m("^main", "C:\\main.rs"));
    }

    #[test]
    fn character_classes_and_escapes() {
        assert!(m("[0-9]+", "file123"));
        assert!(!m("[0-9]+", "file"));
        assert!(m("[^0-9]+", "file"));
        assert!(!m("[^0-9]+", "123"));
        assert!(m(r"\d{3}", "x123y"));
        assert!(m(r"\w+", "hello_world"));
        assert!(m(r"a\.b", "a.b"));
        assert!(!m(r"a\.b", "axb"));
        // A `]` first in the class is a literal.
        assert!(m("[]]", "]"));
    }

    #[test]
    fn alternation_and_groups() {
        assert!(m("cat|dog", "hotdog"));
        assert!(m("cat|dog", "tomcat"));
        assert!(!m("cat|dog", "bird"));
        assert!(m("(ab)+c", "ababc"));
        assert!(!m("(ab)+c", "abac"));
        assert!(m(r"^(main|test)\.rs$", "main.rs"));
    }

    #[test]
    fn empty_matching_loops_compile_and_terminate() {
        // A Pike VM has no backtracking blow-up: `(a?)*` is a legal pattern
        // here (unlike a naive backtracking engine) and must return.
        assert!(Regex::new("(a?)*").is_ok());
        assert!(Regex::new("(a*)+").is_ok());
        assert!(Regex::new("(a?)*").unwrap().is_match("aaa"));
    }

    #[test]
    fn broken_patterns_fail_to_compile() {
        assert!(Regex::new("(unclosed").is_err());
        assert!(Regex::new("[unclosed").is_err());
        assert!(Regex::new(")").is_err());
        assert!(Regex::new("a\\").is_err());
    }

    #[test]
    fn never_matches_nothing() {
        let never = Regex::never();
        assert!(!never.is_match(""));
        assert!(!never.is_match("anything"));
    }

    #[test]
    fn repeated_matching_stays_sound() {
        // The same compiled pattern is queried once per indexed entry per
        // keystroke in the UI; correctness must not depend on call count.
        let re = Regex::new("(a|b|c)*d").unwrap();
        for _ in 0..100 {
            assert!(re.is_match("abcd"));
            assert!(!re.is_match("abc"));
        }
    }
}
