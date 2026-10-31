// SPDX-License-Identifier: Apache-2.0

//! The tree's own walker, held to the crate it replaced.
//!
//! **The oracle.** The `ignore` crate was the walker of the pack store
//! before this one, and it is still the reference: every tree of the
//! corpus is walked through both, with the same flags, and the two sets
//! of entries have to be the same. A store built with one walker and
//! updated with the other has to see the same corpus, or the update
//! tombstones files the build kept.
//!
//! **The corpus.** A tree with a repository in it, ignore files at two
//! levels, the repository's own exclude file, a rule that says keep, a
//! rule that matches a directory, a rule that is anchored, a rule with a
//! separator in it, a class, a rule the reference refuses, hidden files
//! and a hidden directory.
//!
//! **The two matchers.** The walker is only the half of it: the globs
//! and the ignore files are matched by the tree's own engine, and those
//! are held to the crate's own matchers here, over a corpus of globs and
//! a corpus of lines.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use alloc::vec::Vec;

use super::walk::{OverrideBuilder, WalkBuilder};

/// The flags a walk is compared under.
struct Config {
    /// Follow the symbolic links.
    follow_links: bool,
    /// Leave the hidden files out.
    hidden: bool,
    /// Read the ignore files.
    gitignore: bool,
    /// Read the ignore files of the directories above the root.
    parents: bool,
    /// The deepest level the walk reaches.
    max_depth: Option<usize>,
    /// The caller's globs.
    globs: Vec<&'static str>,
}

/// The configurations the two walkers are compared under.
fn configs() -> Vec<Config> {
    vec![
        Config {
            follow_links: false,
            hidden: true,
            gitignore: true,
            parents: true,
            max_depth: None,
            globs: Vec::new(),
        },
        Config {
            follow_links: false,
            hidden: false,
            gitignore: true,
            parents: true,
            max_depth: None,
            globs: Vec::new(),
        },
        Config {
            follow_links: false,
            hidden: true,
            gitignore: false,
            parents: false,
            max_depth: None,
            globs: Vec::new(),
        },
        Config {
            follow_links: false,
            hidden: true,
            gitignore: true,
            parents: false,
            max_depth: None,
            globs: Vec::new(),
        },
        Config {
            follow_links: false,
            hidden: true,
            gitignore: true,
            parents: true,
            max_depth: Some(1),
            globs: Vec::new(),
        },
        Config {
            follow_links: false,
            hidden: true,
            gitignore: true,
            parents: true,
            max_depth: Some(2),
            globs: Vec::new(),
        },
        Config {
            follow_links: false,
            hidden: true,
            gitignore: true,
            parents: true,
            max_depth: None,
            globs: vec!["*.rs"],
        },
        Config {
            follow_links: false,
            hidden: true,
            gitignore: true,
            parents: true,
            max_depth: None,
            globs: vec!["*.rs", "!main.rs"],
        },
        Config {
            follow_links: false,
            hidden: true,
            gitignore: true,
            parents: true,
            max_depth: None,
            globs: vec!["src/**"],
        },
        Config {
            follow_links: false,
            hidden: true,
            gitignore: true,
            parents: true,
            max_depth: None,
            globs: vec!["**"],
        },
        Config {
            follow_links: false,
            hidden: true,
            gitignore: true,
            parents: true,
            max_depth: None,
            globs: vec!["sub/**", "!sub/deep/*.txt"],
        },
    ]
}

