// SPDX-License-Identifier: Apache-2.0

//! The pack-store builder's walker, written in the tree.
//!
//! **What it decides.** The builder walks a corpus directory and asks
//! one question about every entry: does this belong in the store? The
//! hidden files are left out unless the caller asks for them, the ignore
//! files of the tree are honoured, a glob can say what to keep, a depth
//! can cap the walk, and a symbolic link is followed only when the
//! caller says so.
//!
//! **The rules, in the order they decide.**
//!
//! 1. The caller's globs decide first: a glob that matches says keep or
//!    drop, and nothing else is consulted.
//! 2. The `.ignore` files decide next, the deepest file first.
//! 3. The `.gitignore` files, the deepest file first, and only inside
//!    a repository -- and only from the repository's own directory down:
//!    a `.gitignore` above the repository is not the repository's.
//! 4. The `.git/info/exclude` of the repository.
//! 5. The global ignore file -- the one git's own configuration names,
//!    read against the directory the caller is in and not against the
//!    tree, because a global file is a file of the machine.
//! 6. The hidden rule: a name that starts with a dot is left out.
//!
//! Within one file the last rule that matches wins, a rule that matches
//! a directory matches everything under it, and a rule that says `!`
//! says keep.
//!
//! **The oracle.** The `ignore` crate was the walker before this one,
//! and it is still the reference: the crate is a dev-dependency, and the
//! tests beside this file hold this walker to it, entry for entry, over a
//! corpus of trees -- and the globs and the ignore files to the crate's
//! own matchers, over a corpus of globs and a corpus of lines.

use std::collections::VecDeque;
use std::ffi::OsStr;
use std::fs::{self, FileType, Metadata};
use std::io;
use std::path::{Path, PathBuf};

use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::regex::ByteRegex;

/// The name of the file that says what to leave out, in a directory.
const IGNORE_FILE: &str = ".ignore";
/// The name of the file git reads, in a directory.
const GIT_IGNORE_FILE: &str = ".gitignore";
/// The directory a repository keeps its state in.
const GIT_DIR: &str = ".git";
/// The directory jujutsu keeps its state in. Its rules are git's rules, and
/// a tree that holds one is a tree the git rules apply to.
const JJ_DIR: &str = ".jj";

// ---------------------------------------------------------------------------
// The pattern: a glob, as the tree's own byte engine runs it
// ---------------------------------------------------------------------------

/// A piece of a glob.
///
/// The pieces are the ones the reference's glob layer has, because the
/// pieces are what decide what a `**` means: a `**` at the front of a glob
/// crosses any number of directories, a `**` at the back is everything under
/// a directory, and a `**` in the middle is the separator or any directory.
/// A glob that is not text is still a glob, so the pieces are bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    /// A literal byte.
    Literal(u8),
    /// `*`: any run of bytes that is not a separator.
    ZeroOrMore,
    /// `?`: one byte that is not a separator.
    Any,
    /// `**` at the front: the front itself, or any directory.
    RecursivePrefix,
    /// `/**` at the back: the separator and everything under it.
    RecursiveSuffix,
    /// `/**/` in the middle: the separator, or any directory.
    RecursiveZeroOrMore,
    /// A class of bytes: `[a-z]`.
    Class {
        /// The class says what it does not hold.
        negated: bool,
        /// The ranges of the class, the low byte first.
        ranges: Vec<(u8, u8)>,
    },
    /// The alternates of a `{a,b}` group.
    Alternates(Vec<Vec<Token>>),
}

/// The separator of a path, in every platform: a glob says `src/*.rs` and
/// the path says `src\main.rs`.
const SEPARATOR: u8 = b'/';

/// The digits of a hexadecimal byte.
const HEX: &[u8; 16] = b"0123456789abcdef";

/// Is the byte a separator?
fn is_separator(byte: u8) -> bool {
    byte == SEPARATOR
}

/// The parser of a glob.
///
/// The parser is the reference's one, over bytes rather than characters.
/// The rules it follows are the ones that decide a `**`:
///
/// * `**` at the front of the glob crosses any number of directories;
/// * `**` at the back of a component is everything under that directory;
/// * `**` in the middle of a component is the separator or any directory;
/// * `**` that is not at the boundary of a component is two ordinary stars.
///
/// The boundary is what the byte before the `**` says, and it is the byte
/// before it rather than the byte in front of it: a glob that says `a/**`
/// has a `**` at a boundary, and one that says `a**/b` does not.
struct Parser<'a> {
    /// The branches of the glob. The last one is the branch being parsed.
    branches: Vec<Vec<Token>>,
    /// Where each group of alternates starts.
    groups: Vec<usize>,
    /// The bytes that are not parsed yet.
    rest: &'a [u8],
    /// The byte before the current one.
    prev: Option<u8>,
    /// The current byte.
    cur: Option<u8>,
}

impl<'a> Parser<'a> {
    /// A parser of the bytes of a glob.
    fn new(bytes: &'a [u8]) -> Self {
        Parser {
            branches: vec![Vec::new()],
            groups: Vec::new(),
            rest: bytes,
            prev: None,
            cur: None,
        }
    }

