// SPDX-License-Identifier: Apache-2.0

//! Paths as bytes.
//!
//! `no_std` has no `std::path`: a path is a byte string, and the bytes are
//! what the filesystem gave. This tree speaks one shape everywhere -- `/`
//! as the separator, the root as the bytes it arrived in -- so the seam's
//! root is a `Vec<u8>` ([`crate::PortableSource`]) and the platform
//! backends take the same shape: the bytes go to the OS unchanged, and the
//! tree joins them the way the index does.
//!
//! Two functions, and both of them are here because every backend needs
//! them and none of them can have `std`:
//!
//! * [`join`] puts a folder and a name together;
//! * [`cstring`] makes the bytes a C API will take.

/// The Windows spelling is text, and the text functions are only there on
/// Windows (and under the tests, where they are checked on any host).
#[cfg(any(windows, test))]
use alloc::string::String;
use alloc::vec::Vec;

/// Join a folder and a name, with `/` as the separator.
///
/// The rules are the ones `std::path::Path::join` has, because that is
/// what the backends used before they lost `std`:
///
/// * an empty folder gives the name alone -- a relative path, not an
///   absolute one;
/// * a folder that already ends in `/` does not get a second one;
/// * the root `/` keeps its single slash.
pub fn join(dir: &[u8], name: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(dir.len() + 1 + name.len());
    p.extend_from_slice(dir);
    if !dir.is_empty() && !dir.ends_with(b"/") {
        p.push(b'/');
    }
    p.extend_from_slice(name);
    p
}

/// The bytes of a C string: the path, and the NUL the C API wants.
///
/// `None` when the bytes carry a NUL of their own. A path with a NUL in
/// it is not a path, and truncating it at the NUL would name a different
/// file -- so the caller refuses it instead.
pub fn cstring(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.contains(&0) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() + 1);
    out.extend_from_slice(bytes);
    out.push(0);
    Some(out)
}

/// The pointer a C API takes, for a buffer [`cstring`] made.
///
/// The buffer has to outlive the call, which is what the caller's own
/// binding does; this only says where the bytes are.
pub fn as_c_ptr(bytes: &[u8]) -> *const core::ffi::c_char {
    bytes.as_ptr() as *const core::ffi::c_char
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_join_is_the_path_the_tree_speaks() {
        assert_eq!(join(b"/home/user", b"file.txt"), b"/home/user/file.txt");
        assert_eq!(join(b"/", b"home"), b"/home");
        assert_eq!(join(b"/home/", b"file"), b"/home/file");
        // An empty folder is a relative path, as `Path::join` has it.
        assert_eq!(join(b"", b"file.txt"), b"file.txt");
    }

    #[test]
    fn a_c_string_carries_one_nul_and_nothing_else() {
        let c = cstring(b"/home/user").expect("a path without a NUL");
        assert_eq!(c, b"/home/user\0");
        assert_eq!(c.iter().filter(|&&b| b == 0).count(), 1);
    }

    #[test]
    fn a_path_with_a_nul_in_it_is_refused() {
        assert!(cstring(b"/home/u\0ser").is_none());
    }
}

/// The Windows spelling of a path, as text.
///
/// The tree's bytes are UTF-8 and the separator is `/`; Windows wants UTF-16
/// and `\`. This is the one place that translation happens, and it is pure
/// text so that it can be tested on any host — the mistakes it can make
/// (a doubled separator, a lost drive root, a UNC path that lost its
/// leading pair) are the kind that only show up as "that folder is
/// empty" on someone else's machine.
///
/// The text is built with these rules:
///
/// * a NUL in the bytes is refused — a path with a NUL is not a path;
/// * bytes that are not UTF-8 are replaced, because Windows has no way to
///   spell them and truncating would name a different file;
/// * `/` and `\` are the same separator, and a run of them is one;
/// * a leading `\\` is kept: that is what says "UNC";
/// * a trailing separator is dropped — it is not part of a name — except
///   on a drive root (`C:\`), which *is* the root.
#[cfg(any(windows, test))]
pub fn win_text(path: &[u8]) -> Option<String> {
    if path.contains(&0) {
        return None;
    }
    let text = String::from_utf8_lossy(path);
    let mut out = String::with_capacity(text.len());
    let mut sep = false;
    for ch in text.chars() {
        if ch == '/' || ch == '\\' {
            // A UNC path starts with two separators and they are part of
            // the name; anywhere else, one separator is enough.
            if sep {
                if out.len() == 1 && out.as_bytes()[0] == b'\\' {
                    out.push('\\');
                }
            } else {
                out.push('\\');
            }
            sep = true;
            continue;
        }
        sep = false;
        out.push(ch);
    }
    // The root of a drive is `C:\`: three characters, and the slash is
    // the root. Everywhere else the trailing separator is noise.
    while out.len() > 3 && out.ends_with('\\') {
        out.pop();
    }
    Some(out)
}

/// Whether the text is a fully qualified Windows path: a drive (`C:\`) or
/// a UNC share (`\\server\share`).
///
/// Only a qualified path can take the long-path form: the object manager
/// wants a full path there, and a relative one has no business being
/// spelled `\\?\`.
#[cfg(any(windows, test))]
pub fn win_qualified(text: &str) -> bool {
    let b = text.as_bytes();
    // `C:\...`
    if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\' {
        return true;
    }
    // `\\server\share\...`
    text.starts_with("\\\\")
}