/// The corpus: the tree the two walkers are compared on.
fn build_corpus(root: &Path) {
    let write = |path: PathBuf, bytes: &[u8]| {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, bytes).unwrap();
    };
    // A repository: git's own rules apply inside one, and nowhere else.
    fs::create_dir_all(root.join(".git")).unwrap();
    write(root.join(".git").join("HEAD"), b"ref: refs/heads/main\n");
    // The exclude file of the repository.
    write(
        root.join(".git").join("info").join("exclude"),
        b"excluded.txt\n",
    );
    // The rules of the root: a rule that matches at any depth, a rule
    // that matches a directory, a rule that says keep, a rule that is
    // anchored to the root, a rule with a separator in it, a class, a
    // comment, an empty line, and a rule the reference refuses.
    write(
        root.join(".gitignore"),
        b"*.log\nbuild/\n!keep.log\n/only-here.txt\n# a comment\n\nsrc/*.rs\n[abc].txt\nbroken[.txt\n",
    );
    // The `.ignore` file, which outranks the `.gitignore` file.
    write(root.join(".ignore"), b"secret*\n");
    // The files.
    write(root.join("notes.txt"), b"notes\n");
    write(root.join("only-here.txt"), b"only here\n");
    write(root.join("src").join("main.rs"), b"fn main() {}\n");
    write(root.join("src").join("util.rs"), b"fn util() {}\n");
    write(root.join("src").join(".hidden.rs"), b"hidden\n");
    write(root.join("src").join("a.txt"), b"a\n");
    write(root.join("build").join("out.o"), b"object\n");
    write(root.join("logs").join("a.log"), b"a\n");
    write(root.join("logs").join("keep.log"), b"keep\n");
    write(root.join(".hidden_dir").join("x.txt"), b"x\n");
    write(root.join("secret.txt"), b"secret\n");
    write(root.join("excluded.txt"), b"excluded\n");
    // The rules of a directory further down: a rule that matches at any
    // depth below it, and a rule that is anchored to it.
    write(root.join("sub").join(".gitignore"), b"*.tmp\n/deep.txt\n");
    write(root.join("sub").join("deep").join("a.tmp"), b"a\n");
    write(root.join("sub").join("deep").join("b.txt"), b"b\n");
    write(root.join("sub").join("deep").join("plain.txt"), b"plain\n");
    write(root.join("sub").join("deep.txt"), b"deep\n");
    // A name with a backslash in it. Only Unix can hold one: there the
    // backslash is a letter of the name and not a separator, so the rule
    // with a separator in it does not reach it.
    #[cfg(unix)]
    write(root.join("src\\main.rs"), b"fn main() {}\n");
}

/// The corpus of a repository that is BELOW the walk root, with a rule
/// above it.
///
/// The rules of a repository are the ones of its own directory and of the
/// directories under it: a `.gitignore` above the repository is a file of
/// something else, and it does not reach into the repository. The corpus
/// is here because the reference says so, and because the walker has to
/// say the same.
fn build_corpus_below(root: &Path) {
    let write = |path: PathBuf, bytes: &[u8]| {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, bytes).unwrap();
    };
    // A rule above the repository: it is not the repository's.
    write(root.join(".gitignore"), b"*.txt\n");
    // The repository, below the walk root.
    fs::create_dir_all(root.join("repo").join(".git")).unwrap();
    write(
        root.join("repo").join(".git").join("HEAD"),
        b"ref: refs/heads/main\n",
    );
    // The rules of the repository itself.
    write(root.join("repo").join(".gitignore"), b"*.log\n");
    // A file the rule above the repository would take out, and one the
    // repository's own rule takes out.
    write(root.join("repo").join("sub").join("x.txt"), b"x\n");
    write(root.join("repo").join("sub").join("y.log"), b"y\n");
}

/// The tree's own walker: the paths it finds.
fn ours(root: &Path, config: &Config) -> BTreeSet<PathBuf> {
    let mut builder = WalkBuilder::new(root);
    builder
        .follow_links(config.follow_links)
        .hidden(config.hidden)
        .git_ignore(config.gitignore)
        .git_global(config.gitignore)
        .git_exclude(config.gitignore)
        .parents(config.parents)
        .max_depth(config.max_depth);
    builder.filter_entry(|entry| {
        !entry
            .file_name()
            .to_str()
            .is_some_and(crate::walk_policy::is_vcs_dir)
    });
    if !config.globs.is_empty() {
        let mut overrides = OverrideBuilder::new(Path::new("."));
        for glob in &config.globs {
            overrides.add(glob).unwrap();
        }
        builder.overrides(overrides.build().unwrap());
    }
    builder
        .build()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path().to_path_buf())
        .collect()
}

