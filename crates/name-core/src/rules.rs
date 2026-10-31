// SPDX-License-Identifier: Apache-2.0

//! The user's priority rules.
//!
//! The reference's priority is a rule system the user writes: a rule carries a
//! priority, a regex and two flags, and it is attached to a path (§4.6 of
//! the reverse engineering report). rgy's was a table of directory names
//! in the source, which cannot be changed by the user and does not survive
//! an upgrade.
//!
//! This is that rule system, and the table is its first rule set: the
//! default rules reproduce the table's behaviour, so a user who writes no
//! rules gets what rgy always did.
//!
//! A rule file is a list of lines:
//!
//! ```text
//! # <level> <regex> [flags]
//! #   level: ignored | low | normal | internal | high | highfile
//! #   flags: f (the files under the folder), d (the folder itself)
//! #          both by default
//! low  ^node_modules$  fd
//! high ^Documents$    fd
//! ```
//!
//! The regex matches a **name** -- a folder's or a file's. The flags say
//! who the rule is for: `d` is the folder whose name matched, `f` is the
//! files under it. A rule with neither flag applies to both.
//!
//! A line that does not parse is skipped and reported, not fatal: a file
//! with one broken line is a file with one rule missing, not a file that
//! says nothing. The rules that parsed are the rules, and the broken
//! lines are said to the user (see [`Rules::warnings`]).
//!
//! The level of a record is the level of the **closest** rule that matches
//! a name in its chain and applies to it: its own name first, then its
//! parent's, and so on. A record no rule reaches is `Normal`.
//!
//! `Ignored` is a level a rule can give, and a record in it ranks last --
//! but it does not yet take the record out of the live set. That is a
//! bigger change than a level (the parity row P.4): a record that is not
//! searchable is not a record the search can skip.

use crate::rank::Tier;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

/// One rule: a name pattern, the level it gives, and who it is for.
pub struct Rule {
    /// The name pattern.
    re: crate::regex::Regex,
    /// The level the rule gives.
    level: Tier,
    /// The rule is for folders (the one whose name matched).
    folders: bool,
    /// The rule is for files (the ones under the folder that matched).
    files: bool,
}

impl Rule {
    /// Does the rule match `name`?
    ///
    /// The shape first: it is a few byte comparisons, and it is what
    /// keeps the engine from running on every name in the index. The
    /// engine is a Pike VM that restarts at every byte of the name, and
    /// the rules are asked about every name there is.
    fn matches(&self, name: &[u8]) -> bool {
        if !self.re.shape().maybe(name) {
            return false;
        }
        self.re.is_match_bytes(name)
    }
}

/// The rules, in the order they were written.
#[derive(Default)]
pub struct Rules {
    rules: Vec<Rule>,
    /// The lines that did not parse, and why.
    ///
    /// A rule the user wrote and rgy silently dropped is a rule that does
    /// not work, so a line that does not parse is reported and the lines
    /// that do are the rules.
    warnings: Vec<String>,
}

