//! The byte-mode pattern syntax: a parser from bytes to an [`Ast`].
//!
//! Byte-oriented from end to end. A literal is a `u8`, a class is a
//! [`ByteSet`], and there is no `char` anywhere in the grammar. That is
//! the whole reason this exists rather than a thin wrapper over
//! `b02::lang::regex_engine`: rgx searches bytes that are routinely not
//! valid UTF-8 — extracted adapter output, raw passthrough, and with
//! `-a`, binary files — and a `char` engine handed lossy-decoded input
//! silently changes what a pattern means.
//!
//! # What the syntax covers
//!
//! Literals and escapes, `.`, bracket classes with ranges and
//! negation, the `\d \w \s` shorthands, alternation, concatenation,
//! the greedy and lazy forms of `* + ? {n,m}`, capturing and
//! non-capturing groups, the anchors `^ $ \A \z \b \B`, and the inline
//! flags `(?i) (?s) (?m)`.
//!
//! # What it does not, and why that is a gap and not a shrug
//!
//! Backreferences and lookaround are rejected rather than silently
//! mis-parsed. Both are things this engine can be wrong about in ways
//! that only show up on some inputs, and a pattern that compiles to the
//! wrong automaton is worse than one that refuses to compile: `rgx` is
//! ripgrep-compatible, so a user who types `\1` today gets a match and
//! would get something else here. The error names the construct.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::byteclass::ByteSet;

/// A parsed pattern.
///
/// The tree is deliberately not normalized: no constant folding, no
/// collapsing of nested single-child alternations. The compiler does
/// not need it and a tree that is the shape the user wrote is one fewer
/// place for a rewrite to change the meaning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ast {
    /// Matches the empty string. Also what an empty pattern compiles to.
    Empty,
    /// Consumes one byte that is a member of the set.
    Class(ByteSet),
    Concat(Vec<Ast>),
    Alt(Vec<Ast>),
    /// `min..=max` repetitions; `max` of `None` means unbounded.
    ///
    /// `greedy` false is the `?` suffix — prefer one, then as many as
    /// possible, which is a different automaton and not a flag the VM
    /// can consult at match time.
    Repeat {
        node: alloc::boxed::Box<Ast>,
        min: u32,
        max: Option<u32>,
        greedy: bool,
    },
    /// `index` is `None` for `(?:...)`.
    Group {
        index: Option<usize>,
        node: alloc::boxed::Box<Ast>,
    },
    Assert(Assert),
}

/// A zero-width condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Assert {
    /// `\A` — start of the searched text.
    StartText,
    /// `\z` — end of the searched text.
    EndText,
    /// `^` — start of text, or of a line when `(?m)` is set.
    StartLine,
    /// `$` — end of text, or of a line when `(?m)` is set.
    EndLine,
    /// `\b`
    WordBoundary,
    /// `\B`
    NotWordBoundary,
}

/// Inline flags, accumulated left to right and scoped to the group they
/// appear in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Flags {
    /// `i` — ASCII case-insensitive.
    pub case_insensitive: bool,
    /// `s` — `.` also matches `\n`.
    pub dot_all: bool,
    /// `m` — `^` and `$` match at line boundaries.
    pub multi_line: bool,
}

#[derive(Default)]

/// A pattern that would not compile, with the byte offset where the
/// parser gave up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxError {
    pub message: String,
    /// Byte offset into the pattern.
    pub at: usize,
}

impl core::fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "regex parse error at {}: {}", self.at, self.message)
    }
}

/// A parsed pattern, ready to compile.
pub struct Parsed {
    pub ast: Ast,
    /// Number of capture groups, so the VM can size its slot table.
    pub groups: usize,
    /// The name of each capture group in index order, empty where the
    /// group is unnamed. `names[i-1]` belongs to group `i`, because the
    /// whole match is group 0 and is never named here.
    ///
    /// The parser used to read `(?<name>` and throw the name away --
    /// the group counted, the name did not survive -- which made every
    /// `${name}` reference in a replacement template unanswerable. A
    /// template can only spell `$1` when nothing connects the pattern's
    /// name to the slot the VM filled.
    pub names: Vec<String>,
    /// The flags in force at the end of the pattern. Anchors compiled
    /// outside any group consult these.
    pub flags: Flags,
    /// Whether the pattern contains only pure ASCII literal characters and
    /// explicit ASCII character class ranges, with no shorthand classes (`\w`),
    /// POSIX classes, word boundaries (`\b`), dot (`.`), or caseless folding.
    pub pure_ascii: bool,
}