/// The oracle: the paths the crate finds.
fn theirs(root: &Path, config: &Config) -> BTreeSet<PathBuf> {
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .follow_links(config.follow_links)
        .hidden(config.hidden)
        .git_ignore(config.gitignore)
        .git_global(config.gitignore)
        .git_exclude(config.gitignore)
        .parents(config.parents)
        .max_depth(config.max_depth);
    builder.filter_entry(|entry| {
        !entry
            .file_name()
            .to_str()
            .is_some_and(crate::walk_policy::is_vcs_dir)
    });
    if !config.globs.is_empty() {
        let mut overrides = ignore::overrides::OverrideBuilder::new(".");
        for glob in &config.globs {
            overrides.add(glob).unwrap();
        }
        builder.overrides(overrides.build().unwrap());
    }
    builder
        .build()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path().to_path_buf())
        .collect()
}

/// The corpus, in a folder of its own.
///
/// **The folder is not shared.** Two tests of the same process that walked
/// the same tree would see each other's tree rebuilt under them, and a walk
/// that catches a half-built tree is a walk of nothing: the two walkers
/// would disagree about files that are simply not there yet. The name of
/// the test and a counter go into the folder's name, so no two walks ever
/// share a tree.
fn corpus(name: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "rgy-walk-oracle-{}-{name}-{unique}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    build_corpus(&root);
    root
}

/// The corpus of a repository below the walk root, in a folder of its own.
fn corpus_below(name: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "rgy-walk-oracle-{}-{name}-{unique}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    build_corpus_below(&root);
    root
}

/// **The two walkers have to find the same entries.** Every flag, every
/// glob, every tree.
#[test]
fn the_tree_walker_agrees_with_the_crate_it_replaced() {
    let root = corpus("compare");
    for config in configs() {
        let ours = ours(&root, &config);
        let theirs = theirs(&root, &config);
        assert_eq!(
            ours, theirs,
            "the walkers disagree (hidden: {}, gitignore: {}, parents: {}, depth: {:?}, globs: {:?})",
            config.hidden, config.gitignore, config.parents, config.max_depth, config.globs
        );
    }
    let _ = fs::remove_dir_all(&root);
}

/// **The corpus is not empty, and the rules do something.** A test that
/// compares two walks over a tree where every rule is inert proves
/// nothing.
#[test]
fn the_corpus_is_one_the_rules_speak_to() {
    let root = corpus("corpus");
    let all = theirs(&root, &configs()[0]);
    // The repository's state is left out by the walker's own rule.
    assert!(
        !all.contains(&root.join(".git")),
        "the .git directory is in the walk"
    );
    // The hidden files are left out.
    assert!(
        !all.contains(&root.join("src").join(".hidden.rs")),
        "a hidden file is in the walk"
    );
    assert!(
        !all.contains(&root.join(".hidden_dir")),
        "a hidden directory is in the walk"
    );
    // The rules speak.
    assert!(
        !all.contains(&root.join("logs").join("a.log")),
        "a *.log file is in the walk"
    );
    assert!(
        all.contains(&root.join("logs").join("keep.log")),
        "a kept file is not in the walk"
    );
    assert!(
        !all.contains(&root.join("build")),
        "a build directory is in the walk"
    );
    assert!(
        !all.contains(&root.join("secret.txt")),
        "a secret file is in the walk"
    );
    assert!(
        !all.contains(&root.join("only-here.txt")),
        "an anchored rule did not fire"
    );
    assert!(
        !all.contains(&root.join("sub").join("deep").join("a.tmp")),
        "a nested rule did not fire"
    );
    assert!(
        !all.contains(&root.join("sub").join("deep.txt")),
        "a nested anchored rule did not fire"
    );
    assert!(
        !all.contains(&root.join("excluded.txt")),
        "the exclude file did not fire"
    );
    assert!(
        !all.contains(&root.join("src").join("a.txt")),
        "a class did not fire"
    );
    // And the class reaches into the directories under it: a rule with no
    // separator in it matches at any depth.
    assert!(
        !all.contains(&root.join("sub").join("deep").join("b.txt")),
        "a class did not reach into a directory"
    );
    assert!(
        !all.contains(&root.join("src").join("main.rs")),
        "a rule with a separator in it did not fire"
    );
    // And the rest is there.
    assert!(
        all.contains(&root.join("notes.txt")),
        "a note is not in the walk"
    );
    assert!(
        all.contains(&root.join("sub").join("deep").join("plain.txt")),
        "a file is not in the walk"
    );
    // And a name with a backslash in it is one name, not two.
    #[cfg(unix)]
    assert!(
        all.contains(&root.join("src\\main.rs")),
        "a name with a backslash in it was read as a separator"
    );
    let _ = fs::remove_dir_all(&root);
}