impl Rules {
    /// The rules in a file's text.
    ///
    /// A line that does not parse is skipped and reported, not fatal: a
    /// file with one broken line is a file with one rule missing, not a
    /// file that says nothing. The rules that parsed are the rules, and
    /// the broken lines are said to the user (see [`Rules::warnings`]).
    pub fn parse(text: &str) -> Rules {
        let mut rules = Vec::new();
        let mut warnings = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            match parse_rule(line) {
                Ok(rule) => rules.push(rule),
                Err(e) => warnings.push(format!("line {}: {e}", n + 1)),
            }
        }
        Rules { rules, warnings }
    }

    /// The lines of the file that did not parse, and why.
    ///
    /// The caller says them to the user: a rule that does not work is the
    /// user's to fix, and only they can.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// The rules in a file, read from the disk.
    ///
    /// The `std` path: reading the disk is what this is for.
    #[cfg(feature = "std")]
    pub fn read(path: &std::path::Path) -> Result<Rules, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Rules::parse(&text))
    }

    /// The rules beside an index: `<index>.rules`.
    ///
    /// No file is not an error -- the defaults are the rules, and a daemon
    /// that never wrote a file still has them. A file that cannot be read
    /// is an error: the user wrote it, and rgy would otherwise ignore it
    /// without saying so. The defaults stand in for it, and the reason
    /// comes back with them.
    ///
    /// A file that reads but has a line that does not parse is not an
    /// error: the rules that parsed are the rules, and the broken line
    /// comes back as the reason to say.
    #[cfg(feature = "std")]
    pub fn beside(index_path: &std::path::Path) -> (Rules, Option<String>) {
        let path = index_path.with_extension("rules");
        match Rules::read(&path) {
            Ok(rules) => {
                let said = rules.warnings.join("; ");
                let why = (!said.is_empty()).then(|| format!("a rules file line does not parse: {said}; the parsed rules are in use"));
                (rules, why)
            }
            Err(why) => {
                let missing = !path.exists();
                // The message says what is in use, because the caller
                // cannot tell the two cases apart: a file that could not
                // be read leaves the defaults in place, and a file that
                // read with a broken line leaves the parsed rules.
                (Rules::defaults(), (!missing).then(|| format!("{why} -- the default rules are in use")))
            }
        }
    }

    /// The level a name gets, if a rule matches it.
    ///
    /// `for_files` says whether the name is a file's or a folder's.
    pub fn level_of(&self, name: &[u8], for_files: bool) -> Option<Tier> {
        self.rules.iter().rev().find(|r| r.matches(name) && (if for_files { r.files } else { r.folders })).map(|r| r.level)
    }

    /// Both levels (`(for_folders, for_files)`) a name gets, in one pass.
    pub fn levels_of(&self, name: &[u8]) -> (Option<Tier>, Option<Tier>) {
        let mut folder = None;
        let mut file = None;
        for r in self.rules.iter().rev() {
            let need_folder = folder.is_none() && r.folders;
            let need_file = file.is_none() && r.files;
            if (need_folder || need_file) && r.matches(name) {
                if need_folder {
                    folder = Some(r.level);
                }
                if need_file {
                    file = Some(r.level);
                }
                if folder.is_some() && file.is_some() {
                    break;
                }
            }
        }
        (folder, file)
    }

    /// The rules rgy ships with: the table of directory names that used to
    /// be in the source, as rules.
    ///
    /// The levels are the ones the ladder has, so a folder the table did not
    /// name is `Normal` -- which is what the table meant when it said "I do
    /// not know this one", and what it did not mean when it said "-35".
    ///
    /// The order matters: the last rule that matches a name wins, so the
    /// specific ones come after the general ones.
    pub fn defaults() -> Rules {
        Rules {
            rules: vec![
                rule(Tier::Low, r"^(node_modules|__pycache__|site-packages|Pods|venv|bower_components)$").folders(true).files(true),
                rule(Tier::Low, r"^(target|build|dist|out|vendor|deps|tmp|temp)$").folders(true).files(true),
                rule(Tier::Low, r"^(Caches|cache|caches|Logs|DerivedData|CoreSimulator)$").folders(true).files(true),
                rule(Tier::Low, r"^(folders|Containers|Group Containers)$").folders(true).files(true),
                rule(Tier::Low, r"^Application Support$").folders(true).files(true),
                rule(Tier::Low, r"^(Library|System|opt)$").folders(true).files(true),
                rule(Tier::Internal, r"^Applications$").folders(true).files(true),
                // The dot-dirs, and then the two more specific ones: a
                // `.app` is a dot-dir too, and the more specific rule comes
                // last so it is the one that wins.
                rule(Tier::Low, r"^\.").folders(true).files(true),
                rule(Tier::Low, r"(?i)\.(framework|bundle|plugin|appex|kext|xpc|lproj|xcassets|photoslibrary|musiclibrary|tvlibrary|imovielibrary|dSYM|xcarchive|sdk|platform)$").folders(true).files(true),
                rule(Tier::Low, r"(?i)\.app$").folders(true).files(true),
            ],
            warnings: Vec::new(),
        }
    }
}

