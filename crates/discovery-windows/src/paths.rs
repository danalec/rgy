// SPDX-License-Identifier: Apache-2.0

//! Rebuilding a path from a file reference.
//!
//! The USN journal names a file and its parent's file reference, and
//! nothing else: the path has to be rebuilt from the MFT, one record per
//! level. The parent folders repeat across a batch — every change in a
//! folder asks for the same ancestors — so the paths are remembered.
//!
//! The volume is live: a file can be deleted between the journal's record
//! and the read that would place it, and then there is no path to give.

use alloc::string::String;
use alloc::vec::Vec;

use crate::hmap::Cache;
use crate::ntfs;
use crate::volume::Volume;

/// The tests, and only the tests, need `std`: a temp file to ask the volume
/// about, and the process id to name it after. The crate body is `core`
/// plus `alloc`.
#[cfg(test)]
extern crate std;

/// How many levels the walk goes up before it gives up.
///
/// A path deeper than this is not a path a person has, and a record whose
/// parent chain is a cycle (a corrupted MFT) must not spin.
const MAX_DEPTH: u32 = 64;

/// The paths of a volume, rebuilt from its MFT.
pub struct Paths<'a> {
    volume: &'a Volume,
    /// File reference to the path it holds, relative to the volume's root.
    cache: Cache,
}

