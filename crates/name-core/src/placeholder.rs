// SPDX-License-Identifier: Apache-2.0

//! Cloud placeholders: files whose bytes are not on this disk.
//!
//! OneDrive, Dropbox and the other cloud drives leave a file behind as a
//! placeholder: the directory entry is there, with the size and the times the
//! file had when it was last local, but the bytes are not. Reading it is a
//! request to the cloud. The download is invisible -- the open succeeds, the
//! bytes arrive, and the user's metered connection paid for it -- so a
//! program that opens files to look inside them can pull down gigabytes
//! nobody asked for.
//!
//! Windows says so in the file's own attributes:
//!
//! - `FILE_ATTRIBUTE_RECALL_ON_OPEN` (0x00040000): the file must be fetched
//!   from the cloud before it can be opened.
//! - `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS` (0x00400000): the file must be
//!   fetched before its data can be read.
//!
//! Neither is set on a file that is really on the disk, so asking is the
//! whole answer: [`is_placeholder`] turns the attributes into the yes or the
//! no, and [`skip_reason`] says why, in the words a user can act on.
//!
//! macOS says the same thing with one bit: `SF_DATALESS` in the file's
//! `st_flags`, which the file providers (iCloud Drive, Dropbox, OneDrive,
//! Box) set on the placeholders they leave behind. Apple's `<sys/stat.h>`
//! calls it "file is dataless object", and `FileFlags.dataless` says what
//! it means: "the system will attempt to materialize the file when
//! accessed according to the dataless file materialization policy of the
//! accessing thread or process" -- which is `getiopolicy_np(3)`, the
//! policy the content layer already sets on every reader thread
//! (`no_materialize`). So the question is asked before the open, and the
//! open would not download the file anyway. The bit is translated into the
//! vocabulary above ([`translate_dataless`]), so the decision is one
//! decision on every platform -- and the translation is explicit, because
//! the two vocabularies collide: `SF_APPEND` (macOS) is the same value as
//! `FILE_ATTRIBUTE_RECALL_ON_OPEN` (Windows).
//!
//! The question is asked at the moment the file is about to be opened, and
//! not carried in the index. It is a property of the file *now* -- the user
//! may have pinned the file since the index was built, and the index may
//! have been built on another machine -- while the index format is a
//! cross-language contract (the index format) that has no reason to
//! grow a field for a question only the open path asks.

/// `FILE_ATTRIBUTE_RECALL_ON_OPEN`: the file must be fetched from the cloud
/// before it can be opened.
pub const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x0004_0000;

/// `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS`: the file must be fetched from the
/// cloud before its data can be read.
pub const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;

/// `SF_DATALESS`: the file's bytes are not on this disk (macOS).
///
/// Apple's `<sys/stat.h>`: "file is dataless object". The kernel sets it on
/// the placeholders the file providers leave behind, and it clears the
/// instant the file is downloaded.
pub const SF_DATALESS: u32 = 0x4000_0000;

/// The bits that mean "the bytes are not on this disk", in the vocabulary
/// this module speaks.
///
/// Windows sets the two recall bits. macOS says the same thing with one
/// dataless bit, and [`translate_dataless`] translates it into this
/// vocabulary, so the decision below is one decision on every platform.
/// Nowhere else is there a placeholder to mark -- a sparse file is an
/// ordinary file, whose holes read as the zeros they are.
pub const PLACEHOLDER_BITS: u32 = FILE_ATTRIBUTE_RECALL_ON_OPEN | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS;

/// Why a placeholder is not read, in the words a user can act on.
///
/// "Skipped" alone leaves the user guessing whether the file was empty,
/// locked, or never there; this says what happened and what it would have
/// cost.
pub const PLACEHOLDER_REASON: &str = "a cloud placeholder; reading it would download it";

/// The attributes of a macOS file's `st_flags`, in this module's
/// vocabulary.
///
/// The vocabulary is the rgy's: the platform's answer is translated. A
/// dataless file -- the bytes are not on this disk -- is reported the way
/// Windows reports one, so that the decision is one decision everywhere.
///
/// Pure, so it can be tested on any platform: `SF_DATALESS` is a bit, and
/// the bit is the whole question.
pub fn translate_dataless(flags: u32) -> u32 {
    if flags & SF_DATALESS != 0 { FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS } else { 0 }
}

/// Is a file with these attributes a cloud placeholder -- one whose bytes are
/// not on this disk, and whose reading would fetch them?
///
/// Pure, so the decision can be tested without a cloud drive: the attributes
/// are the whole question, and a file that is really on the disk has none of
/// the placeholder bits.
pub fn is_placeholder(attributes: u32) -> bool {
    attributes & PLACEHOLDER_BITS != 0
}

/// Why a file with these attributes is not read, or `None` when it is an
/// ordinary file that is read as usual.
pub fn skip_reason(attributes: u32) -> Option<&'static str> {
    is_placeholder(attributes).then_some(PLACEHOLDER_REASON)
}

