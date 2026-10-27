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
//!
//! The same wrapper answers the prefilter's question: which literal must
//! every match start with, and which must every match end with. Those
//! answers come from the pattern's AST ([`prefix_literals`],
//! [`suffix_literals`]), not from a second regex engine: the pattern is
//! parsed once, and the tree is walked.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

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
        Self::compile(pattern, true)
    }

    /// Compile `pattern`, case-sensitively.
    ///
    /// The `re:` query filter is case-insensitive — paths are compared that
    /// way — but the user's priority rules are not: a rule for
    /// `Cargo.toml` is for `Cargo.toml`.
    pub fn new_sensitive(pattern: &str) -> Result<Self, String> {
        Self::compile(pattern, false)
    }

    /// Compile `pattern` with the case sensitivity the caller asks for.
    fn compile(pattern: &str, case_insensitive: bool) -> Result<Self, String> {
        let flags = syntax::Flags { case_insensitive, ..syntax::Flags::default() };
        let parsed = syntax::parse_with(pattern.as_bytes(), flags).map_err(|e| e.to_string())?;
        let prog = nfa::compile(&parsed.ast, parsed.groups);
        Ok(Regex { pattern: pattern.to_string(), prog: Some(prog) })
    }

    /// A pattern that never matches. Used to fail closed when a user's
    /// pattern does not compile: a positive `re:` filter then rejects
    /// everything rather than silently admitting everything.
    pub fn never() -> Self {
        Regex { pattern: String::new(), prog: None }
    }

    /// The pattern as typed.
    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    /// True when the pattern matches anywhere in `text`.
    ///
    /// Unanchored — `^` / `$` still work and mean the ends of `text`.
    /// The text is matched as raw bytes, which for UTF-8 paths is exactly
    /// the path's own encoding; case folding is ASCII-only.
    pub fn is_match(&self, text: &str) -> bool {
        self.is_match_bytes(text.as_bytes())
    }

    /// True when the pattern matches anywhere in `bytes`.
    ///
    /// The engine is byte-mode, and a name is bytes: this is the form the
    /// query layer actually calls.
    pub fn is_match_bytes(&self, bytes: &[u8]) -> bool {
        match &self.prog {
            Some(prog) => vm::matches_anywhere(prog, bytes),
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

/// How many members a class may have and still be a set of literals.
///
/// A class of two bytes is a fork: every match starts with one of them,
/// and two literals are still a prefilter. A class of sixty-four is a
/// prefilter that says nothing.
const MAX_FORK: usize = 8;

/// How many literals the extraction may produce before it gives up.
///
/// The forks multiply: eight classes of eight members are 16 million
/// literals, and a prefilter that lists sixteen million things is not a
/// prefilter. Past this many, the pattern has no usable literal.
const MAX_LITERALS: usize = 32;

/// Lowercased literals one of which starts every match of `pattern`.
///
/// This is the prefilter's question, asked of the pattern: a name the
/// pattern matches has to start with one of these, and the name index's
/// bitmaps can say so without reading the name.
///
/// `None` when the pattern has no such literal — nothing fixed at the
/// front, or too many possibilities to be worth listing.
pub fn prefix_literals(pattern: &str) -> Option<Vec<Vec<u8>>> {
    let ast = parse(pattern)?;
    let lits = prefixes(&ast);
    finish(lits)
}

/// Lowercased literals one of which ends every match of `pattern`, when the
/// pattern is anchored at the end ("swift$") and they hold no '/'.
///
/// Then a match's own name — the path's last part — ends with one of them.
pub fn suffix_literals(pattern: &str) -> Option<Vec<Vec<u8>>> {
    let ast = parse(pattern)?;
    if !ends_at_end(&ast) {
        return None;
    }
    let lits = suffixes(&ast);
    let lits = finish(lits)?;
    // A suffix with a '/' in it is not the end of a name: it is the end of
    // a path, and the name path cannot use it.
    if lits.iter().any(|l| l.contains(&b'/')) {
        return None;
    }
    Some(lits)
}

/// The pattern's AST, parsed the way the engine parses it.
fn parse(pattern: &str) -> Option<syntax::Ast> {
    syntax::parse_with(pattern.as_bytes(), syntax::Flags { case_insensitive: true, ..syntax::Flags::default() }).ok().map(|p| p.ast)
}

/// The extracted literals, sorted, deduplicated, and kept only if they are
/// all usable.
fn finish(mut lits: Vec<Vec<u8>>) -> Option<Vec<Vec<u8>>> {
    if lits.is_empty() {
        return None;
    }
    lits.sort();
    lits.dedup();
    if lits.iter().any(|l| l.is_empty()) {
        return None;
    }
    Some(lits)
}

/// The literals that start a match of `ast`, lowercased.
///
/// Empty when there is no fixed prefix: the caller treats that as "no
/// prefilter".
fn prefixes(ast: &syntax::Ast) -> Vec<Vec<u8>> {
    match ast {
        syntax::Ast::Class(set) => {
            let folded = fold_set(set);
            if folded.len() == 1 {
                vec![vec![folded[0]]]
            } else if folded.len() <= MAX_FORK {
                folded.iter().map(|b| vec![*b]).collect()
            } else {
                Vec::new()
            }
        }
        syntax::Ast::Group { node, .. } => prefixes(node),
        syntax::Ast::Concat(nodes) => {
            let mut out: Vec<Vec<u8>> = vec![Vec::new()];
            for node in nodes {
                if matches!(node, syntax::Ast::Empty) {
                    // A zero-width node -- an inline flag, an empty group --
                    // adds nothing to the prefix and does not end it
                    // either. `(?i)` parses to one of these, so a pattern
                    // that opens with the flag is not a pattern with no
                    // prefix.
                    continue;
                }
                let step = prefixes(node);
                if step.is_empty() {
                    // Nothing fixed here: the prefix ends before this node.
                    break;
                }
                out = join(&out, &step, false);
                out.sort();
                out.dedup();
                if out.len() > MAX_LITERALS {
                    return Vec::new();
                }
                if can_be_empty(node) {
                    // The node may match nothing, so what comes after it
                    // is not part of the prefix.
                    break;
                }
            }
            out
        }
        syntax::Ast::Alt(arms) => {
            let mut out = Vec::new();
            for arm in arms {
                let p = prefixes(arm);
                if p.is_empty() {
                    // One arm with no prefix: there is no literal that
                    // starts every match.
                    return Vec::new();
                }
                out.extend(p);
            }
            // The arms of a caseless literal repeat: `(?i)s` is an
            // alternation of `s`, `S` and the long s (U+017F), and the
            // two ASCII ones fold to the same literal. Without this the
            // sets multiply -- two per letter, and a five-letter word
            // would blow the limit for nothing.
            out.sort();
            out.dedup();
            if out.len() > MAX_LITERALS {
                return Vec::new();
            }
            out
        }
        syntax::Ast::Repeat { node, min, .. } => {
            if *min == 0 {
                // It may match nothing.
                Vec::new()
            } else {
                // A repetition starts with the thing it repeats.
                prefixes(node)
            }
        }
        syntax::Ast::Empty | syntax::Ast::Assert(_) => Vec::new(),
    }
}

/// The literals that end a match of `ast`, lowercased.
///
/// The walk is the prefix walk, backwards. The trailing end anchor is
/// zero-width, so it is skipped rather than counted.
fn suffixes(ast: &syntax::Ast) -> Vec<Vec<u8>> {
    match ast {
        syntax::Ast::Class(set) => {
            let folded = fold_set(set);
            if folded.len() == 1 {
                vec![vec![folded[0]]]
            } else if folded.len() <= MAX_FORK {
                folded.iter().map(|b| vec![*b]).collect()
            } else {
                Vec::new()
            }
        }
        syntax::Ast::Group { node, .. } => suffixes(node),
        syntax::Ast::Concat(nodes) => {
            let mut out: Vec<Vec<u8>> = vec![Vec::new()];
            let mut past_anchor = false;
            for node in nodes.iter().rev() {
                if matches!(node, syntax::Ast::Empty) {
                    // Zero-width: adds nothing to the suffix.
                    continue;
                }
                if !past_anchor {
                    if let syntax::Ast::Assert(a) = node
                        && matches!(a, syntax::Assert::EndLine | syntax::Assert::EndText)
                    {
                        // The end anchor: zero-width, and not part of the
                        // suffix. What came before it is.
                        continue;
                    }
                    past_anchor = true;
                }
                if matches!(node, syntax::Ast::Assert(_)) {
                    // Some other zero-width condition: the suffix ends.
                    break;
                }
                let step = suffixes(node);
                if step.is_empty() {
                    break;
                }
                out = join(&out, &step, true);
                out.sort();
                out.dedup();
                if out.len() > MAX_LITERALS {
                    return Vec::new();
                }
                if can_be_empty(node) {
                    break;
                }
            }
            out
        }
        syntax::Ast::Alt(arms) => {
            let mut out = Vec::new();
            for arm in arms {
                let p = suffixes(arm);
                if p.is_empty() {
                    return Vec::new();
                }
                out.extend(p);
            }
            // The same fold as in the prefix walk: the arms of a caseless
            // literal repeat.
            out.sort();
            out.dedup();
            if out.len() > MAX_LITERALS {
                return Vec::new();
            }
            out
        }
        syntax::Ast::Repeat { node, min, .. } => {
            if *min == 0 {
                Vec::new()
            } else {
                suffixes(node)
            }
        }
        syntax::Ast::Empty | syntax::Ast::Assert(_) => Vec::new(),
    }
}

/// The two literal sets, combined.
///
/// `prepend` puts `step` before `base`; otherwise it puts it after. The
/// two walks need opposite orders: the prefix walk goes forwards, so the
/// new node's literal goes after the one before it, and the suffix walk
/// goes backwards, so the new node's literal goes before it.
fn join(base: &[Vec<u8>], step: &[Vec<u8>], prepend: bool) -> Vec<Vec<u8>> {
    let mut out = Vec::with_capacity(base.len() * step.len());
    for b in base {
        for s in step {
            let mut bytes = Vec::with_capacity(b.len() + s.len());
            if prepend {
                bytes.extend_from_slice(s);
                bytes.extend_from_slice(b);
            } else {
                bytes.extend_from_slice(b);
                bytes.extend_from_slice(s);
            }
            out.push(bytes);
        }
    }
    out
}

/// Whether the pattern ends with an end anchor ("$" or "\z").
fn ends_at_end(ast: &syntax::Ast) -> bool {
    match ast {
        syntax::Ast::Concat(nodes) => match nodes.last() {
            Some(syntax::Ast::Assert(a)) => matches!(a, syntax::Assert::EndLine | syntax::Assert::EndText),
            _ => false,
        },
        syntax::Ast::Group { node, .. } => ends_at_end(node),
        syntax::Ast::Alt(arms) => !arms.is_empty() && arms.iter().all(ends_at_end),
        _ => false,
    }
}

/// Whether `ast` can match the empty string.
fn can_be_empty(ast: &syntax::Ast) -> bool {
    match ast {
        syntax::Ast::Empty | syntax::Ast::Assert(_) => true,
        syntax::Ast::Class(_) => false,
        syntax::Ast::Group { node, .. } => can_be_empty(node),
        syntax::Ast::Repeat { min, .. } => *min == 0,
        syntax::Ast::Concat(nodes) => nodes.iter().all(can_be_empty),
        syntax::Ast::Alt(arms) => arms.iter().any(can_be_empty),
    }
}

/// The class's members, folded to lower case and deduplicated.
///
/// The pattern is compiled case-insensitively, so a literal `f` is a class
/// of `f` and `F`. The comparison the prefilter runs against is the
/// ASCII-folded name, so the literal is the lower-case one.
fn fold_set(set: &rgx_core::byteclass::ByteSet) -> Vec<u8> {
    let mut out: Vec<u8> = set.iter().map(|b| b.to_ascii_lowercase()).collect();
    out.sort_unstable();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn a_literal_is_its_own_prefix() {
        assert_eq!(prefix_literals("(?i)foo"), Some(vec![b"foo".to_vec()]));
    }

    #[test]
    fn a_prefix_stops_at_the_wildcard() {
        assert_eq!(prefix_literals("(?i)foo.*bar"), Some(vec![b"foo".to_vec()]));
    }

    #[test]
    fn an_alternation_gives_one_literal_each() {
        let mut got = prefix_literals("(?i)(foo|bar)baz").unwrap();
        got.sort();
        assert_eq!(got, vec![b"barbaz".to_vec(), b"foobaz".to_vec()]);
    }

    #[test]
    fn no_prefix_is_none() {
        assert_eq!(prefix_literals("(?i).*"), None);
        assert_eq!(prefix_literals("(?i)[a-z]+"), None);
    }

    #[test]
    fn a_suffix_needs_the_anchor() {
        assert_eq!(suffix_literals("(?i)swift"), None);
        // The long s (U+017F) is a case variant of `s`, and the engine
        // folds it: `(?i)swift` matches "ſwift" as well as "swift". A
        // suffix set that only knew "swift" would throw the ſ name away,
        // so both are here -- and the check that uses them compares the
        // name's own bytes, so the ſ one still matches.
        assert_eq!(suffix_literals("(?i)swift$"), Some(vec![b"swift".to_vec(), "\u{017f}wift".as_bytes().to_vec()]));
    }

    /// The case variants of a letter that reach outside ASCII come with
    /// the letter: `k` also folds to the Kelvin sign (U+212A).
    #[test]
    fn a_caseless_letter_brings_its_far_case() {
        let lits = prefix_literals("(?i)kelvin").unwrap();
        assert!(lits.contains(&b"kelvin".to_vec()), "the plain spelling is there: {lits:?}");
        assert!(lits.iter().any(|l| l.starts_with(&[0xE2, 0x84, 0xAA])), "the Kelvin sign is there too: {lits:?}");
    }

    #[test]
    fn a_suffix_holds_no_slash() {
        assert_eq!(suffix_literals("(?i)a/b$"), None);
    }

    #[test]
    fn the_case_is_folded() {
        assert_eq!(prefix_literals("(?i)Foo"), Some(vec![b"foo".to_vec()]));
    }
}