/// Whether the text holds a `.` or a `..` component.
///
/// The long-path form turns the path parser off: there, `.` and `..` are
/// names, not movements. A path with one of them in it is therefore not
/// spelled in the long form at all — it keeps the normal one, where the
/// parser is still on.
#[cfg(any(windows, test))]
pub fn win_has_dot(text: &str) -> bool {
    text.split('\\').any(|c| c == "." || c == "..")
}

/// The long-path form of a Windows path, as text.
///
/// `\\?\` turns off the path parser, which is what lets a path be longer
/// than `MAX_PATH`. It only applies to a qualified path with no `.`/`..`
/// in it; anything else keeps the normal spelling, where the parser still
/// expands those components.
///
/// A UNC path takes the UNC spelling of the prefix (`\\?\UNC\`), because
/// `\\?\` followed by a UNC path is not a UNC path any more.
#[cfg(any(windows, test))]
pub fn win_verbatim_text(text: &str) -> String {
    // A path that ALREADY carries the long-path prefix is already in the
    // form the object manager wants: the prefix is not something to be
    // added twice. And a second one is not a longer path — `\\?\` followed
    // by a UNC path is not a UNC path any more, so the drive would become
    // the name of a share called "?" and every call on it would fail.
    if text.starts_with(r"\\?\") {
        return text.into();
    }
    if !win_qualified(text) || win_has_dot(text) {
        return text.into();
    }
    let mut full = String::with_capacity(text.len() + 8);
    if let Some(rest) = text.strip_prefix("\\\\") {
        full.push_str("\\\\?\\UNC\\");
        full.push_str(rest);
    } else {
        full.push_str("\\\\?\\");
        full.push_str(text);
    }
    full
}

/// The UTF-16 a Win32 call takes: the text, NUL-terminated.
///
/// The bytes go to Windows as they are — the tree speaks UTF-8, and the
/// lossless conversion is the boundary's job, not the caller's.
#[cfg(windows)]
pub fn win_wide(text: &str) -> Vec<u16> {
    let mut out: Vec<u16> = text.encode_utf16().collect();
    out.push(0);
    out
}

/// The Windows spelling of a path, ready for a Win32 call.
///
/// The long-path form, when the path can take it.
#[cfg(windows)]
pub fn win_verbatim(path: &[u8]) -> Option<Vec<u16>> {
    let text = win_text(path)?;
    Some(win_wide(&win_verbatim_text(&text)))
}

/// The pattern that lists a folder: the folder, then `\*`.
///
/// The pattern is built in the text and not in the bytes, so the long-path
/// form is applied to the whole of it.
#[cfg(windows)]
pub fn win_pattern(dir: &[u8]) -> Option<Vec<u16>> {
    let mut text = win_text(dir)?;
    if !text.ends_with('\\') {
        text.push('\\');
    }
    text.push('*');
    Some(win_wide(&win_verbatim_text(&text)))
}

#[cfg(test)]
mod win_tests {
    use super::*;

    #[test]
    fn the_separator_is_the_windows_one() {
        assert_eq!(win_text(b"C:/Users/foo").unwrap(), r"C:\Users\foo");
        assert_eq!(win_text(b"C:\\Users\\foo").unwrap(), r"C:\Users\foo");
    }

    #[test]
    fn a_unc_path_keeps_its_leading_pair() {
        assert_eq!(
            win_text(b"\\\\server\\share\\dir").unwrap(),
            r"\\server\share\dir"
        );
        assert_eq!(
            win_text(b"//server/share/dir").unwrap(),
            r"\\server\share\dir"
        );
    }

    #[test]
    fn a_doubled_separator_is_one_separator() {
        assert_eq!(win_text(b"C:/foo//bar").unwrap(), r"C:\foo\bar");
        assert_eq!(win_text(b"C:/foo/").unwrap(), r"C:\foo");
    }

    #[test]
    fn the_root_of_a_drive_keeps_its_slash() {
        assert_eq!(win_text(b"C:/").unwrap(), r"C:\");
        assert_eq!(win_text(b"C:").unwrap(), "C:");
    }

    #[test]
    fn a_nul_is_refused() {
        assert!(win_text(b"C:/foo\0bar").is_none());
    }

    #[test]
    fn only_a_qualified_path_takes_the_long_form() {
        assert_eq!(win_verbatim_text(r"C:\foo"), r"\\?\C:\foo");
        assert_eq!(
            win_verbatim_text(r"\\server\share"),
            r"\\?\UNC\server\share"
        );
        // A relative path has no business in the long form.
        assert_eq!(win_verbatim_text(r"foo\bar"), r"foo\bar");
        // Nor one with a `.` in it: there, `.` is a name, not a movement.
        assert_eq!(win_verbatim_text(r"C:\foo\..\bar"), r"C:\foo\..\bar");
        // And a path that is ALREADY in the long form keeps it: the prefix
        // is not something to be added twice, and a second one turns the
        // drive into the name of a share called "?".
        assert_eq!(win_verbatim_text(r"\\?\C:\foo"), r"\\?\C:\foo");
        assert_eq!(
            win_verbatim_text(r"\\?\UNC\server\share"),
            r"\\?\UNC\server\share"
        );
    }

    #[test]
    fn a_qualified_path_is_a_drive_or_a_share() {
        assert!(win_qualified(r"C:\foo"));
        assert!(win_qualified(r"\\server\share"));
        assert!(!win_qualified(r"foo\bar"));
        assert!(!win_qualified("C:"));
    }
}
