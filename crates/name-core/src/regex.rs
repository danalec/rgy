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
//! used to sit here is deleted: this calls into rgx-core directly.
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
    /// What a text that matches has to look like.
    shape: Shape,
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
        let shape = shape_of(&parsed.ast);
        let prog = nfa::compile(&parsed.ast, parsed.groups);
        Ok(Regex { pattern: pattern.to_string(), prog: Some(prog), shape })
    }

    /// A pattern that never matches. Used to fail closed when a user's
    /// pattern does not compile: a positive `re:` filter then rejects
    /// everything rather than silently admitting everything.
    pub fn never() -> Self {
        Regex { pattern: String::new(), prog: None, shape: Shape::default() }
    }

    /// What a text that matches this pattern has to look like.
    ///
    /// The conditions are necessary, not sufficient: the engine still
    /// decides. They are here so that a caller asking about many texts --
    /// the rules, which are asked about every name in the index -- can
    /// say "no" without running the engine.
    pub fn shape(&self) -> &Shape {
        &self.shape
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
        Regex { pattern: self.pattern.clone(), prog: self.prog.clone(), shape: self.shape.clone() }
    }
}

impl fmt::Debug for Regex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Regex({:?})", self.pattern)
    }
}

/// What a pattern says about the text it has to match.
///
/// The conditions are *necessary*: a text the pattern matches always
/// satisfies them, and the engine still has the last word. They come
/// from the pattern's AST -- the pattern is parsed once, and the tree is
/// walked -- so they cost nothing to compute and little to test.
///
/// They are what keeps a rule that matches nothing from costing a run of
/// the engine on every name. The engine is a Pike VM that restarts at
/// every byte of the text, and a rule set of ten patterns over two
/// hundred thousand names is two million VM runs to say "no".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Shape {
    /// The text is one of these, whole.
    pub whole: Option<Vec<Vec<u8>>>,
    /// The text starts with one of these.
    pub start: Option<Vec<Vec<u8>>>,
    /// The text ends with one of these.
    pub end: Option<Vec<Vec<u8>>>,
}

impl Shape {
    /// Could a text that looks like this match the pattern?
    ///
    /// The comparison folds the ASCII case on both sides: the literals
    /// are the pattern's own bytes, folded, and the text is folded the
    /// same way. Folding is a per-byte map, so a pattern that matches
    /// the text matches the folded text too -- and the pattern's own
    /// case insensitivity is already in the tree -- so this never
    /// rejects a match.
    pub fn maybe(&self, text: &[u8]) -> bool {
        if let Some(lits) = &self.whole {
            return lits.iter().any(|l| eq_folded(text, l));
        }
        if let Some(lits) = &self.start
            && !lits.iter().any(|l| starts_folded(text, l))
        {
            return false;
        }
        if let Some(lits) = &self.end
            && !lits.iter().any(|l| ends_folded(text, l))
        {
            return false;
        }
        true
    }
}

/// The shape of a pattern, from its tree.
///
/// The anchors are the text's own edges: `^` is the start of the text
/// and `$` is its end, and the engine checks them against the absolute
/// position in the text (`pos == 0`, `pos == len`). So a pattern that
/// starts with `^` can only match a text that starts with what follows
/// it, and one that ends with `$` only a text that ends with what comes
/// before it -- which is what the shape says.
fn shape_of(ast: &syntax::Ast) -> Shape {
    let syntax::Ast::Concat(nodes) = ast else {
        // A group is the shape of what it holds; anything else -- an
        // alternation at the top, a repetition -- has no shape this can
        // name.
        return match ast {
            syntax::Ast::Group { node, .. } => shape_of(node),
            _ => Shape::default(),
        };
    };
    // The anchors, and what is between them. The empty nodes are the
    // inline flags (`(?i)`), and they are nothing.
    let mut from = 0;
    while from < nodes.len() && matches!(nodes[from], syntax::Ast::Empty) {
        from += 1;
    }
    let mut to = nodes.len();
    while to > from && matches!(nodes[to - 1], syntax::Ast::Empty) {
        to -= 1;
    }
    let start = matches!(nodes.get(from), Some(syntax::Ast::Assert(a)) if matches!(a, syntax::Assert::StartText));
    let end = matches!(nodes.get(to.wrapping_sub(1)), Some(syntax::Ast::Assert(a)) if matches!(a, syntax::Assert::EndText));
    let body = &nodes[from + usize::from(start)..to - usize::from(end)];
    // The whole text: both anchors, and a body that is a fixed set of
    // literals.
    if start
        && end
        && let Some(lits) = body_literals(body)
    {
        return Shape { whole: Some(lits), start: None, end: None };
    }
    Shape {
        whole: None,
        // The start: the literals the body begins with.
        start: if start { prefix_of(body) } else { None },
        // The end: the literals the body ends with.
        end: if end { suffix_of(body) } else { None },
    }
}

/// The literals the body is, when the body is a fixed set of them.
///
/// Every node has to allow a fixed set of bytes: a run of them is the
/// product of what they allow, and an alternation is the union.
fn body_literals(body: &[syntax::Ast]) -> Option<Vec<Vec<u8>>> {
    let mut out: Vec<Vec<u8>> = vec![Vec::new()];
    for node in body {
        let step = alternatives(node)?;
        out = join(&out, &step, false);
        if out.len() > MAX_LITERALS {
            return None;
        }
    }
    finish(out)
}