    /// The branch being parsed.
    fn branch(&mut self) -> &mut Vec<Token> {
        self.branches
            .last_mut()
            .expect("the parser always has a branch")
    }

    /// The next byte, and the byte before it.
    fn bump(&mut self) -> Option<u8> {
        self.prev = self.cur;
        self.cur = self.rest.first().copied();
        self.rest = if self.rest.is_empty() {
            &[]
        } else {
            &self.rest[1..]
        };
        self.cur
    }

    /// The byte after the current one.
    fn peek(&self) -> Option<u8> {
        self.rest.first().copied()
    }

    /// A piece in the branch being parsed.
    fn push(&mut self, token: Token) {
        self.branch().push(token);
    }

    /// The last piece of the branch being parsed.
    fn pop(&mut self) -> Option<Token> {
        self.branch().pop()
    }

    /// The glob, parsed into its pieces.
    fn parse(mut self) -> Result<Vec<Token>, String> {
        while let Some(byte) = self.bump() {
            match byte {
                b'?' => self.push(Token::Any),
                b'*' => self.parse_star()?,
                b'[' => self.parse_class()?,
                b'{' => {
                    self.groups.push(self.branches.len());
                    self.branches.push(Vec::new());
                }
                b'}' => self.pop_group()?,
                b',' => {
                    if self.groups.is_empty() {
                        // A comma outside a group is an ordinary comma.
                        self.push(Token::Literal(b','));
                    } else {
                        self.branches.push(Vec::new());
                    }
                }
                b'\\' => {
                    let byte = self
                        .bump()
                        .ok_or_else(|| "the glob ends with a backslash".to_string())?;
                    self.push(Token::Literal(byte));
                }
                byte => self.push(Token::Literal(byte)),
            }
        }
        if !self.groups.is_empty() {
            return Err("the glob has a group of alternates that is not closed".to_string());
        }
        Ok(self.branches.pop().expect("the parser always has a branch"))
    }

    /// The group of alternates that ends here.
    fn pop_group(&mut self) -> Result<(), String> {
        let start = self
            .groups
            .pop()
            .ok_or_else(|| "the glob has a `}` that opens nothing".to_string())?;
        let alts: Vec<Vec<Token>> = self.branches.drain(start..).collect();
        self.push(Token::Alternates(alts));
        Ok(())
    }

    /// The `*` at the current byte, and the `*` that may follow it.
    fn parse_star(&mut self) -> Result<(), String> {
        let prev = self.prev;
        if self.peek() != Some(b'*') {
            // A lone star is any run of bytes that is not a separator.
            self.push(Token::ZeroOrMore);
            return Ok(());
        }
        // The second star.
        self.bump();
        if self.branch().is_empty() {
            // Nothing before it: the `**` is at the front of the glob.
            if self.peek().is_none_or(is_separator) {
                // A separator follows, so the `**` crosses directories and
                // the separator is part of it.
                self.push(Token::RecursivePrefix);
                self.bump();
            } else {
                // Something else follows, so the `**` is two ordinary stars.
                self.push(Token::ZeroOrMore);
                self.push(Token::ZeroOrMore);
            }
            return Ok(());
        }
        if !prev.is_some_and(is_separator) {
            // The `**` does not start a component, so it is two ordinary
            // stars -- unless it sits in a group of alternates, where the
            // reference reads it as a recursive one.
            if self.groups.is_empty() || (prev != Some(b',') && prev != Some(b'{')) {
                self.push(Token::ZeroOrMore);
                self.push(Token::ZeroOrMore);
                return Ok(());
            }
        }
        let is_suffix = match self.peek() {
            None => {
                self.bump();
                true
            }
            Some(b',') | Some(b'}') if !self.groups.is_empty() => true,
            Some(byte) if is_separator(byte) => {
                self.bump();
                false
            }
            _ => {
                self.push(Token::ZeroOrMore);
                self.push(Token::ZeroOrMore);
                return Ok(());
            }
        };
        // The separator that came before the `**` is part of it, so it goes
        // back: what is left is the `**` alone.
        match self.pop() {
            Some(Token::RecursivePrefix) => self.push(Token::RecursivePrefix),
            Some(Token::RecursiveSuffix) => self.push(Token::RecursiveSuffix),
            _ => {
                if is_suffix {
                    self.push(Token::RecursiveSuffix);
                } else {
                    self.push(Token::RecursiveZeroOrMore);
                }
            }
        }
        Ok(())
    }