impl<'a> Paths<'a> {
    /// The paths of one volume.
    pub fn new(volume: &'a Volume) -> Paths<'a> {
        Paths {
            volume,
            cache: Cache::new(),
        }
    }

    /// The path of the entry with this file reference, relative to the
    /// volume's root.
    ///
    /// `None` when the record is gone or unreadable: the volume is live,
    /// and a file can be deleted between the journal's record and this
    /// read.
    pub fn of(&mut self, frn: u64) -> Option<Vec<u8>> {
        let volume = self.volume;
        path_of(ntfs::frn_number(frn), &mut self.cache, &mut |frn| {
            volume.read_file_record(frn).ok().flatten()
        })
    }

    /// The path a change record names, as a path the disk can open.
    ///
    /// The record names the file and its parent's file reference; the
    /// parent's path comes from the MFT and the file's name from the
    /// record itself.
    pub fn of_record(&mut self, record: &rgy_discovery::UsnRecord) -> Option<String> {
        let root = self.volume.root.clone();
        path_of_record(&root, record, &mut self.cache, &mut |frn| {
            self.volume.read_file_record(frn).ok().flatten()
        })
    }
}

/// The path of an entry, from the records its parents name.
///
/// `read` is how a record is fetched: the volume, in production; a map of
/// hand-built records, in a test.
fn path_of(
    frn: u64,
    cache: &mut Cache,
    read: &mut impl FnMut(u64) -> Option<Vec<u8>>,
) -> Option<Vec<u8>> {
    path_of_at(frn, 0, cache, read)
}

fn path_of_at(
    frn: u64,
    depth: u32,
    cache: &mut Cache,
    read: &mut impl FnMut(u64) -> Option<Vec<u8>>,
) -> Option<Vec<u8>> {
    if depth > MAX_DEPTH {
        return None;
    }
    if let Some(p) = cache.get(frn) {
        return Some(p.to_vec());
    }
    // The volume's root is the tree's own root: its path is empty.
    if frn == ntfs::ROOT_FRN {
        return Some(Vec::new());
    }
    let record = read(frn)?;
    let names = ntfs::parse_file_names(&record, frn);
    let best = ntfs::best_name(&names)?;
    let parent = ntfs::frn_number(best.parent_frn);

    let mut path = Vec::new();
    if parent != frn && parent != ntfs::ROOT_FRN {
        path = path_of_at(parent, depth + 1, cache, read)?;
        if !path.is_empty() {
            path.push(b'/');
        }
    }
    path.extend_from_slice(best.name.as_bytes());
    cache.insert(frn, path.clone());
    Some(path)
}

/// The path a change record names, as a path the disk can open.
///
/// The record's own file reference is not used: a file can have several
/// hard links, and the change happened in the folder the record's *parent*
/// names — that is the folder to re-list, whichever link it was.
fn path_of_record(
    root: &str,
    record: &rgy_discovery::UsnRecord,
    cache: &mut Cache,
    read: &mut impl FnMut(u64) -> Option<Vec<u8>>,
) -> Option<String> {
    let parent = path_of(ntfs::frn_number(record.parent_reference), cache, read)?;
    let root = root.trim_end_matches(['\\', '/']);
    let mut path = Vec::with_capacity(root.len() + parent.len() + record.name.len() + 2);
    path.extend_from_slice(root.as_bytes());
    path.push(b'/');
    if !parent.is_empty() {
        path.extend_from_slice(&parent);
        path.push(b'/');
    }
    path.extend_from_slice(record.name.as_bytes());
    Some(String::from_utf8_lossy(&path).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ntfs::testutil::build_record;
    use alloc::format;
    use alloc::string::ToString;
    // The records of the fixture are a map of the oracle's: the cache
    // under test is the tree's own.
    use hashbrown::HashMap;

    /// The records of a small tree: `\Users` (7), `\Users\me` (42) and
    /// `\Users\me\docs` (99), each naming its parent.
    fn tree_records() -> HashMap<u64, Vec<u8>> {
        HashMap::from([
            (
                7,
                build_record(0, ntfs::ROOT_FRN, ntfs::NAMESPACE_WIN32, "Users"),
            ),
            (42, build_record(0, 7, ntfs::NAMESPACE_WIN32, "me")),
            (99, build_record(0, 42, ntfs::NAMESPACE_WIN32, "docs")),
        ])
    }

    /// A file reference resolves to the path of the folder it is in.
    ///
    /// This is the whole of "item 2": the journal names a file and its
    /// parent's file reference, and the path has to be rebuilt from the
    /// records. A resolution that stopped one level short would name the
    /// wrong folder, and the change would be applied to the wrong tree.
    #[test]
    fn a_file_reference_resolves_to_the_path_of_its_folder() {
        let records = tree_records();
        let mut cache = Cache::new();
        let mut read = |frn: u64| records.get(&frn).cloned();

        assert_eq!(
            path_of(99, &mut cache, &mut read),
            Some(b"Users/me/docs".to_vec())
        );
        assert_eq!(
            path_of(42, &mut cache, &mut read),
            Some(b"Users/me".to_vec())
        );
        assert_eq!(path_of(7, &mut cache, &mut read), Some(b"Users".to_vec()));
        // The volume's root is the tree's own root: its path is empty.
        assert_eq!(
            path_of(ntfs::ROOT_FRN, &mut cache, &mut read),
            Some(Vec::new())
        );
        // A record that is not there has no path.
        assert_eq!(path_of(1234, &mut cache, &mut read), None);
    }

    /// A change record is placed at the path the disk can open.
    ///
    /// The record names the file and its parent's file reference, and the
    /// volume's root is in front of the result: the path is openable, and
    /// it is the one the index's paths are spelled with.
    #[test]
    fn a_change_record_is_placed_at_an_openable_path() {
        let records = tree_records();
        let mut cache = Cache::new();
        let mut read = |frn: u64| records.get(&frn).cloned();

        let record = rgy_discovery::UsnRecord {
            file_reference: 500,
            parent_reference: 42,
            usn: 7,
            reason: 0,
            file_attributes: 0,
            name: "notes.txt".to_string(),
        };
        assert_eq!(
            path_of_record("C:\\", &record, &mut cache, &mut read),
            Some("C:/Users/me/notes.txt".to_string())
        );
    }

    /// The real volume, when the shell can open one.
    ///
    /// Opening a volume handle needs an administrator, so this is skipped
    /// where the handle is refused: the logic is pinned by the tests above,
    /// and this one pins the read against a real MFT.
    #[test]
    fn a_file_reference_resolves_to_the_path_the_file_is_at() {
        use crate::win::{self, FILE_ID_INFO, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};

        let file = std::env::temp_dir().join(format!("rgy_paths_{}.txt", std::process::id()));
        std::fs::write(&file, b"x").expect("the temp file is written");
        let text = file.to_string_lossy().replace('/', "\\");
        if text.len() < 3 || text.as_bytes()[1] != b':' {
            // A UNC or relative temp path: the volume is not obvious.
            let _ = std::fs::remove_file(&file);
            return;
        }
        let drive = &text[..2];

        // The path the operating system holds for the file: the long name
        // of every folder on the way, not the alias the environment
        // spelled `%TEMP%` with. This is the tree's own answer to the
        // same question the MFT answers, and the two have to agree.
        let canonical = rgy_discovery::file::canonicalize(file.as_os_str().as_encoded_bytes())
            .expect("the temp file resolves to its own path");
        let canonical = String::from_utf8_lossy(&canonical).into_owned();

        let volume = match Volume::open(&format!("{drive}\\")) {
            Ok(v) => v,
            Err(_) => {
                // The handle needs an administrator. The logic is pinned
                // above; this one needs a shell that can open a volume.
                let _ = std::fs::remove_file(&file);
                return;
            }
        };

        let wide: Vec<u16> = text.encode_utf16().chain(core::iter::once(0)).collect();
        let handle = win::create_file_w(
            wide.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            OPEN_EXISTING,
            0,
        )
        .expect("the file opens");
        let mut info = FILE_ID_INFO::default();
        let ok = win::get_file_information_by_handle_ex(handle, &mut info);
        let _ = win::close_handle(handle);
        assert!(ok.is_ok(), "the file id is readable");
        let frn = u64::from_le_bytes(info.FileId.Identifier[..8].try_into().expect("8 bytes"));

        let mut paths = Paths::new(&volume);
        let relative = paths.of(frn).expect("the file's path is rebuilt");
        let relative = String::from_utf8_lossy(&relative).into_owned();

        // The rebuilt path is relative to the volume's root, and this index
        // speaks `/`.
        //
        // The MFT holds the names the disk holds: the long spelling of
        // every folder, never the 8.3 alias the environment happens to
        // spell `%TEMP%` with (`C:\Users\RUNNER~1\...` where the disk
        // says `C:\Users\runneradmin\...`). The expected path is
        // therefore the one the operating system hands out, and not the
        // one the test typed — otherwise the test measures which alias
        // of one folder the machine happened to have.
        let tail = canonical
            .strip_prefix(drive)
            .expect("the canonical path is on the drive the file is on")
            .trim_start_matches(['\\', '/'])
            .replace('\\', "/");
        assert_eq!(relative, tail, "the rebuilt path must be the file's own");

        let _ = std::fs::remove_file(&file);
    }
}