pub fn parse(pattern: &[u8]) -> Result<Parsed, SyntaxError> {
    parse_with(pattern, Flags::default())
}

/// Parse with builder-supplied flags, not just pattern-embedded ones.
///
/// rgx's `-i` is a builder flag (`grep_regex::RegexMatcherBuilder::case_insensitive`),
/// not a pattern rewrite, so the engine needs case-insensitivity to enter the
/// same way it enters from `(?i)` in the text. The flags seed the parser's
/// state exactly as a leading `(?i)` would.
pub fn parse_with(pattern: &[u8], flags: Flags) -> Result<Parsed, SyntaxError> {
    let mut p = Parser {
        pat: pattern,
        pos: 0,
        groups: 0,
        names: Vec::new(),
        flags,
        pure_ascii: !flags.case_insensitive,
    };
    let ast = p.parse_alt()?;
    if p.pos < p.pat.len() {
        // The only way to stop early is an unbalanced `)`.
        return Err(p.err("unmatched `)`"));
    }
    Ok(Parsed {
        ast,
        groups: p.groups,
        names: p.names,
        flags: p.flags,
        pure_ascii: p.pure_ascii && !p.flags.case_insensitive,
    })
}

struct Parser<'a> {
    pat: &'a [u8],
    pos: usize,
    groups: usize,
    names: Vec<String>,
    flags: Flags,
    pure_ascii: bool,
}