/// A rule from a line of a rule file.
fn parse_rule(line: &str) -> Result<Rule, String> {
    let mut parts = line.split_whitespace();
    let level = match parts.next().ok_or("no level")? {
        "ignored" | "ignore" => Tier::Ignored,
        "low" => Tier::Low,
        "normal" => Tier::Normal,
        "internal" => Tier::Internal,
        "high" => Tier::High,
        "highfile" => Tier::HighFile,
        other => return Err(format!("unknown level: {other}")),
    };
    let pattern = parts.next().ok_or("no pattern")?;
    let mut folders = false;
    let mut files = false;
    for flag in parts {
        match flag {
            "d" | "folders" | "dirs" => folders = true,
            "f" | "files" => files = true,
            // The flags can be written together: `fd` is both.
            other if !other.is_empty() && other.chars().all(|c| c == 'd' || c == 'f') => {
                for c in other.chars() {
                    if c == 'd' {
                        folders = true;
                    } else {
                        files = true;
                    }
                }
            }
            other => return Err(format!("unknown flag: {other}")),
        }
    }
    if !folders && !files {
        folders = true;
        files = true;
    }
    let re = crate::regex::Regex::new_sensitive(pattern).map_err(|e| e.to_string())?;
    Ok(Rule { re, level, folders, files })
}

/// A rule with a pattern, before the flags are set.
fn rule(level: Tier, pattern: &str) -> Rule {
    Rule { re: crate::regex::Regex::new_sensitive(pattern).expect("the default rules' patterns compile"), level, folders: false, files: false }
}

impl Rule {
    /// The rule is for folders.
    fn folders(mut self, yes: bool) -> Rule {
        self.folders = yes;
        self
    }
    /// The rule is for files.
    fn files(mut self, yes: bool) -> Rule {
        self.files = yes;
        self
    }
}