    /// The class at the current byte.
    fn parse_class(&mut self) -> Result<(), String> {
        let mut ranges: Vec<(u8, u8)> = Vec::new();
        let negated = match self.peek() {
            Some(b'!') | Some(b'^') => {
                self.bump();
                true
            }
            _ => false,
        };
        let mut first = true;
        let mut in_range = false;
        loop {
            let byte = match self.bump() {
                Some(byte) => byte,
                None => return Err("the glob has a class that is not closed".to_string()),
            };
            match byte {
                // A `]` right after the bracket is a literal one.
                b']' if first => ranges.push((b']', b']')),
                b']' => break,
                // A `-` right after the bracket is a literal one too.
                b'-' if first => ranges.push((b'-', b'-')),
                b'-' if in_range => {
                    // The range ends at the dash itself.
                    let last = ranges.last_mut().expect("a range has a start");
                    if last.1 < last.0 {
                        return Err(
                            "the glob has a class with a range that goes backwards".to_string()
                        );
                    }
                    last.1 = b'-';
                    in_range = false;
                }
                b'-' => in_range = true,
                byte => {
                    if in_range {
                        let last = ranges.last_mut().expect("a range has a start");
                        if last.1 < last.0 {
                            return Err(
                                "the glob has a class with a range that goes backwards".to_string()
                            );
                        }
                        last.1 = byte;
                    } else {
                        ranges.push((byte, byte));
                    }
                    in_range = false;
                }
            }
            first = false;
        }
        if in_range {
            // The class ends with a dash, so the dash is a literal.
            ranges.push((b'-', b'-'));
        }
        self.push(Token::Class { negated, ranges });
        Ok(())
    }
}

/// The pieces of a glob, or the reason it is not a glob.
fn parse_glob(glob: &str) -> Result<Vec<Token>, String> {
    Parser::new(glob.as_bytes()).parse()
}

/// The pieces, as a pattern the tree's own byte engine runs.
fn render(tokens: &[Token], out: &mut Vec<u8>) {
    for token in tokens {
        match token {
            Token::Literal(byte) => push_literal(out, *byte),
            Token::ZeroOrMore => out.extend_from_slice(b"[^/]*"),
            Token::Any => out.extend_from_slice(b"[^/]"),
            Token::RecursivePrefix => out.extend_from_slice(b"(?:/?|.*/)"),
            Token::RecursiveSuffix => out.extend_from_slice(b"/.*"),
            Token::RecursiveZeroOrMore => out.extend_from_slice(b"(?:/|/.*/)"),
            Token::Class { negated, ranges } => {
                out.push(b'[');
                if *negated {
                    out.push(b'^');
                }
                for (low, high) in ranges {
                    push_class_literal(out, *low);
                    if low != high {
                        out.push(b'-');
                        push_class_literal(out, *high);
                    }
                }
                out.push(b']');
            }
            Token::Alternates(alts) => {
                out.extend_from_slice(b"(?:");
                for (index, alt) in alts.iter().enumerate() {
                    if index > 0 {
                        out.push(b'|');
                    }
                    render(alt, out);
                }
                out.push(b')');
            }
        }
    }
}

/// A glob, turned into a pattern the tree's own byte engine runs.
///
/// The translation is the one the reference implementation's glob layer
/// uses: `*` stops at a separator, `**` crosses one, `?` is one byte that
/// is not a separator, and `[...]` is a class. The result is anchored at
/// both ends, because a glob is a whole path and not a part of one.
///
/// `None` is a glob the reference refuses: a class that is not closed, a
/// range that goes backwards, a group of alternates that is not closed. A
/// refused glob is a line that is not a rule.
fn translate(glob: &str) -> Option<Vec<u8>> {
    let tokens = parse_glob(glob).ok()?;
    let mut out = Vec::with_capacity(glob.len() + 8);
    out.extend_from_slice(b"(?-u)^");
    if tokens.len() == 1 && tokens[0] == Token::RecursivePrefix {
        // The whole glob is a `**`, so it matches everything.
        out.extend_from_slice(b".*");
    } else {
        render(&tokens, &mut out);
    }
    out.push(b'$');
    Some(out)
}

/// A literal byte, escaped for the pattern.
///
/// The engine is a byte engine, so a byte above 127 is named rather than
/// carried: the pattern says `\xNN` where the reference says the same.
fn push_literal(out: &mut Vec<u8>, byte: u8) {
    if matches!(
        byte,
        b'\\'
            | b'.'
            | b'+'
            | b'*'
            | b'?'
            | b'('
            | b')'
            | b'|'
            | b'['
            | b']'
            | b'{'
            | b'}'
            | b'^'
            | b'$'
    ) {
        out.push(b'\\');
        out.push(byte);
        return;
    }
    if byte >= 128 {
        push_hex(out, byte);
        return;
    }
    out.push(byte);
}

/// A literal byte, escaped for a class.
///
/// A dash is a literal in a class only when it is escaped, and a `]` ends
/// the class only when it is not.
fn push_class_literal(out: &mut Vec<u8>, byte: u8) {
    if matches!(byte, b'\\' | b']' | b'^' | b'-') {
        out.push(b'\\');
    }
    if byte >= 128 {
        push_hex(out, byte);
        return;
    }
    out.push(byte);
}

/// A byte above 127, named.
fn push_hex(out: &mut Vec<u8>, byte: u8) {
    out.push(b'\\');
    out.push(b'x');
    out.push(HEX[(byte >> 4) as usize]);
    out.push(HEX[(byte & 0x0f) as usize]);
}

// ---------------------------------------------------------------------------
// The rules: the lines of an ignore file
// ---------------------------------------------------------------------------