/// **The rules above a repository are not the repository's.** A file the
/// rule above the repository takes out is still part of the corpus, and
/// the walkers have to agree about it.
#[test]
fn the_rules_above_a_repository_are_not_its_own() {
    let root = corpus_below("below");
    for config in configs() {
        let ours = ours(&root, &config);
        let theirs = theirs(&root, &config);
        assert_eq!(
            ours, theirs,
            "the walkers disagree (hidden: {}, gitignore: {}, parents: {}, depth: {:?}, globs: {:?})",
            config.hidden, config.gitignore, config.parents, config.max_depth, config.globs
        );
    }
    let all = theirs(&root, &configs()[0]);
    assert!(
        all.contains(&root.join("repo").join("sub").join("x.txt")),
        "a rule above the repository reached into it"
    );
    assert!(
        !all.contains(&root.join("repo").join("sub").join("y.log")),
        "the rule of the repository did not fire"
    );
    let _ = fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// The globs
// ---------------------------------------------------------------------------

/// The globs the two matchers are compared on.
///
/// Every corner of the translation is in here: a star, a question mark, a
/// class, a group of alternates, a `**` at the front, in the middle and
/// at the back, a rule that ends with a separator, a rule that is
/// anchored, a rule that says the opposite, a rule with a space in it and
/// a rule that is not a glob at all.
fn globs() -> Vec<&'static str> {
    vec![
        "*.rs",
        "!main.rs",
        "src/**",
        "**/target/**",
        "a/**/b",
        "**",
        "*",
        "?",
        "?.rs",
        "[abc].rs",
        "[a-z].rs",
        "[!a-z].rs",
        "[^a-z].rs",
        "src/*.rs",
        "src/main.rs",
        "build/",
        "docs/",
        "/root.rs",
        "**/root.rs",
        "*.{rs,toml}",
        "{a,b}/c",
        "a**b",
        "a**/b",
        "**/",
        "foo/**",
        "foo/**/bar",
        "sp ace.rs",
        "esc\\!.rs",
        "a*b",
        "a?b",
        "[a-]",
        "[-a]",
        "[]a]",
        "[[]",
        "a/b/",
        "**/*.rs",
        "x/**",
        "# a comment",
        "",
    ]
}

/// The paths the globs are tried against.
fn paths() -> Vec<&'static str> {
    vec![
        "main.rs",
        "util.rs",
        "src/main.rs",
        "src/util.rs",
        "src/deep/x.rs",
        "target/debug/x",
        "a/x/b",
        "a/b",
        "a/x/y/b",
        "build/out.o",
        "build",
        "docs/index.md",
        "root.rs",
        "x/root.rs",
        "a.toml",
        "b.toml",
        "a/c",
        "b/c",
        "axb",
        "sp ace.rs",
        "esc!.rs",
        "q.rs",
        "b.rs",
        "-",
        "]",
        "]a]",
        "[",
        "x",
        "x/y",
        "foo/x",
        "foo/x/bar",
        "foo/bar",
    ]
}

/// What the crate's matcher says, in the tree's own words: `None` is
/// nothing, `Some(true)` is keep and `Some(false)` is drop.
fn theirs_override(m: ignore::Match<ignore::overrides::Glob<'_>>) -> Option<bool> {
    if m.is_ignore() {
        Some(false)
    } else if m.is_whitelist() {
        Some(true)
    } else {
        None
    }
}

/// What the crate's matcher of an ignore file says.
fn theirs_rule(m: ignore::Match<&ignore::gitignore::Glob>) -> Option<bool> {
    if m.is_ignore() {
        Some(false)
    } else if m.is_whitelist() {
        Some(true)
    } else {
        None
    }
}

