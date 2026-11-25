// SPDX-License-Identifier: Apache-2.0

//! The paths the user typed, and the home directory a `~` names.
//!
//! A path that begins with `~` names the user's home directory: `~` is the
//! home, `~/Documents` is a folder inside it. The query language already
//! spells it that way — `dir:~/src` and `in:~/src` are the scope filters'
//! own expansion — and the pack store's paths do the same, so a `--root ~`
//! that searched a folder literally named `~` was the one place where the
//! tree disagreed with itself.
//!
//! It disagreed silently, which is the worst way to disagree: the search
//! answered, and the answer was about a folder that does not exist. A POSIX
//! shell expands `~` before rgy ever sees it; Windows PowerShell does not,
//! and the example every document carries — `searchctl --root ~ "invoice"` —
//! became a search of nothing that looked exactly like a tree with no
//! invoice in it.
//!
//! The expansion is the platform's own home: `USERPROFILE` on Windows and
//! `HOME` everywhere else, with the other one as the fallback. A path that
//! does not start with `~`, a `~` that is not at the front, a `~` that names
//! another user (`~someone`), and a machine with no home at all all come
//! back as they came: the path the user typed, and not a guess about it.

use alloc::string::{String, ToString};

/// The user's home directory, as the environment names it.
///
/// `USERPROFILE` on Windows and `HOME` everywhere else, with the other one as
/// the fallback: a machine that sets one of them and not the other still has
/// a home. `None` when the machine names none.
#[cfg(feature = "std")]
pub fn home_dir() -> Option<std::path::PathBuf> {
    let (first, second) = if cfg!(windows) {
        ("USERPROFILE", "HOME")
    } else {
        ("HOME", "USERPROFILE")
    };
    std::env::var_os(first)
        .or_else(|| std::env::var_os(second))
        .map(std::path::PathBuf::from)
}

/// Expand a leading `~` into the user's home directory.
///
/// A path that does not start with `~` comes back as it came, and so does a
/// machine with no home: an expansion that cannot be done is not an
/// expansion, and a search of the wrong tree is worse than one of the tree
/// the user named.
#[cfg(feature = "std")]
pub fn expand_home(path: &str) -> String {
    match home_dir() {
        Some(home) => expand_home_with(path, &home.to_string_lossy()),
        None => path.to_string(),
    }
}

/// The expansion, with the home directory given.
///
/// The pure half, so the test does not have to mutate the process
/// environment: a test that sets `HOME` races every other test in the
/// process, and the tree learned that the hard way.
pub fn expand_home_with(path: &str, home: &str) -> String {
    let Some(rest) = path.strip_prefix('~') else {
        return path.to_string();
    };
    // `~` alone is the home, and `~/x` and `~\x` are inside it. A `~` that
    // is followed by anything else is not a path this can spell: `~someone`
    // is another user's home, and this does not know that user's name.
    if !rest.is_empty() && !rest.starts_with('/') && !rest.starts_with('\\') {
        return path.to_string();
    }
    let trimmed = home.trim_end_matches(['/', '\\']);
    // The home that is the separator itself -- the root user's `/` -- has
    // nothing to trim and everything to keep. A drive root is the same
    // thing: `C:\` trimmed of its separator is `C:`, and on Windows `C:`
    // is not the root of the drive -- it is *the current directory* on
    // drive C:, which is a scope the caller never asked for. The
    // separator is what says which one it is, so it is kept.
    let base = if trimmed.is_empty() || trimmed.ends_with(':') {
        home
    } else {
        trimmed
    };
    if base.is_empty() {
        return path.to_string();
    }
    if rest.is_empty() {
        return base.to_string();
    }
    // The separator is the platform's own: a Windows home keeps its `C:\`
    // and a unix one its `/`.
    let sep = if home.contains('\\') { '\\' } else { '/' };
    let mut out = String::with_capacity(base.len() + 1 + rest.len());
    out.push_str(base);
    // A home that is already the separator does not take another one: that
    // would spell `//Documents`.
    if !base.ends_with('/') && !base.ends_with('\\') {
        out.push(sep);
    }
    out.push_str(rest.trim_start_matches(['/', '\\']));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tilde_is_the_home() {
        assert_eq!(expand_home_with("~", "/home/you"), "/home/you");
        assert_eq!(expand_home_with("~", "C:\\Users\\you"), "C:\\Users\\you");
    }

    #[test]
    fn a_tilde_with_a_path_is_inside_the_home() {
        assert_eq!(
            expand_home_with("~/Documents", "/home/you"),
            "/home/you/Documents"
        );
        assert_eq!(
            expand_home_with("~\\Documents", "C:\\Users\\you"),
            "C:\\Users\\you\\Documents"
        );
    }

    #[test]
    fn a_path_that_does_not_start_with_a_tilde_is_left_alone() {
        assert_eq!(expand_home_with("/home/you", "/home/other"), "/home/you");
        assert_eq!(expand_home_with(".", "/home/other"), ".");
        assert_eq!(expand_home_with("", "/home/other"), "");
    }

    #[test]
    fn a_tilde_that_names_another_user_is_left_alone() {
        // `~someone` is another user's home, and this does not know that
        // user's name. Spelling it as the current user's home would search
        // the wrong tree.
        assert_eq!(expand_home_with("~someone", "/home/you"), "~someone");
        assert_eq!(
            expand_home_with("~someone/Documents", "/home/you"),
            "~someone/Documents"
        );
    }

    #[test]
    fn a_tilde_inside_the_path_is_not_the_home() {
        assert_eq!(
            expand_home_with("/tmp/~/backup", "/home/you"),
            "/tmp/~/backup"
        );
    }

    #[test]
    fn a_machine_with_no_home_leaves_the_path_alone() {
        assert_eq!(expand_home_with("~", ""), "~");
        assert_eq!(expand_home_with("~/Documents", ""), "~/Documents");
    }

    #[test]
    fn the_home_that_is_the_separator_itself_is_kept() {
        // The root user's home is `/`, and trimming it would leave nothing
        // to join the rest of the path to.
        assert_eq!(expand_home_with("~", "/"), "/");
        assert_eq!(expand_home_with("~/Documents", "/"), "/Documents");
    }

    #[test]
    fn a_home_that_is_a_drive_root_keeps_its_separator() {
        // `C:\` trimmed of its separator is `C:`, and on Windows `C:` is
        // not the root of the drive: it is the current directory on drive
        // C:. A scope that quietly becomes "wherever the daemon was
        // started" is the opposite of what the caller asked for.
        assert_eq!(expand_home_with("~", "C:\\"), "C:\\");
        assert_eq!(expand_home_with("~/Documents", "C:\\"), "C:\\Documents");
        // And the unix root still is what it was.
        assert_eq!(expand_home_with("~", "/"), "/");
    }

    #[test]
    fn a_trailing_separator_on_the_home_is_not_doubled() {
        assert_eq!(
            expand_home_with("~/Documents", "/home/you/"),
            "/home/you/Documents"
        );
        assert_eq!(
            expand_home_with("~/Documents", "C:\\Users\\you\\"),
            "C:\\Users\\you\\Documents"
        );
    }
}
