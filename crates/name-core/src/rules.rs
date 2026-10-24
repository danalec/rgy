// SPDX-License-Identifier: Apache-2.0

//! The user's priority rules.
//!
//! Listary's priority is a rule system the user writes: a rule carries a
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
//! The level of a record is the level of the **closest** rule that matches
//! a name in its chain and applies to it: its own name first, then its
//! parent's, and so on. A record no rule reaches is `Normal`.
//!
//! `Ignored` is a level a rule can give, and a record in it ranks last --
//! but it does not yet take the record out of the live set. That is a
//! bigger change than a level (the parity row P.4): a record that is not
//! searchable is not a record the search can skip.

use crate::rank::Tier;

/// One rule: a name pattern, the level it gives, and who it is for.
pub struct Rule {
    /// The name pattern.
    re: regex::bytes::Regex,
    /// The level the rule gives.
    level: Tier,
    /// The rule is for folders (the one whose name matched).
    folders: bool,
    /// The rule is for files (the ones under the folder that matched).
    files: bool,
}

impl Rule {
    /// Does the rule match `name`?
    fn matches(&self, name: &[u8]) -> bool {
        self.re.is_match(name)
    }
}

/// The rules, in the order they were written.
#[derive(Default)]
pub struct Rules {
    rules: Vec<Rule>,
}

impl Rules {
    /// The rules in a file's text.
    ///
    /// A line that does not parse is an error, not a warning: a rule the
    /// user wrote and rgy silently dropped is a rule that does not work.
    pub fn parse(text: &str) -> Result<Rules, String> {
        let mut rules = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            rules.push(parse_rule(line).map_err(|e| format!("line {}: {e}", n + 1))?);
        }
        Ok(Rules { rules })
    }

    /// The rules in a file, read from the disk.
    pub fn read(path: &std::path::Path) -> Result<Rules, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Rules::parse(&text)
    }

    /// The rules beside an index: `<index>.rules`.
    ///
    /// No file is not an error -- the defaults are the rules, and a daemon
    /// that never wrote a file still has them. A file that does not parse is
    /// an error: the user wrote it, and rgy would otherwise ignore it
    /// without saying so. The defaults stand in for it, and the reason
    /// comes back with them.
    pub fn beside(index_path: &std::path::Path) -> (Rules, Option<String>) {
        let path = index_path.with_extension("rules");
        match Rules::read(&path) {
            Ok(rules) => (rules, None),
            Err(why) => {
                let missing = !path.exists();
                (Rules::defaults(), (!missing).then_some(why))
            }
        }
    }

    /// The level a name gets, if a rule matches it.
    ///
    /// `for_files` says whether the name is a file's or a folder's.
    pub fn level_of(&self, name: &[u8], for_files: bool) -> Option<Tier> {
        self.rules.iter().rev().find(|r| r.matches(name) && (if for_files { r.files } else { r.folders })).map(|r| r.level)
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
    let re = regex::bytes::Regex::new(pattern).map_err(|e| e.to_string())?;
    Ok(Rule { re, level, folders, files })
}

/// A rule with a pattern, before the flags are set.
fn rule(level: Tier, pattern: &str) -> Rule {
    Rule { re: regex::bytes::Regex::new(pattern).expect("the default rules' patterns compile"), level, folders: false, files: false }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn level(text: &str, name: &str, for_files: bool) -> Option<Tier> {
        Rules::parse(text).expect("the rules parse").level_of(name.as_bytes(), for_files)
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

    #[test]
    fn a_line_that_does_not_parse_is_an_error() {
        assert!(Rules::parse("bogus ^x$").is_err(), "an unknown level");
        assert!(Rules::parse("low [unclosed").is_err(), "an uncompilable pattern");
        assert!(Rules::parse("low ^x$ q").is_err(), "an unknown flag");
        assert!(Rules::parse("low").is_err(), "no pattern");
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