/// One line of an ignore file, compiled.
#[derive(Clone)]
pub(crate) struct Rule {
    /// The pattern, as the tree's own byte engine runs it. The engine is
    /// not copied from one rule to the next: the rules of a file are
    /// shared by every directory the file applies to.
    pattern: Arc<ByteRegex>,
    /// The rule says what to keep, not what to leave out.
    whitelist: bool,
    /// The rule matches directories only.
    only_dir: bool,
}

impl Rule {
    /// Does the rule match this path?
    fn matches(&self, path: &[u8]) -> bool {
        match self.pattern.search(path, 0) {
            Some(found) => found.start == 0 && found.end == path.len(),
            None => false,
        }
    }
}

/// The rules of one ignore file.
#[derive(Clone, Default)]
pub(crate) struct Rules {
    /// The rules, in the order the file writes them.
    pub(crate) rules: Vec<Rule>,
}

impl Rules {
    /// The last rule that matches decides.
    pub(crate) fn matched(&self, path: &[u8], is_dir: bool) -> Option<bool> {
        let mut out = None;
        for rule in &self.rules {
            if rule.only_dir && !is_dir {
                continue;
            }
            if rule.matches(path) {
                out = Some(rule.whitelist);
            }
        }
        out
    }
}

/// The rules of one ignore file, and the directory they are relative to.
#[derive(Clone, Default)]
struct Level {
    /// The directory the rules are relative to.
    base: PathBuf,
    /// The rules.
    rules: Rules,
}

/// How the `!` at the front of a line is read.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sense {
    /// The sense of an ignore file: `!` says keep.
    Keep,
    /// The sense of the caller's globs: `!` says drop, because a glob says
    /// what to keep and a negated one says what to leave out.
    Drop,
}

/// One line of an ignore file, as a rule.
///
/// The rules are the ones the reference implementation applies:
///
/// * a line that starts with `#` is a comment;
/// * a line that is empty is nothing;
/// * a line that starts with `!` says keep, not drop -- unless the line is
///   one of the caller's globs, where the sense is the other way round;
/// * a line that starts with `/` is relative to the file's own directory;
/// * a line that ends with `/` matches directories only;
/// * a line with no `/` in it matches at any depth below the file's
///   directory.
///
/// `Ok(None)` is a line that is not a rule: a comment, or nothing. `Err` is
/// a line that is not a glob: the reference refuses it, and the other lines
/// of the file still count.
pub(crate) fn parse_line(line: &str, sense: Sense) -> Result<Option<Rule>, String> {
    let original = line;
    if line.starts_with('#') {
        return Ok(None);
    }
    let mut line = line;
    if !line.ends_with("\\ ") {
        line = line.trim_end();
    }
    if line.is_empty() {
        return Ok(None);
    }
    let mut negated = false;
    let mut absolute = false;
    if line.starts_with("\\!") || line.starts_with("\\#") {
        line = &line[1..];
        absolute = line.starts_with('/');
    } else {
        if line.starts_with('!') {
            negated = true;
            line = &line[1..];
        }
        if line.starts_with('/') {
            line = &line[1..];
            absolute = true;
        }
    }
    let mut only_dir = false;
    if line.as_bytes().last() == Some(&b'/') {
        only_dir = true;
        line = &line[..line.len() - 1];
        if line.as_bytes().last() == Some(&b'\\') {
            line = &line[..line.len() - 1];
        }
    }
    // The sense of the rule. In an ignore file a plain rule drops and a
    // `!` keeps; in the caller's globs it is the other way round, because
    // a glob says what to keep.
    let whitelist = match sense {
        Sense::Keep => negated,
        Sense::Drop => !negated,
    };
    let mut glob = line.to_string();
    if !absolute && !glob.contains('/') {
        // A pattern with no separator in it matches at any depth.
        if !glob.starts_with("**/") {
            glob = format!("**/{glob}");
        }
    }
    if glob.ends_with("/**") {
        // `/**` matches everything inside a directory, not the directory.
        glob.push_str("/*");
    }
    let pattern = translate(&glob).ok_or_else(|| format!("the glob `{original}` is not a glob"))?;
    let pattern = ByteRegex::new(&pattern)
        .map_err(|err| format!("the glob `{original}` does not compile: {err:?}"))?;
    Ok(Some(Rule {
        pattern: Arc::new(pattern),
        whitelist,
        only_dir,
    }))
}

/// The rules of an ignore file, read from its bytes.
///
/// A line the reference refuses is not a rule, and the other lines of the
/// file still count: a file with one broken line is a file with one rule
/// missing, not a file that says nothing.
pub(crate) fn parse_file(bytes: &[u8]) -> Rules {
    let mut rules = Rules::default();
    for line in bytes.split(|byte| *byte == b'\n') {
        let line = String::from_utf8_lossy(line);
        let line = line.strip_suffix('\r').unwrap_or(&line);
        if let Ok(Some(rule)) = parse_line(line, Sense::Keep) {
            rules.rules.push(rule);
        }
    }
    rules
}

// ---------------------------------------------------------------------------
// The caller's globs
// ---------------------------------------------------------------------------