/// The literals the body begins with, when it begins with a fixed set.
///
/// The walk stops at the first node that allows too much to name: what
/// is before it is the prefix, and what is after it is not.
fn prefix_of(body: &[syntax::Ast]) -> Option<Vec<Vec<u8>>> {
    let mut out: Vec<Vec<u8>> = vec![Vec::new()];
    for node in body {
        if matches!(node, syntax::Ast::Empty) {
            continue;
        }
        let Some(step) = alternatives(node) else { break };
        out = join(&out, &step, false);
        if out.len() > MAX_LITERALS {
            return None;
        }
        if can_be_empty(node) {
            break;
        }
    }
    finish(out)
}

/// The literals the body ends with, when it ends with a fixed set.
///
/// The prefix walk, backwards: what is after the first node that allows
/// too much is the suffix, and what is before it is not.
fn suffix_of(body: &[syntax::Ast]) -> Option<Vec<Vec<u8>>> {
    let mut out: Vec<Vec<u8>> = vec![Vec::new()];
    for node in body.iter().rev() {
        if matches!(node, syntax::Ast::Empty) {
            continue;
        }
        let Some(step) = alternatives(node) else { break };
        out = join(&out, &step, true);
        if out.len() > MAX_LITERALS {
            return None;
        }
        if can_be_empty(node) {
            break;
        }
    }
    finish(out)
}

/// The literals a node allows, when it allows a fixed set of them.
///
/// A class is its members; a run of nodes is the product of what they
/// allow; an alternation is the union of its arms. Anything else -- a
/// repetition, a zero-width condition -- allows too much to name.
fn alternatives(node: &syntax::Ast) -> Option<Vec<Vec<u8>>> {
    match node {
        syntax::Ast::Class(set) => {
            let folded = fold_set(set);
            if folded.is_empty() {
                return None;
            }
            Some(folded.into_iter().map(|b| vec![b]).collect())
        }
        syntax::Ast::Concat(nodes) => {
            let mut out: Vec<Vec<u8>> = vec![Vec::new()];
            for node in nodes {
                let step = alternatives(node)?;
                out = join(&out, &step, false);
                if out.len() > MAX_LITERALS {
                    return None;
                }
            }
            Some(out)
        }
        syntax::Ast::Alt(arms) => {
            let mut out = Vec::new();
            for arm in arms {
                out.extend(alternatives(arm)?);
            }
            // The arms of a caseless character are its case variants, and
            // folded they are the same bytes: `S` and `s` are one
            // literal, and only the ones ASCII does not fold -- the long
            // s, the Kelvin sign -- are their own.
            let out = fold_and_dedup(out);
            if out.len() > MAX_LITERALS {
                return None;
            }
            Some(out)
        }
        syntax::Ast::Group { node, .. } => alternatives(node),
        _ => None,
    }
}

/// The literals, folded and without repeats.
///
/// The fold is the ASCII one, and it is the same fold the comparison
/// does: a literal that folds to another is the same literal, and
/// keeping both would only make the set longer.
fn fold_and_dedup(lits: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = lits.into_iter().map(|l| l.into_iter().map(|b| b.to_ascii_lowercase()).collect()).collect();
    out.sort();
    out.dedup();
    out
}

/// The text and the literal, equal the way the engine compares them:
/// the ASCII case does not count.
fn eq_folded(text: &[u8], lit: &[u8]) -> bool {
    text.len() == lit.len() && text.iter().zip(lit).all(|(a, b)| a.to_ascii_lowercase() == *b)
}

/// The text starts with the literal.
fn starts_folded(text: &[u8], lit: &[u8]) -> bool {
    text.len() >= lit.len() && eq_folded(&text[..lit.len()], lit)
}

/// The text ends with the literal.
fn ends_folded(text: &[u8], lit: &[u8]) -> bool {
    text.len() >= lit.len() && eq_folded(&text[text.len() - lit.len()..], lit)
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

    /// The shape never rejects a match.
    ///
    /// The shape is a necessary condition, not a sufficient one: it may
    /// admit a text the engine rejects, but it must never reject a text
    /// the engine accepts. That is the whole of its contract, and this
    /// is where it is checked -- over the pattern shapes a user can
    /// write, and not only over the ones the defaults use.
    #[test]
    fn the_shape_never_rejects_a_match() {
        let patterns = [
            "^(a|b)$",
            "^\\.",
            "(?i)\\.app$",
            "^foo.*bar$",
            "(?i)foo",
            "foo$",
            "a|b",
            "[a-z]+",
            ".*",
            "^$",
            "node_modules",
            "(?i)(framework|bundle)$",
            "^\\w+",
            "x{2,3}",
            "(?s)a.b",
            "^(a|b)c$",
            "(?i)^\\w+$",
        ];
        let texts = [
            "",
            "a",
            "b",
            "ab",
            "c",
            "ac",
            "bc",
            "foo",
            "FOO",
            "foo.bar",
            "xfoobar",
            "foobar",
            ".git",
            ".hidden",
            "foo.app",
            "FOO.APP",
            "node_modules",
            "x.app",
            "framework",
            "FRAMEWORK",
            "x.framework",
            "xx",
            "xxx",
            "abcd",
            "a.b",
            "a\nb",
            "src",
            "notes.txt",
            "barfoo",
        ];
        for pattern in patterns {
            let re = Regex::new_sensitive(pattern).expect("the pattern compiles");
            for text in texts {
                if re.is_match_bytes(text.as_bytes()) {
                    assert!(re.shape().maybe(text.as_bytes()), "the shape of {pattern:?} rejected {text:?}, which it matches");
                }
            }
        }
    }

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