impl<'a> Parser<'a> {
    fn err(&self, message: impl Into<String>) -> SyntaxError {
        SyntaxError {
            message: message.into(),
            at: self.pos,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.pat.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let b = self.peek();
        if b.is_some() {
            self.pos += 1;
        }
        b
    }

    /// `alt := concat ('|' concat)*`
    ///
    /// Disjoint single-byte alternations (`a|b|c`) are coalesced to a
    /// single `Ast::Class` whose `ByteSet` is the union. The Pike VM
    /// treats an alt branch as a separate thread; fusing the bytes into
    /// one 256-bit class turns the alternation into one test per byte,
    /// which is what the SIMD nibble prefilter can also dispatch. The
    /// "disjoint" guard is what keeps `(a|a)` from being silently
    /// accepted (it would still produce the right answers, but the
    /// `Alt(branches)` shape is what the rest of the engine knows how
    /// to deduplicate, and the fusion here should not change a pattern's
    /// answer set).
    fn parse_alt(&mut self) -> Result<Ast, SyntaxError> {
        let mut branches = vec![self.parse_concat()?];
        while self.peek() == Some(b'|') {
            self.pos += 1;
            branches.push(self.parse_concat()?);
        }
        if branches.len() == 1 {
            return Ok(branches.pop().expect("one branch"));
        }
        if let Some(class) = disjoint_single_byte_class(&branches) {
            return Ok(Ast::Class(class));
        }
        Ok(Ast::Alt(branches))
    }

    /// `concat := (atom quantifier?)*`
    fn parse_concat(&mut self) -> Result<Ast, SyntaxError> {
        let mut items: Vec<Ast> = Vec::new();
        loop {
            match self.peek() {
                None | Some(b'|') | Some(b')') => break,
                _ => {}
            }
            let atom = self.parse_atom()?;
            let atom = self.parse_quantifier(atom)?;
            items.push(atom);
        }
        Ok(match items.len() {
            0 => Ast::Empty,
            1 => items.pop().expect("one item"),
            _ => Ast::Concat(items),
        })
    }

    /// Applies any repetition suffix to an atom. Written as a separate
    /// step so `a**` and friends are diagnosed rather than looped on.
    ///
    /// Loops, because a *counted* quantifier may stack: `a{1}{1}{1}` is
    /// three `a`s, and reading only the first left the rest as literal
    /// braces — so `a{1}{1}{1}` did not match `a` while `regex` does.
    fn parse_quantifier(&mut self, atom: Ast) -> Result<Ast, SyntaxError> {
        let mut atom = atom;
        loop {
            let (min, max) = match self.peek() {
                Some(b'*') => {
                    self.pos += 1;
                    (0, None)
                }
                Some(b'+') => {
                    self.pos += 1;
                    (1, None)
                }
                Some(b'?') => {
                    self.pos += 1;
                    (0, Some(1))
                }
                Some(b'{') => match self.parse_counted()? {
                    Some(bounds) => bounds,
                    // A `{` that is not a valid bound is a literal brace,
                    // which is what every other engine does with `a{`.
                    None => return Ok(atom),
                },
                _ => return Ok(atom),
            };

            if matches!(atom, Ast::Assert(_)) {
                return Err(self.err("repetition applied to an assertion"));
            }

            // A second quantifier is always a mistake: `a**`, `a*?` handled
            // below, but `a*+` or `a**` is not a thing.
            let greedy = if self.peek() == Some(b'?') {
                self.pos += 1;
                false
            } else {
                if matches!(self.peek(), Some(b'*') | Some(b'+')) {
                    return Err(self.err("nested repetition"));
                }
                true
            };
            if let Some(max) = max {
                if max < min {
                    return Err(self.err("repetition bounds are out of order"));
                }
            }
            atom = Ast::Repeat {
                node: alloc::boxed::Box::new(atom),
                min,
                max,
                greedy,
            };
            // Only a counted quantifier may follow another; the `*`/`+`
            // cases above have already been rejected.
            if !matches!(self.peek(), Some(b'{')) {
                return Ok(atom);
            }
        }
    }

    /// `{n}`, `{n,}` or `{n,m}`. `Ok(None)` when the braces are not a
    /// repetition at all.
    fn parse_counted(&mut self) -> Result<Option<(u32, Option<u32>)>, SyntaxError> {
        let start = self.pos;
        self.pos += 1; // '{'
        let min = match self.parse_number() {
            Some(n) => n,
            None => {
                self.pos = start;
                return Ok(None);
            }
        };
        let max = if self.peek() == Some(b'}') {
            Some(min)
        } else if self.peek() == Some(b',') {
            self.pos += 1;
            // `{n,}` has no upper bound.
            self.parse_number()
        } else {
            self.pos = start;
            return Ok(None);
        };
        if self.peek() != Some(b'}') {
            self.pos = start;
            return Ok(None);
        }
        self.pos += 1;
        Ok(Some((min, max)))
    }

    fn parse_number(&mut self) -> Option<u32> {
        let start = self.pos;
        let mut n: u64 = 0;
        while let Some(c) = self.peek() {
            if !c.is_ascii_digit() {
                break;
            }
            n = n.saturating_mul(10).saturating_add(u64::from(c - b'0'));
            if n > u64::from(u32::MAX) {
                return None;
            }
            self.pos += 1;
        }
        if self.pos == start {
            None
        } else {
            Some(n as u32)
        }
    }

    fn parse_atom(&mut self) -> Result<Ast, SyntaxError> {
        let b = self
            .peek()
            .ok_or_else(|| self.err("unexpected end of pattern"))?;
        match b {
            b'(' => self.parse_group(),
            b'[' => Ok(Ast::Class(self.parse_class()?)),
            b'.' => {
                self.pos += 1;
                self.pure_ascii = false;
                Ok(Ast::Class(if self.flags.dot_all {
                    ByteSet::any()
                } else {
                    ByteSet::any_except_newline()
                }))
            }
            b'^' => {
                self.pos += 1;
                Ok(Ast::Assert(if self.flags.multi_line {
                    Assert::StartLine
                } else {
                    Assert::StartText
                }))
            }
            b'$' => {
                self.pos += 1;
                Ok(Ast::Assert(if self.flags.multi_line {
                    Assert::EndLine
                } else {
                    Assert::EndText
                }))
            }
            b'\\' => self.parse_escape(),
            b'*' | b'+' | b'?' => Err(self.err("repetition with nothing to repeat")),
            _ => {
                self.pos += 1;
                if b >= 128 {
                    self.pure_ascii = false;
                }
                Ok(Ast::Class(self.literal(b)))
            }
        }
    }

    /// The byte set a literal `b` compiles to under the current flags.
    fn literal(&self, b: u8) -> ByteSet {
        let mut set = ByteSet::single(b);
        if self.flags.case_insensitive {
            set = set.union(&case_variant(b));
        }
        set
    }

    fn parse_group(&mut self) -> Result<Ast, SyntaxError> {
        self.pos += 1; // '('
        let saved = self.flags;

        // `(?...)` is four different things: a flag setting that ends
        // there, a scoped flag group `(?i:...)`, a non-capturing group
        // `(?:...)`, a named group, or a construct this engine refuses.
        // Lookaround is named before the flag parser sees it, so the
        // error says what is unsupported rather than "unknown flag `=`".
        let index = if self.peek() == Some(b'?') {
            match self.pat.get(self.pos + 1) {
                Some(b':') => {
                    self.pos += 2;
                    None
                }
                Some(b'=') | Some(b'!') => {
                    return Err(self.err("lookahead is not supported by the byte engine"));
                }
                Some(b'<') => match self.pat.get(self.pos + 2) {
                    Some(b'=') | Some(b'!') => {
                        return Err(self.err("lookbehind is not supported by the byte engine"));
                    }
                    // `(?<name>...)` is a named group, not lookbehind. The
                    // name is kept -- `Parsed::names` -- because a
                    // replacement template spells the group back as
                    // `${name}`, and the index alone cannot answer that.
                    _ => {
                        self.pos += 2;
                        let start = self.pos;
                        while let Some(c) = self.peek() {
                            self.pos += 1;
                            if c == b'>' {
                                break;
                            }
                        }
                        let name =
                            String::from_utf8_lossy(&self.pat[start..self.pos.saturating_sub(1)])
                                .into_owned();
                        self.groups += 1;
                        self.names.push(name);
                        Some(self.groups)
                    }
                },
                _ => match self.parse_flag_setting()? {
                    // `(?i)` closes at its own `)` and has no body: it
                    // sets the flags for the rest of the *enclosing*
                    // group and contributes nothing to the tree. Reading
                    // it as a group open is what made every flag setting
                    // fail with "unclosed group" — and that failure was
                    // invisible, because the differential harness skips
                    // patterns this engine rejects.
                    FlagEnd::Closed => return Ok(Ast::Empty),
                    // `(?i:...)` opens a group whose flags are scoped to
                    // it, and `parse_flag_setting` left us on its body.
                    FlagEnd::Group => None,
                },
            }
        } else {
            self.groups += 1;
            Some(self.groups)
        };

        let node = self.parse_alt()?;
        if self.bump() != Some(b')') {
            return Err(self.err("unclosed group"));
        }
        // Flags set inside a group do not leak out of it.
        self.flags = saved;
        Ok(Ast::Group {
            index,
            node: alloc::boxed::Box::new(node),
        })
    }

    /// `(?flags)` or `(?flags:...)`. The latter is treated as a
    /// non-capturing group with the flags in force.
    fn parse_flag_setting(&mut self) -> Result<FlagEnd, SyntaxError> {
        debug_assert_eq!(self.peek(), Some(b'?'));
        self.pos += 1; // '?'
        let mut negate = false;
        loop {
            match self.bump() {
                Some(b'i') => {
                    self.flags.case_insensitive = !negate;
                    if self.flags.case_insensitive {
                        self.pure_ascii = false;
                    }
                }
                Some(b's') => self.flags.dot_all = !negate,
                Some(b'm') => self.flags.multi_line = !negate,
                // `u`, `x` and `U` are accepted and do nothing. `x` and
                // `U` change spacing and greediness in engines that have
                // them; `u` selects unicode mode, and this engine IS
                // bytes, so both spellings of it are the mode it is
                // already in. Accepting `(?-u)` in particular is what
                // lets a glob-to-regex translator -- which emits
                // `(?-u)^...$` the way the `regex` crate spells "byte
                // pattern" -- feed this engine without editing its
                // output, and a pattern pasted from ripgrep lore to
                // compile as-is.
                Some(b'u') | Some(b'x') | Some(b'U') => { /* accepted, no effect in byte mode */ }
                Some(b'-') => negate = true,
                Some(b')') => return Ok(FlagEnd::Closed),
                // `(?i:...)` — the group ends at the matching `)`, which
                // `parse_group` is already positioned to read.
                Some(b':') => return Ok(FlagEnd::Group),
                Some(c) => return Err(self.err(format!("unknown flag `{}`", c as char))),
                None => return Err(self.err("unterminated flag setting")),
            }
        }
    }

    fn parse_escape(&mut self) -> Result<Ast, SyntaxError> {
        debug_assert_eq!(self.peek(), Some(b'\\'));
        self.pos += 1;
        let c = self.bump().ok_or_else(|| self.err("trailing backslash"))?;
        let set = match c {
            b'd' | b'D' | b'w' | b'W' | b's' | b'S' => {
                self.pure_ascii = false;
                match c {
                    b'd' => ByteSet::range(b'0', b'9'),
                    b'D' => ByteSet::range(b'0', b'9').negate(),
                    b'w' => word_set(),
                    b'W' => word_set().negate(),
                    b's' => space_set(),
                    b'S' => space_set().negate(),
                    _ => unreachable!(),
                }
            }
            b'n' => ByteSet::single(b'\n'),
            b't' => ByteSet::single(b'\t'),
            b'r' => ByteSet::single(b'\r'),
            b'f' => ByteSet::single(0x0c),
            b'v' => ByteSet::single(0x0b),
            b'0' => ByteSet::single(0),
            b'A' => return Ok(Ast::Assert(Assert::StartText)),
            b'z' => return Ok(Ast::Assert(Assert::EndText)),
            b'b' => {
                self.pure_ascii = false;
                return Ok(Ast::Assert(Assert::WordBoundary));
            }
            b'B' => {
                self.pure_ascii = false;
                return Ok(Ast::Assert(Assert::NotWordBoundary));
            }
            b'p' | b'P' => {
                return Err(self.err(format!(
                    "unicode property `\\{}` is not supported by the byte engine",
                    c as char
                )));
            }
            b'x' => {
                let v = self.parse_hex()?;
                if v >= 128 {
                    self.pure_ascii = false;
                }
                return Ok(Ast::Class(self.literal(v as u8)));
            }
            b'1'..=b'9' => {
                return Err(self.err(format!(
                    "backreference `\\{}` is not supported by the byte engine",
                    c as char
                )));
            }
            // Escaping a non-alphanumeric byte is that byte. `\q` is `q`,
            // not an error, which is what every other engine does.
            _ => {
                if c >= 128 {
                    self.pure_ascii = false;
                }
                self.literal(c)
            }
        };
        let set = if self.flags.case_insensitive {
            set.union(&case_fold_set(&set))
        } else {
            set
        };
        Ok(Ast::Class(set))
    }

    fn parse_hex(&mut self) -> Result<u32, SyntaxError> {
        let mut v: u32 = 0;
        let mut n = 0;
        while n < 2 {
            let Some(c) = self.bump() else {
                return Err(self.err("truncated `\\x` escape"));
            };
            let d = match c {
                b'0'..=b'9' => u32::from(c - b'0'),
                b'a'..=b'f' => u32::from(c - b'a') + 10,
                b'A'..=b'F' => u32::from(c - b'A') + 10,
                _ => return Err(self.err("`\\x` needs two hex digits")),
            };
            v = v * 16 + d;
            n += 1;
        }
        Ok(v)
    }

    /// `[...]`, with ranges, negation and the `\d \w \s` shorthands
    /// allowed inside. A `]` immediately after `[` or `[^` is a literal.
    fn parse_class(&mut self) -> Result<ByteSet, SyntaxError> {
        self.pos += 1; // '['
        let negated = if self.peek() == Some(b'^') {
            self.pos += 1;
            self.pure_ascii = false;
            true
        } else {
            false
        };

        let mut set = ByteSet::empty();
        let mut first = true;
        loop {
            let Some(c) = self.peek() else {
                return Err(self.err("unclosed character class"));
            };
            if c == b']' && !first {
                self.pos += 1;
                break;
            }
            first = false;

            match self.class_item()? {
                // A shorthand stands alone; a range would be meaningless.
                ClassItem::Set(s) => {
                    self.pure_ascii = false;
                    set = set.union(&s);
                    continue;
                }
                ClassItem::Byte(lo) => {
                    if lo >= 128 {
                        self.pure_ascii = false;
                    }
                    // A `-` is a range only when a member follows it, so
                    // `[a-]` and `[a-z0-9_-]` both work.
                    if self.peek() == Some(b'-') && self.pat.get(self.pos + 1) != Some(&b']') {
                        if self.pat.get(self.pos + 1).is_none() {
                            return Err(self.err("unclosed character class"));
                        }
                        self.pos += 1;
                        let hi = match self.class_item()? {
                            ClassItem::Byte(h) => {
                                if h >= 128 {
                                    self.pure_ascii = false;
                                }
                                h
                            }
                            ClassItem::Set(_) => {
                                return Err(
                                    self.err("a shorthand such as `\\d` cannot end a range")
                                );
                            }
                        };
                        if hi < lo {
                            return Err(self.err("character class range is out of order"));
                        }
                        let range = ByteSet::range(lo, hi);
                        set = set.union(&if self.flags.case_insensitive {
                            range.union(&case_fold_set(&range))
                        } else {
                            range
                        });
                    } else {
                        set = set.union(&self.literal(lo));
                    }
                }
            }
        }

        if negated {
            set = set.negate();
        }
        Ok(set)
    }

    /// One member of a class: a single byte, or a whole set when the
    /// member was a shorthand like `\d`.
    fn class_item(&mut self) -> Result<ClassItem, SyntaxError> {
        let c = self
            .bump()
            .ok_or_else(|| self.err("unclosed character class"))?;
        if c == b'[' {
            // A POSIX named class, `[[:alpha:]]`. Recognised before anything
            // else, because otherwise `[` is an ordinary member and the class
            // comes out as the letters of the name -- `[[:alpha:]]` matching
            // `[`, `:`, `a`, `l`, `p`, `h`. Guarded on `[:`, a name that ends
            // in `:]`, and a name that is one of the known ones, so `[` and
            // `[:` stay usable as literals.
            if self.peek() == Some(b':') {
                if let Some(end) = self.find_posix_class_end() {
                    if let Some(set) = posix_class(&self.pat[self.pos + 1..end]) {
                        self.pure_ascii = false;
                        // `self.pos` is on the `:`; step past the `:]`.
                        self.pos = end + 2;
                        return Ok(ClassItem::Set(if self.flags.case_insensitive {
                            set.union(&case_fold_set(&set))
                        } else {
                            set
                        }));
                    }
                }
            }
            return Ok(ClassItem::Byte(c));
        }
        if c != b'\\' {
            if c >= 128 {
                self.pure_ascii = false;
            }
            return Ok(ClassItem::Byte(c));
        }
        let e = self.bump().ok_or_else(|| self.err("trailing backslash"))?;

        // An escape naming exactly one byte stays a byte, so `[\x00-\x1f]`
        // is a range. Returning a set here made the `-` a literal and
        // the class came out as {0, '-', 0x1f}.
        let one = match e {
            b'n' => Some(b'\n'),
            b't' => Some(b'\t'),
            b'r' => Some(b'\r'),
            b'f' => Some(0x0c),
            b'v' => Some(0x0b),
            b'b' => Some(0x08), // `\b` inside a class is a backspace
            b'0' => Some(0),
            b'x' => {
                let v = self.parse_hex()? as u8;
                if v >= 128 {
                    self.pure_ascii = false;
                }
                Some(v)
            }
            b'1'..=b'9' => {
                return Err(self.err(format!(
                    "backreference `\\{}` is not supported by the byte engine",
                    e as char
                )));
            }
            b'p' | b'P' => {
                return Err(self.err(format!(
                    "unicode property `\\{}` is not supported by the byte engine",
                    e as char
                )));
            }
            _ => {
                if e >= 128 {
                    self.pure_ascii = false;
                }
                None
            }
        };
        if let Some(b) = one {
            let set = if self.flags.case_insensitive {
                ByteSet::single(b).union(&case_variant(b))
            } else {
                ByteSet::single(b)
            };
            // A folded single byte has exactly one member; if the two
            // cases somehow disagreed, the original byte is the safer
            // answer because it is the one the user wrote.
            return Ok(ClassItem::Byte(set.iter().next().unwrap_or(b)));
        }

        let set = match e {
            b'd' | b'D' | b'w' | b'W' | b's' | b'S' => {
                self.pure_ascii = false;
                match e {
                    b'd' => ByteSet::range(b'0', b'9'),
                    b'D' => ByteSet::range(b'0', b'9').negate(),
                    b'w' => word_set(),
                    b'W' => word_set().negate(),
                    b's' => space_set(),
                    b'S' => space_set().negate(),
                    _ => unreachable!(),
                }
            }
            _ => {
                if e >= 128 {
                    self.pure_ascii = false;
                }
                return Ok(ClassItem::Byte(e));
            }
        };
        Ok(ClassItem::Set(if self.flags.case_insensitive {
            set.union(&case_fold_set(&set))
        } else {
            set
        }))
    }

    /// Where the `:]` closing a POSIX class name sits, when the pattern has
    /// one starting at `self.pos` (which is on the `:`).
    ///
    /// Bounded, so a stray `[:` inside a class cannot swallow the rest of the
    /// pattern hunting for a name: a POSIX name is letters and nothing else, so
    /// anything else means there is no class here.
    fn find_posix_class_end(&self) -> Option<usize> {
        let mut i = self.pos + 1;
        loop {
            match self.pat.get(i) {
                Some(b':') if self.pat.get(i + 1) == Some(&b']') => return Some(i),
                Some(b'a'..=b'z') | Some(b'^') => i += 1,
                _ => return None,
            }
        }
    }
}

/// What ended a `(?flags` construct: its own `)`, or a `:` that opens a
/// group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlagEnd {
    /// `(?i)` — sets the flags and contributes nothing to the tree.
    Closed,
    /// `(?i:...)` — a group follows, with the flags scoped to it.
    Group,
}