/// The globs the caller gave, which decide before anything else.
#[derive(Clone, Default)]
pub struct Override {
    /// The rules, in the order the caller gave them.
    rules: Vec<Rule>,
    /// How many of the rules say keep. A caller who named what to keep
    /// leaves everything else out.
    whitelists: usize,
    /// The directory the globs are relative to.
    root: PathBuf,
}

impl Override {
    /// What the globs say about this path.
    ///
    /// A glob that does not negate says keep, and a glob that negates says
    /// drop: the reference inverts the matcher, because an override says
    /// what to keep.
    pub(crate) fn matched(&self, path: &Path, is_dir: bool) -> Option<bool> {
        let path = self.strip(path);
        let path = normalize(path);
        let mut out = None;
        for rule in &self.rules {
            if rule.only_dir && !is_dir {
                continue;
            }
            if rule.matches(&path) {
                out = Some(rule.whitelist);
            }
        }
        if out.is_none() && self.whitelists > 0 && !is_dir {
            // The caller named what to keep, and this is not it.
            return Some(false);
        }
        out
    }

    /// The path, relative to the directory the globs were built for.
    ///
    /// The reference strips the root of the matcher from the path before it
    /// matches, and strips nothing when the root is `.` -- which is what the
    /// builder uses, because the globs of a pack store are written against
    /// the corpus itself and not against a directory above it.
    fn strip<'a>(&self, path: &'a Path) -> &'a Path {
        if self.root == Path::new(".") {
            return path;
        }
        match path.strip_prefix(&self.root) {
            Ok(rest) => rest.strip_prefix("/").unwrap_or(rest),
            Err(_) => path,
        }
    }
}

/// The builder of the caller's globs.
pub struct OverrideBuilder {
    /// The rules, in the order the caller gave them.
    rules: Vec<Rule>,
    /// How many of the rules say keep.
    whitelists: usize,
    /// The directory the globs are relative to.
    root: PathBuf,
}

impl OverrideBuilder {
    /// A builder of globs, relative to `root`.
    pub fn new(root: &Path) -> Self {
        OverrideBuilder {
            rules: Vec::new(),
            whitelists: 0,
            root: root.to_path_buf(),
        }
    }

    /// Add a glob.
    ///
    /// A glob that does not start with `!` says what to keep, and one that
    /// does says what to leave out.
    pub fn add(&mut self, glob: &str) -> Result<&mut Self, String> {
        match parse_line(glob, Sense::Drop) {
            Ok(Some(rule)) => {
                if rule.whitelist {
                    self.whitelists += 1;
                }
                self.rules.push(rule);
            }
            Ok(None) => {
                // A glob that is a comment, or nothing, is not a rule.
            }
            Err(err) => return Err(err),
        }
        Ok(self)
    }

    /// The globs.
    pub fn build(&self) -> Result<Override, String> {
        Ok(Override {
            rules: self.rules.clone(),
            whitelists: self.whitelists,
            root: self.root.clone(),
        })
    }
}

// ---------------------------------------------------------------------------
// The chain: the rules that apply to a directory
// ---------------------------------------------------------------------------

/// Every ignore file that applies to a directory, grouped by what the
/// file is.
#[derive(Clone, Default)]
struct Chain {
    /// The `.ignore` files, the shallowest first.
    ignore: Vec<Level>,
    /// The `.gitignore` files, the shallowest first.
    gitignore: Vec<Level>,
    /// The `.git/info/exclude` of the repository, if there is one.
    exclude: Option<Level>,
    /// The global ignore file.
    global: Option<Level>,
    /// The caller's globs.
    overrides: Option<Override>,
    /// Whether a repository was found at or above this directory: git's
    /// own rules apply only inside one.
    has_git: bool,
}

impl Chain {
    /// What the rules say about this path.
    fn matched(&self, path: &Path, is_dir: bool) -> Option<bool> {
        // The caller's globs decide first, and nothing else is consulted.
        if let Some(overrides) = &self.overrides {
            if let Some(keep) = overrides.matched(path, is_dir) {
                return Some(keep);
            }
        }
        // The `.ignore` files, the deepest first.
        for level in self.ignore.iter().rev() {
            if let Some(keep) = level.rules.matched(&relative(&level.base, path), is_dir) {
                return Some(keep);
            }
        }
        if !self.has_git {
            // Outside a repository, git's rules do not apply.
            return None;
        }
        // The `.gitignore` files, the deepest first.
        for level in self.gitignore.iter().rev() {
            if let Some(keep) = level.rules.matched(&relative(&level.base, path), is_dir) {
                return Some(keep);
            }
        }
        // The repository's exclude file.
        if let Some(level) = &self.exclude {
            if let Some(keep) = level.rules.matched(&relative(&level.base, path), is_dir) {
                return Some(keep);
            }
        }
        // The global ignore file.
        if let Some(level) = &self.global {
            if let Some(keep) = level.rules.matched(&relative(&level.base, path), is_dir) {
                return Some(keep);
            }
        }
        None
    }
}

/// The path of `path` relative to `base`, in the spelling the patterns
/// use: `/` between the parts.
fn relative(base: &Path, path: &Path) -> Vec<u8> {
    let path = match path.strip_prefix(base) {
        Ok(rest) => rest,
        Err(_) => path,
    };
    normalize(path)
}