/// **The globs have to mean the same thing.** Every glob, every path, and
/// the path as a file and as a directory.
#[test]
fn the_globs_agree_with_the_crate_it_replaced() {
    for glob in globs() {
        let mut ours = OverrideBuilder::new(Path::new("."));
        let ours = ours.add(glob);
        let mut theirs = ignore::overrides::OverrideBuilder::new(".");
        let theirs = theirs.add(glob);
        assert_eq!(
            ours.is_ok(),
            theirs.is_ok(),
            "the glob `{glob}` is refused by one matcher and not by the other"
        );
        let (Ok(ours), Ok(theirs)) = (ours, theirs) else {
            continue;
        };
        let ours = ours.build().unwrap();
        let theirs = theirs.build().unwrap();
        for path in paths() {
            for is_dir in [false, true] {
                assert_eq!(
                    ours.matched(Path::new(path), is_dir),
                    theirs_override(theirs.matched(Path::new(path), is_dir)),
                    "the glob `{glob}` and the path `{path}` (a directory: {is_dir}) do not mean the same thing"
                );
            }
        }
    }
}

/// **The globs are not all the same.** A corpus of globs that all mean
/// nothing would compare equal and prove nothing.
#[test]
fn the_globs_are_not_all_the_same() {
    let mut answers = 0usize;
    for glob in globs() {
        let mut ours = OverrideBuilder::new(Path::new("."));
        let Ok(ours) = ours.add(glob) else {
            continue;
        };
        let ours = ours.build().unwrap();
        for path in paths() {
            if ours.matched(Path::new(path), false).is_some() {
                answers += 1;
            }
        }
    }
    assert!(
        answers > 50,
        "the corpus of globs only answered {answers} times"
    );
}

// ---------------------------------------------------------------------------
// The ignore files
// ---------------------------------------------------------------------------

/// The lines the two matchers of an ignore file are compared on.
fn lines() -> Vec<&'static str> {
    vec![
        "*.log",
        "!keep.log",
        "build/",
        "/only-here.txt",
        "secret*",
        "sub/",
        "**/deep",
        "a/**/b",
        "?.txt",
        "[abc].txt",
        "[!a-z].txt",
        "*.{rs,toml}",
        "# a comment",
        "",
        "   ",
        "trailing   ",
        "trailing\\ ",
        "\\!literal",
        "\\#literal",
        "!",
        "/",
        "**",
        "*",
        "src/*.rs",
        "**/*.rs",
        "deep/**",
        "x/**/y",
        "a/b/c",
    ]
}

/// **The lines of an ignore file have to mean the same thing.** Every
/// line, every path, and the path as a file and as a directory.
#[test]
fn the_rules_agree_with_the_crate_it_replaced() {
    for line in lines() {
        let ours = super::walk::parse_file(line.as_bytes());
        let mut builder = ignore::gitignore::GitignoreBuilder::new(".");
        let theirs = builder.add_line(None, line);
        let Ok(theirs) = theirs else {
            // The reference refuses the line, and so does the tree.
            assert!(
                super::walk::parse_file(line.as_bytes()).rules.is_empty(),
                "the line `{line}` is refused by the crate and not by the tree"
            );
            continue;
        };
        let theirs = theirs.build().unwrap();
        for path in paths() {
            for is_dir in [false, true] {
                assert_eq!(
                    ours.matched(path.as_bytes(), is_dir),
                    theirs_rule(theirs.matched(Path::new(path), is_dir)),
                    "the line `{line}` and the path `{path}` (a directory: {is_dir}) do not mean the same thing"
                );
            }
        }
    }
}

/// **The lines are not all the same.** A corpus of lines that all mean
/// nothing would compare equal and prove nothing.
#[test]
fn the_rules_are_not_all_the_same() {
    let mut answers = 0usize;
    for line in lines() {
        let rules = super::walk::parse_file(line.as_bytes());
        for path in paths() {
            if rules.matched(path.as_bytes(), false).is_some() {
                answers += 1;
            }
        }
    }
    assert!(
        answers > 50,
        "the corpus of lines only answered {answers} times"
    );
}