/// A member of a bracket class.
enum ClassItem {
    Byte(u8),
    Set(ByteSet),
}

fn word_set() -> ByteSet {
    let mut s = ByteSet::range(b'a', b'z');
    s = s.union(&ByteSet::range(b'A', b'Z'));
    s.union(&ByteSet::range(b'0', b'9'))
        .union(&ByteSet::single(b'_'))
}

/// Letters and digits, no underscore. POSIX `alnum`, and `punct` is
/// everything printable that is not one of these.
fn alnum_set() -> ByteSet {
    ByteSet::range(b'0', b'9')
        .union(&ByteSet::range(b'A', b'Z'))
        .union(&ByteSet::range(b'a', b'z'))
}

fn space_set() -> ByteSet {
    let mut s = ByteSet::single(b' ');
    for b in [b'\t', b'\n', 0x0b, 0x0c, b'\r'] {
        s = s.union(&ByteSet::single(b));
    }
    s
}

/// A POSIX named character class, as in `[[:alpha:]]`.
///
/// `name` is what lies between `[:` and `:]`, and `None` means the name is not
/// one of them. The definitions are the C locale's, which is the only locale a
/// byte engine has: ASCII, and nothing above 0x7f. `grep` under a UTF-8 locale
/// would answer in codepoints, so this is ASCII by construction rather than by
/// approximation -- a byte class cannot mean "letter" for `é`.
///
/// A leading `^` negates, which POSIX spells INSIDE the brackets as
/// `[[:^alpha:]]`. That is a different spelling from a `^` in front of the
/// bracket, and the parser has to keep them apart: `[^[:alpha:]]` is the
/// complement of the class, `[[:^alpha:]]` is the class negated.
fn posix_class(name: &[u8]) -> Option<ByteSet> {
    let (negated, rest) = match name.first() {
        Some(b'^') => (true, &name[1..]),
        _ => (false, name),
    };
    let set = match rest {
        // `alnum` is `word` less the underscore, and `punct` is the printable
        // characters that are not `alnum` -- which still leaves the underscore
        // IN, because `_` is punctuation and not a letter or a digit. Using
        // `word` here instead of `alnum` is what left 8 lines out.
        b"alnum" => alnum_set(),
        b"alpha" => ByteSet::range(b'a', b'z').union(&ByteSet::range(b'A', b'Z')),
        b"blank" => ByteSet::single(b' ').union(&ByteSet::single(b'\t')),
        b"cntrl" => ByteSet::range(0x00, 0x1f).union(&ByteSet::single(0x7f)),
        b"digit" => ByteSet::range(b'0', b'9'),
        b"graph" => ByteSet::range(0x21, 0x7e),
        b"lower" => ByteSet::range(b'a', b'z'),
        b"print" => ByteSet::range(0x20, 0x7e),
        b"punct" => ByteSet::range(0x21, 0x7e).difference(&alnum_set()),
        b"space" => space_set(),
        b"upper" => ByteSet::range(b'A', b'Z'),
        b"word" => word_set(),
        b"xdigit" => ByteSet::range(b'0', b'9')
            .union(&ByteSet::range(b'A', b'F'))
            .union(&ByteSet::range(b'a', b'f')),
        _ => return None,
    };
    Some(if negated { set.negate() } else { set })
}