/// The offset a level is worth in the score.
///
/// The tier is what decides the order; the offset is what the score says
/// about a record that shares a tier with another. It is small on purpose:
/// the tier is the level, and the score is the match.
pub fn offset(level: Tier) -> i32 {
    match level {
        Tier::Ignored => -100,
        Tier::Low => -30,
        Tier::Normal => 0,
        Tier::Internal => 10,
        Tier::High => 40,
        Tier::HighFile => 60,
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;

    fn level(text: &str, name: &str, for_files: bool) -> Option<Tier> {
        Rules::parse(text).level_of(name.as_bytes(), for_files)
    }

    #[test]
    fn a_rule_gives_its_level_to_what_it_names() {
        assert_eq!(level("low ^node_modules$", "node_modules", false), Some(Tier::Low));
        assert_eq!(level("low ^node_modules$", "node_modules", true), Some(Tier::Low));
        assert_eq!(level("low ^node_modules$", "src", false), None, "another name is not it");
    }

    #[test]
    fn the_flags_say_who_the_rule_is_for() {
        let text = "low ^node_modules$ d";
        assert_eq!(level(text, "node_modules", false), Some(Tier::Low), "the folder");
        assert_eq!(level(text, "node_modules", true), None, "not its files");
        let text = "low ^node_modules$ f";
        assert_eq!(level(text, "node_modules", false), None, "not the folder");
        assert_eq!(level(text, "node_modules", true), Some(Tier::Low), "its files");
        // No flags at all means both.
        let text = "low ^node_modules$";
        assert_eq!(level(text, "node_modules", false), Some(Tier::Low));
        assert_eq!(level(text, "node_modules", true), Some(Tier::Low));
    }

    #[test]
    fn the_closest_rule_wins() {
        let text = "low ^build$ fd\nhigh ^build$ fd";
        assert_eq!(level(text, "build", false), Some(Tier::High), "the last rule written wins");
    }

    /// A line that does not parse is reported, and the other lines stay.
    ///
    /// The file is a list of rules, and one broken line is one rule
    /// missing — not a file that says nothing. Dropping the whole file
    /// turned every rule the user wrote off, and the search answered a
    /// different question than the one in the file.
    #[test]
    fn a_line_that_does_not_parse_is_reported_and_the_rest_stay() {
        let rules = Rules::parse("low ^node_modules$\nbogus ^x$\nlow [unclosed\nlow ^x$ q\nlow");
        // The rule that parsed is the rule.
        assert_eq!(rules.level_of(b"node_modules", false), Some(Tier::Low), "the good rule stands");
        // And each broken line is said, with the line it is on.
        let said = rules.warnings();
        assert_eq!(said.len(), 4, "every broken line is said: {said:?}");
        assert!(said[0].starts_with("line 2:"), "the line is named: {}", said[0]);
        assert!(said[0].contains("unknown level"), "and why: {}", said[0]);
        assert!(said[1].contains("line 3:"), "the line is named: {}", said[1]);
        assert!(said[2].contains("line 4:"), "the line is named: {}", said[2]);
        assert!(said[3].contains("line 5:"), "the line is named: {}", said[3]);
        // A file that parses says nothing.
        assert!(Rules::parse("low ^node_modules$").warnings().is_empty(), "a good file is quiet");
    }

    /// The shape never rejects a name the engine accepts.
    ///
    /// The shape is a necessary condition, not a sufficient one: it may
    /// admit a name the engine rejects, but it must never reject a name
    /// the engine accepts. That is the whole of its contract, and this
    /// is where it is checked -- over the default rules and over the
    /// names that exercise them.
    #[test]
    fn the_shape_never_rejects_a_match() {
        let rules = Rules::defaults();
        let names = [
            "node_modules",
            "__pycache__",
            "site-packages",
            "Pods",
            "venv",
            "bower_components",
            "target",
            "build",
            "dist",
            "out",
            "vendor",
            "deps",
            "tmp",
            "temp",
            "Caches",
            "cache",
            "caches",
            "Logs",
            "DerivedData",
            "CoreSimulator",
            "folders",
            "Containers",
            "Group Containers",
            "Application Support",
            "Library",
            "System",
            "opt",
            "Applications",
            ".git",
            ".hidden",
            "foo.app",
            "Foo.APP",
            "x.framework",
            "x.bundle",
            "x.dSYM",
            "x.sdk",
            "src",
            "main.rs",
            "notes.txt",
            "",
            "node_modules2",
            "xnode_modules",
            "app",
            "myapp",
        ];
        for rule in &rules.rules {
            for name in names {
                if rule.re.is_match_bytes(name.as_bytes()) {
                    assert!(rule.re.shape().maybe(name.as_bytes()), "the shape of {:?} rejected {name:?}, which the rule matches", rule.re.pattern());
                }
            }
        }
    }

    /// The shape of the default rules is what makes them cheap.
    ///
    /// Seven of them name a whole folder, one says "starts with a dot",
    /// and two say "ends with". A name that is none of those is rejected
    /// without the engine running at all.
    #[test]
    fn the_default_rules_have_a_shape() {
        let rules = Rules::defaults();
        let shapes: Vec<(&str, bool, bool, bool)> = rules
            .rules
            .iter()
            .map(|r| {
                let shape = r.re.shape();
                (r.re.pattern(), shape.whole.is_some(), shape.start.is_some(), shape.end.is_some())
            })
            .collect();
        for (pattern, whole, start, end) in &shapes {
            assert!(*whole || *start || *end, "the rule {pattern:?} has no shape: the engine would run on every name");
        }
    }

    #[test]
    fn the_default_rules_reproduce_the_table() {
        let r = Rules::defaults();
        assert_eq!(r.level_of(b"node_modules", false), Some(Tier::Low));
        assert_eq!(r.level_of(b"target", false), Some(Tier::Low));
        assert_eq!(r.level_of(b".git", false), Some(Tier::Low));
        assert_eq!(r.level_of(b"Applications", false), Some(Tier::Internal));
        assert_eq!(r.level_of(b"Documents", false), None, "a name the table did not have is normal");
    }
}