/// The attributes of `path`, as the platform reports them.
///
/// `None` when the path could not be asked at all: it does not exist, or the
/// platform has no such notion (everywhere but Windows and macOS).
///
/// On Windows this is one `GetFileAttributesExW` call -- a question about
/// the directory entry, answered without opening the file, so it fetches
/// nothing. It arrives through `std::fs::metadata` and the standard
/// library's Windows metadata extension, the same way `discovery`'s walk
/// asks for the hidden bit (`crates/discovery/src/walk.rs`), so there is no
/// second spelling of the same call in the tree.
#[cfg(windows)]
pub fn attributes(path: &[u8]) -> Option<u32> {
    // The tree's paths are UTF-8 bytes with '/' separators; Windows takes
    // them as they are (both separators are accepted), and the standard
    // library handles the long-path form itself.
    let p = std::path::Path::new(std::str::from_utf8(path).ok()?);
    let md = std::fs::metadata(p).ok()?;
    use std::os::windows::fs::MetadataExt;
    Some(md.file_attributes())
}

/// The attributes of `path`, as the platform reports them.
///
/// On macOS this is the file's `st_flags`, translated into this module's
/// vocabulary: `SF_DATALESS` becomes the recall bit, so a dataless file
/// is refused the way a OneDrive placeholder is. Asking is a `stat`: it
/// reads the directory entry and never the data, so nothing is
/// materialised.
#[cfg(target_os = "macos")]
pub fn attributes(path: &[u8]) -> Option<u32> {
    let p = std::path::Path::new(std::str::from_utf8(path).ok()?);
    let md = std::fs::metadata(p).ok()?;
    use std::os::macos::fs::MetadataExt;
    Some(translate_dataless(md.st_flags()))
}

/// The attributes of `path`, as the platform reports them.
///
/// There is no placeholder outside Windows and macOS: a sparse file is a
/// file whose blocks are not allocated, and it reads as zeros without
/// asking anyone, so it is an ordinary file here.
#[cfg(not(any(windows, target_os = "macos")))]
pub fn attributes(_path: &[u8]) -> Option<u32> {
    None
}

/// Is this path a cloud placeholder -- a file whose bytes are not on this
/// disk, and whose reading would fetch them?
pub fn is_placeholder_path(path: &[u8]) -> bool {
    attributes(path).is_some_and(is_placeholder)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;

    /// The two bits Windows sets on a placeholder, and one bit it sets on
    /// every ordinary file, so the test can tell "the attribute" from "any
    /// attribute".
    const ARCHIVE: u32 = 0x0020;
    const HIDDEN: u32 = 0x0002;

    /// The bits this module speaks mark a placeholder, and the everyday
    /// attributes do not.
    #[test]
    fn the_placeholder_bits_mark_a_placeholder() {
        assert!(is_placeholder(PLACEHOLDER_BITS), "the placeholder bits mark a placeholder");
        assert!(!is_placeholder(0), "no attributes at all is an ordinary file");
        assert!(!is_placeholder(ARCHIVE | HIDDEN), "the everyday attributes are not a placeholder");
    }

    /// A dataless file (macOS) is reported the way Windows reports one, so
    /// that the decision is one decision on every platform.
    #[test]
    fn a_dataless_file_is_reported_the_way_windows_reports_one() {
        assert_eq!(translate_dataless(SF_DATALESS), FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS, "the dataless bit becomes the recall bit");
        assert!(is_placeholder(translate_dataless(SF_DATALESS)), "a translated dataless file is a placeholder");
        assert_eq!(translate_dataless(0), 0, "an ordinary file translates to nothing");
        assert_eq!(translate_dataless(SF_DATALESS - 1), 0, "the bits below it must not be mistaken for it");
    }

    /// The two bits Windows sets on a placeholder, and one bit it sets on
    /// every ordinary file, so the test can tell "the attribute" from "any
    /// attribute".
    #[cfg(windows)]
    #[test]
    fn a_recall_attribute_marks_a_placeholder() {
        assert!(is_placeholder(FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS), "recall on data access is a placeholder");
        assert!(is_placeholder(FILE_ATTRIBUTE_RECALL_ON_OPEN), "recall on open is a placeholder");
        assert!(
            is_placeholder(FILE_ATTRIBUTE_RECALL_ON_OPEN | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS | ARCHIVE),
            "the bits travel together with the others"
        );
        assert!(!is_placeholder(FILE_ATTRIBUTE_RECALL_ON_OPEN - 1), "the bits below the recall one must not be mistaken for one");
        assert!(
            !is_placeholder(FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS - 0x0010_0000),
            "the bits between the two recall ones must not be mistaken for one"
        );
    }

    #[test]
    fn the_reason_says_what_would_have_happened() {
        let reason = skip_reason(FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS).expect("a placeholder has a reason");
        assert!(reason.contains("cloud placeholder"), "the reason must name the thing: {reason}");
        assert!(reason.contains("download"), "the reason must say what reading it would have done: {reason}");
        assert_eq!(skip_reason(ARCHIVE), None, "an ordinary file is read, so it has no skip reason");
    }

    /// The same decision, asked of a real file: a file that is really on the
    /// disk must not be mistaken for a placeholder, or every file would be
    /// skipped.
    #[test]
    fn a_real_file_is_not_a_placeholder() {
        let dir = std::env::temp_dir().join(format!("rgy_placeholder_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("plain.txt");
        std::fs::write(&p, b"not a placeholder\n").unwrap();
        let bytes = p.to_string_lossy().into_owned().into_bytes();
        assert!(!is_placeholder_path(&bytes), "a file on the disk is not a placeholder");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