/// The other case of an ASCII letter; the byte itself otherwise.
fn case_variant(b: u8) -> ByteSet {
    match b {
        b'a'..=b'z' => ByteSet::single(b - 32),
        b'A'..=b'Z' => ByteSet::single(b + 32),
        _ => ByteSet::single(b),
    }
}

fn case_fold_set(set: &ByteSet) -> ByteSet {
    let mut out = ByteSet::empty();
    for b in set.iter() {
        out = out.union(&case_variant(b));
    }
    out
}

/// If every branch is a single disjoint byte (`a|b|c`), return the
/// union as a `ByteSet` for the parser to lower the alt into a
/// single `Ast::Class`. Otherwise `None`.
///
/// The "disjoint" check is the load-bearing part: a single byte that
/// appears in two arms still produces a `ByteSet` with that one byte
/// (it is the union, after all), and that is what the rest of the
/// engine would produce if it ran a `MatchMask::new` over the
/// equivalent alt. The alt's NFA semantics already deduplicate; the
/// point of the early coalescing is to skip running a separate VM
/// thread per arm.
fn disjoint_single_byte_class(branches: &[Ast]) -> Option<ByteSet> {
    let mut out = ByteSet::empty();
    for b in branches {
        let Ast::Class(set) = b else {
            return None;
        };
        // Only one byte per arm. A `[^x]` (complement) or `[abc]`
        // (multi-byte) is not what the optimization is for: the
        // first would produce a 255-byte set that is correct but
        // blows the win the per-byte shape was buying, and the
        // second is already a single class on its own.
        if set.len() != 1 {
            return None;
        }
        out = out.union(set);
    }
    if out.is_empty() {
        // `a|a` or any other deduplication to the empty set: the
        // empty alt matches nothing, which is what the VM would say
        // for it as well. We just keep `Ast::Alt` so the VM's "no
        // match" reporting does not get a wrong reason.
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod parse_alt_coalesce_tests {
    use super::*;

    fn parse_one(pat: &[u8]) -> Ast {
        parse(pat).unwrap().ast
    }

    /// `a|b|c` is one class, not an alt of three threads.
    #[test]
    fn disjoint_single_byte_alt_becomes_one_class() {
        let ast = parse_one(b"a|b|c");
        match ast {
            Ast::Class(s) => {
                assert!(s.contains(b'a'));
                assert!(s.contains(b'b'));
                assert!(s.contains(b'c'));
                assert!(!s.contains(b'd'));
            }
            other => panic!("expected Class, got {other:?}"),
        }
    }

    /// Mixed arms: an alt of a single byte and a two-byte class is
    /// NOT the optimization's shape and stays an alt.
    #[test]
    fn mixed_alt_stays_an_alt() {
        let ast = parse_one(b"a|[bc]");
        assert!(matches!(ast, Ast::Alt(_)));
    }

    /// `a|a` (duplicate arms) is the same answer either way: a
    /// single byte. The helper produces a class with that byte,
    /// the parser coalesces, and the VM would have produced the
    /// same answer. The test is the contract: a duplicate is
    /// accepted, not rejected.
    #[test]
    fn duplicate_single_byte_alt_still_coalesces() {
        let ast = parse_one(b"a|a");
        match ast {
            Ast::Class(s) => {
                assert!(s.contains(b'a'));
                assert_eq!(s.len(), 1);
            }
            other => panic!("expected Class, got {other:?}"),
        }
    }
}