/// The path in the spelling the patterns use: `/` between the parts.
///
/// On Windows the separator of the platform is a backslash and a pattern
/// says `/`, so the backslash becomes one. On Unix the separator already
/// is `/` and a backslash is a letter of a name -- the reference keeps it,
/// and git keeps it too, so the tree keeps it as well: a name that says
/// `src\\main.rs` is one name and not two.
#[cfg(unix)]
fn normalize(path: &Path) -> Vec<u8> {
    path.as_os_str().as_encoded_bytes().to_vec()
}

/// The path in the spelling the patterns use: `/` between the parts.
///
/// On Windows the separator of the platform is a backslash and a pattern
/// says `/`, so the backslash becomes one. On Unix the separator already
/// is `/` and a backslash is a letter of a name -- the reference keeps it,
/// and git keeps it too, so the tree keeps it as well: a name that says
/// `src\\main.rs` is one name and not two.
#[cfg(not(unix))]
fn normalize(path: &Path) -> Vec<u8> {
    let bytes = path.as_os_str().as_encoded_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    for byte in bytes {
        if *byte == b'\\' {
            // The separator is `/` everywhere, on every platform: a
            // pattern that says `src/*.rs` has to match a path that says
            // `src\main.rs`.
            out.push(b'/');
        } else {
            out.push(*byte);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The walk
// ---------------------------------------------------------------------------

/// The predicate that says whether an entry is walked into.
type Filter = Arc<dyn Fn(&Entry) -> bool + Send + Sync>;

/// An entry the walk found.
pub struct Entry {
    /// The path of the entry.
    path: PathBuf,
    /// The depth of the entry: the root is zero.
    depth: usize,
    /// The type of the entry, when the walk knows it.
    file_type: Option<FileType>,
    /// The name of the entry.
    name: std::ffi::OsString,
}

impl Entry {
    /// The path of the entry.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The depth of the entry: the root is zero.
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// The type of the entry, when the walk knows it.
    pub fn file_type(&self) -> Option<FileType> {
        self.file_type
    }

    /// The name of the entry.
    pub fn file_name(&self) -> &OsStr {
        &self.name
    }

    /// The metadata of the entry.
    pub fn metadata(&self) -> io::Result<Metadata> {
        fs::metadata(&self.path)
    }
}

/// A directory the walk has still to read.
struct Pending {
    /// The path of the directory.
    path: PathBuf,
    /// The depth of the directory: the root is zero.
    depth: usize,
    /// The rules that apply inside it.
    chain: Chain,
}

/// The walk itself.
pub struct Walk {
    /// The directories still to read, the deepest last.
    stack: Vec<Pending>,
    /// The entries of the directory being read.
    entries: VecDeque<io::Result<Entry>>,
    /// The options of the walk.
    opts: Options,
}

/// The options of a walk.
#[derive(Clone, Default)]
struct Options {
    /// Follow symbolic links.
    follow_links: bool,
    /// Leave the hidden files out.
    hidden: bool,
    /// Read the `.gitignore` files.
    git_ignore: bool,
    /// Read the global ignore file.
    git_global: bool,
    /// Read the repository's exclude file.
    git_exclude: bool,
    /// Read the ignore files of the directories above the root.
    parents: bool,
    /// The deepest level the walk reaches.
    max_depth: Option<usize>,
    /// The predicate that says whether an entry is walked into.
    filter: Option<Filter>,
    /// The caller's globs.
    overrides: Option<Override>,
    /// The root of the walk.
    root: PathBuf,
}

impl Walk {
    /// The next entry of the walk.
    fn step(&mut self) -> Option<io::Result<Entry>> {
        loop {
            if let Some(entry) = self.entries.pop_front() {
                return Some(entry);
            }
            let pending = self.stack.pop()?;
            let read = fs::read_dir(&pending.path);
            let mut children = Vec::new();
            match read {
                Ok(entries) => {
                    for entry in entries {
                        match entry {
                            Ok(entry) => children.push(Ok(entry)),
                            Err(err) => children.push(Err(err)),
                        }
                    }
                }
                Err(err) => {
                    // A directory that cannot be read is an error the
                    // walk reports and moves on from.
                    self.entries.push_back(Err(err));
                    continue;
                }
            }
            for child in children {
                let child = match child {
                    Ok(child) => child,
                    Err(err) => {
                        self.entries.push_back(Err(err));
                        continue;
                    }
                };
                let path = child.path();
                let name = child.file_name();
                let file_type = child.file_type().ok();
                let is_dir = file_type.is_some_and(|kind| kind.is_dir());
                let depth = pending.depth + 1;
                if let Some(limit) = self.opts.max_depth {
                    if depth > limit {
                        continue;
                    }
                }
                let entry = Entry {
                    path: path.clone(),
                    depth,
                    file_type,
                    name,
                };
                // The rules decide.
                let keep = pending.chain.matched(&path, is_dir);
                let hidden = self.opts.hidden && is_hidden(&entry.name);
                let skip = match keep {
                    Some(true) => false,
                    Some(false) => true,
                    None => hidden,
                };
                if skip {
                    continue;
                }
                // The caller's own predicate decides.
                if let Some(filter) = &self.opts.filter {
                    if !filter(&entry) {
                        continue;
                    }
                }
                // A directory that is walked into carries its own ignore
                // files down with it.
                if is_dir {
                    let chain = add_level(&pending.chain, &path, &self.opts);
                    self.stack.push(Pending {
                        path: path.clone(),
                        depth,
                        chain,
                    });
                }
                self.entries.push_back(Ok(entry));
            }
        }
    }
}

impl Iterator for Walk {
    type Item = io::Result<Entry>;

    fn next(&mut self) -> Option<io::Result<Entry>> {
        self.step()
    }
}

/// Is the name a hidden one?
fn is_hidden(name: &OsStr) -> bool {
    match name.to_str() {
        Some(name) => name.starts_with('.') && name != "." && name != "..",
        None => false,
    }
}

/// The builder of a walk.
pub struct WalkBuilder {
    /// The options.
    opts: Options,
}

impl WalkBuilder {
    /// A walk of `root`.
    pub fn new(root: &Path) -> Self {
        WalkBuilder {
            opts: Options {
                root: root.to_path_buf(),
                ..Options::default()
            },
        }
    }

    /// Follow symbolic links.
    pub fn follow_links(&mut self, yes: bool) -> &mut Self {
        self.opts.follow_links = yes;
        self
    }

    /// Leave the hidden files out.
    pub fn hidden(&mut self, yes: bool) -> &mut Self {
        self.opts.hidden = yes;
        self
    }

    /// Read the `.gitignore` files.
    pub fn git_ignore(&mut self, yes: bool) -> &mut Self {
        self.opts.git_ignore = yes;
        self
    }

    /// Read the global ignore file.
    pub fn git_global(&mut self, yes: bool) -> &mut Self {
        self.opts.git_global = yes;
        self
    }

    /// Read the repository's exclude file.
    pub fn git_exclude(&mut self, yes: bool) -> &mut Self {
        self.opts.git_exclude = yes;
        self
    }

    /// Read the ignore files of the directories above the root.
    pub fn parents(&mut self, yes: bool) -> &mut Self {
        self.opts.parents = yes;
        self
    }

    /// The deepest level the walk reaches.
    pub fn max_depth(&mut self, depth: Option<usize>) -> &mut Self {
        self.opts.max_depth = depth;
        self
    }

    /// The predicate that says whether an entry is walked into.
    pub fn filter_entry(
        &mut self,
        filter: impl Fn(&Entry) -> bool + Send + Sync + 'static,
    ) -> &mut Self {
        self.opts.filter = Some(Arc::new(filter));
        self
    }

    /// The caller's globs.
    pub fn overrides(&mut self, overrides: Override) -> &mut Self {
        self.opts.overrides = Some(overrides);
        self
    }

    /// The walk.
    pub fn build(&self) -> Walk {
        let root = &self.opts.root;
        let mut chain = Chain {
            overrides: self.opts.overrides.clone(),
            ..Chain::default()
        };
        // The global ignore file.
        if self.opts.git_global {
            if let Some((base, bytes)) = global_ignore() {
                chain.global = Some(Level {
                    base,
                    rules: parse_file(&bytes),
                });
            }
        }
        // The ignore files of the directories above the root.
        if self.opts.parents {
            for parent in parents(root) {
                chain = add_level(&chain, &parent, &self.opts);
            }
        }
        let chain = add_level(&chain, root, &self.opts);
        let mut walk = Walk {
            stack: vec![Pending {
                path: root.to_path_buf(),
                depth: 0,
                chain,
            }],
            entries: VecDeque::new(),
            opts: self.opts.clone(),
        };
        // The root is the first entry, the way the crate's walker has
        // it: a walk of a file has to yield that file.
        walk.entries.push_back(Ok(Entry {
            path: root.to_path_buf(),
            depth: 0,
            file_type: fs::metadata(root).ok().map(|meta| meta.file_type()),
            name: root
                .file_name()
                .map(|name| name.to_os_string())
                .unwrap_or_default(),
        }));
        walk
    }
}

/// The rules of one directory, added to the chain.
fn add_level(chain: &Chain, dir: &Path, opts: &Options) -> Chain {
    {
        let mut chain = chain.clone();
        // The `.ignore` file.
        let ignore = dir.join(IGNORE_FILE);
        if let Ok(bytes) = fs::read(&ignore) {
            chain.ignore.push(Level {
                base: dir.to_path_buf(),
                rules: parse_file(&bytes),
            });
        }
        // The `.gitignore` file.
        let git_ignore = dir.join(GIT_IGNORE_FILE);
        if opts.git_ignore {
            if let Ok(bytes) = fs::read(&git_ignore) {
                chain.gitignore.push(Level {
                    base: dir.to_path_buf(),
                    rules: parse_file(&bytes),
                });
            }
        }
        // A repository: git's rules apply inside one, and only from the
        // repository's own directory down.
        //
        // The state of a repository is a directory or a file -- a worktree
        // and a submodule have a file -- and jujutsu keeps its state in a
        // directory of its own, which the git rules follow too.
        let git = dir.join(GIT_DIR);
        let jj = dir.join(JJ_DIR);
        if (opts.git_ignore || opts.git_exclude) && (git.exists() || jj.exists()) {
            chain.has_git = true;
            // The `.gitignore` files above the repository are not the
            // repository's: the rules of a repository are the ones of its
            // own directory and of the directories under it. A level is
            // kept when its own directory is at or below the repository's
            // -- `base.starts_with(dir)`, and not the other way round,
            // which would keep the files above the repository and drop the
            // ones under it.
            chain.gitignore.retain(|level| level.base.starts_with(dir));
            if opts.git_exclude {
                let exclude = git.join("info").join("exclude");
                if let Ok(bytes) = fs::read(&exclude) {
                    chain.exclude = Some(Level {
                        base: dir.to_path_buf(),
                        rules: parse_file(&bytes),
                    });
                }
            }
        }
        chain
    }
}

/// The directories above `path`, the shallowest first.
fn parents(path: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut current = path.parent();
    while let Some(parent) = current {
        if parent.as_os_str().is_empty() {
            break;
        }
        out.push(parent.to_path_buf());
        current = parent.parent();
    }
    out.reverse();
    out
}

/// The global ignore file, and the directory its rules are read against.
///
/// **The file.** Git names one in its own configuration, under
/// `core.excludesFile`, and falls back to one under the XDG directory. The
/// configuration files are read in git's own order: the one the
/// environment names, the one in the home directory, the one under the XDG
/// directory, and the one of the system. The first of them that names a
/// file is the one that counts.
///
/// **The directory.** The rules of the global file are read against the
/// directory the caller is in, and not against the tree being walked: a
/// global file is a file of the machine and not of the tree. When that
/// directory cannot be found, the file says nothing -- which is what the
/// reference does, because it cannot read the file either.
fn global_ignore() -> Option<(PathBuf, Vec<u8>)> {
    let cwd = std::env::current_dir().ok()?;
    let path = global_ignore_file()?;
    let bytes = fs::read(&path).ok()?;
    Some((cwd, bytes))
}

/// The file git's own configuration names, or the one it falls back to.
fn global_ignore_file() -> Option<PathBuf> {
    let mut configs: Vec<PathBuf> = Vec::new();
    if let Some(path) = env_path("GIT_CONFIG_GLOBAL") {
        configs.push(path);
    }
    if let Some(home) = home_dir() {
        configs.push(home.join(".gitconfig"));
    }
    if let Some(path) = env_path("XDG_CONFIG_HOME") {
        configs.push(path.join("git").join("config"));
    } else if let Some(home) = home_dir() {
        configs.push(home.join(".config").join("git").join("config"));
    }
    if let Some(path) = env_path("GIT_CONFIG_SYSTEM") {
        configs.push(path);
    } else {
        configs.push(PathBuf::from("/etc/gitconfig"));
    }
    for config in configs {
        if let Ok(bytes) = fs::read(&config) {
            if let Some(path) = excludes_file(&bytes) {
                return Some(path);
            }
        }
    }
    // The file git falls back to.
    if let Some(path) = env_path("XDG_CONFIG_HOME") {
        let path = path.join("git").join("ignore");
        if path.is_file() {
            return Some(path);
        }
    } else if let Some(home) = home_dir() {
        let path = home.join(".config").join("git").join("ignore");
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

/// The `core.excludesFile` setting of a git configuration file.
///
/// The setting is a line of its own: the name, an `=`, and a path that is
/// a single run of non-space bytes. The first line of the file that says
/// it is the one that counts, and a path that is not a single run -- a
/// path with a space in it -- is not a path the reference finds either.
fn excludes_file(bytes: &[u8]) -> Option<PathBuf> {
    for line in bytes.split(|byte| *byte == b'\n') {
        let line = String::from_utf8_lossy(line);
        let line = line.trim_start();
        let Some(rest) = strip_prefix_ci(line, "excludesfile") else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start();
        let rest = rest.strip_prefix('"').unwrap_or(rest);
        let rest = rest.trim_start();
        let value: String = rest.chars().take_while(|c| !c.is_whitespace()).collect();
        if value.is_empty() {
            continue;
        }
        // The path has to be the whole of what is left of the line.
        let tail = rest[value.len()..].trim_start();
        let tail = tail.strip_prefix('"').unwrap_or(tail);
        if !tail.trim().is_empty() {
            continue;
        }
        return Some(PathBuf::from(expand_tilde(&value)));
    }
    None
}

/// The prefix, when the text starts with it, whatever the case.
fn strip_prefix_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    if head.eq_ignore_ascii_case(prefix) {
        text.get(prefix.len()..)
    } else {
        None
    }
}

/// A `~` in a path is the home directory.
fn expand_tilde(path: &str) -> String {
    let Some(home) = home_dir() else {
        return path.to_string();
    };
    path.replace('~', &home.to_string_lossy())
}

/// The home directory of the user.
fn home_dir() -> Option<PathBuf> {
    env_path("HOME").or_else(|| env_path("USERPROFILE"))
}

/// An environment variable that names a path.
fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}
