// SPDX-License-Identifier: Apache-2.0

//! The lookup surface: five indexes over one record store.
//!
//! The core DB carries five indexes: `file_name`,
//! `folder_name`, `folder_backend`, `file_path` and `folder_path`. They are
//! five ways into the same records, not five copies of them.
//!
//! rgy's index is one flat structure: interned names, DFS ranges for the
//! tree, and a name mask. It answers a name query and it resolves a path,
//! but the lookup surface does not separate files from folders -- and the
//! one index the reference has that rgy does not is `folder_backend`, the map
//! from a folder's name to the volume that owns it.
//!
//! This module is that surface. It is a view over the index, not a second
//! copy of it: every lookup reads the same bytes the search reads, and
//! building it costs one pass over the entries.
//!
//! The five indexes are:
//!
//! * `file_name` -- the basename of the files, with the entries that carry
//!   it;
//! * `folder_name` -- the basename of the folders, with the entries that
//!   carry it;
//! * `folder_backend` -- the volume that owns each folder;
//! * `file_path` -- the full path of a file, resolved from its chain;
//! * `folder_path` -- the full path of a folder, resolved from its chain.

use crate::name_index::Index;
use alloc::vec;
use alloc::vec::Vec;
use rgy_discovery::walk::{KIND_DIR, KIND_FILE, KIND_LINK};

/// The volume that owns a folder.
///
/// rgy indexes one tree, so every folder belongs to the volume that tree
/// is on. The index is the answer, and the field is there so the surface
/// has the shape a multi-volume search needs: when rgy indexes several
/// volumes, the answer is the volume that owns the folder, and the
/// `folder_backend` index says which.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Backend(u32);

impl Backend {
    /// The backend's number: the volume's, in the order they were indexed.
    pub fn id(self) -> u32 {
        self.0
    }
}

/// The five indexes over one record store.
pub struct Lookup<'a> {
    index: &'a Index,
    /// The name ids of the files and of the folders, sorted by name.
    ///
    /// Built when the name indexes are asked for. The pass over the
    /// entries and the two sorts are the price of the surface, and a
    /// caller that only wants the backend does not pay it: the daemon
    /// resolves a path per request, and the resolution wants the path and
    /// the volume, not the name indexes.
    names: crate::sync::OnceCell<Names>,
}

/// The two name indexes: the names of the files and the names of the
/// folders, each sorted by the name's bytes.
struct Names {
    /// The name ids of the files, sorted by name.
    files: Vec<u32>,
    /// The name ids of the folders, sorted by name.
    folders: Vec<u32>,
}

impl<'a> Lookup<'a> {
    /// Build the lookup surface over `index`.
    ///
    /// One pass over the entries: the **name ids** of the files and of the
    /// folders, sorted by name so the lookup is a binary search. The name
    /// id is the index's own -- the entry's name is stored once and the
    /// entries point at it -- so a name several entries carry is one name
    /// here, and the entries behind it come from the index's own
    /// name-to-entries table.
    ///
    /// The pass happens when the name indexes are asked for, not here.
    pub fn new(index: &'a Index) -> Lookup<'a> {
        Lookup { index, names: crate::sync::OnceCell::new() }
    }

    /// The two name indexes, built once.
    fn names(&self) -> &Names {
        self.names.get_or_init(|| {
            let kinds = self.index.kind();
            let ent_name = self.index.ent_name();
            let mut seen = vec![0u8; self.index.u];
            for i in 0..self.index.n {
                seen[ent_name[i] as usize] |= 1 << (kinds[i] & 3);
            }
            let mut file_names = Vec::new();
            let mut folder_names = Vec::new();
            let file_mask = (1 << KIND_FILE) | (1 << KIND_LINK);
            let dir_mask = 1 << KIND_DIR;
            for (k, &m) in seen.iter().enumerate() {
                if m & file_mask != 0 {
                    file_names.push(k as u32);
                }
                if m & dir_mask != 0 {
                    folder_names.push(k as u32);
                }
            }
            // The names are sorted by the name's bytes -- the index's own
            // order, the one the path resolution walks -- so the lookup is
            // a binary search over the same order.
            let by_name = |a: &u32, b: &u32| self.index.uname(*a).cmp(self.index.uname(*b));
            rgy_par::join(|| rgy_par::sort_unstable_by(&mut file_names, by_name), || rgy_par::sort_unstable_by(&mut folder_names, by_name));
            Names { files: file_names, folders: folder_names }
        })
    }

    /// The `file_name` index: the files whose name is `name`.
    ///
    /// The answer is the entries, not the name: a name several files carry
    /// is one name in the index and several entries here.
    pub fn file_name(&self, name: &[u8]) -> Vec<u32> {
        self.entries_of(&self.names().files, name, false)
    }

    /// The `folder_name` index: the folders whose name is `name`.
    ///
    /// Only the folders: a file with that name is not in this index.
    pub fn folder_name(&self, name: &[u8]) -> Vec<u32> {
        self.entries_of(&self.names().folders, name, true)
    }

    /// The `folder_backend` index: the volume that owns the folder.
    ///
    /// The folder is named by its entry index, the way the index holds it.
    /// A file is not a folder and owns no volume of its own -- it is on
    /// the volume of the folder it is in, and the caller asks about that
    /// folder.
    ///
    /// rgy indexes one tree, so the volume is the one the tree is on.
    pub fn folder_backend(&self, entry: u32) -> Option<Backend> {
        let kind = *self.index.kind().get(entry as usize)?;
        (kind & 3 == KIND_DIR).then_some(Backend(0))
    }

    /// The `file_path` index: the full path of the file at `entry`.
    pub fn file_path(&self, entry: u32) -> Vec<u8> {
        let mut out = Vec::new();
        self.index.path(entry as usize, &mut out);
        out
    }

    /// The `folder_path` index: the full path of the folder at `entry`.
    pub fn folder_path(&self, entry: u32) -> Vec<u8> {
        let mut out = Vec::new();
        self.index.path(entry as usize, &mut out);
        out
    }

    /// The entries whose name is `name`, among `names`.
    ///
    /// The names are sorted, so this is a binary search over the range of
    /// the names that start with the same bytes; the entries behind each
    /// name come from the index's own name-to-entries table.
    ///
    /// The kind is checked on the way out: the name table holds every
    /// entry that carries the name, files and folders alike, and the file
    /// index is not supposed to answer with a folder.
    fn entries_of(&self, names: &[u32], name: &[u8], want_dir: bool) -> Vec<u32> {
        let Ok(at) = names.binary_search_by(|&n| self.index.uname(n).cmp(name)) else {
            return Vec::new();
        };
        self.index.name_ents_of(names[at]).iter().copied().filter(|&e| (self.index.kind()[e as usize] & 3 == KIND_DIR) == want_dir).collect()
    }

    /// The number of names in each of the two name indexes.
    pub fn counts(&self) -> (usize, usize) {
        let n = self.names();
        (n.files.len(), n.folders.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::name_index::Index;
    use alloc::vec;
    use rgy_discovery::walk::{KIND_DIR, KIND_FILE, Listing, NONE, RawEnt};

    /// A tree with two folders and two files: `/docs` (a folder) holding
    /// `/docs/notes.txt`, and `/notes` (a folder) holding `/notes/readme.md`.
    fn index() -> Index {
        let listings = vec![
            Listing {
                id: 0,
                names: b"docsnotes".to_vec(),
                ents: vec![
                    RawEnt { name_off: 0, name_len: 4, kind: KIND_DIR, size: 0, mtime: 1, child: 1 },
                    RawEnt { name_off: 4, name_len: 5, kind: KIND_DIR, size: 0, mtime: 1, child: 2 },
                ],
            },
            Listing {
                id: 1,
                names: b"notes.txt".to_vec(),
                ents: vec![RawEnt { name_off: 0, name_len: 9, kind: KIND_FILE, size: 10, mtime: 1, child: NONE }],
            },
            Listing {
                id: 2,
                names: b"readme.md".to_vec(),
                ents: vec![RawEnt { name_off: 0, name_len: 9, kind: KIND_FILE, size: 20, mtime: 1, child: NONE }],
            },
        ];
        Index::build(listings, 0, 1, b"/")
    }

    /// The five indexes answer what they say they answer.
    #[test]
    fn the_five_indexes_answer() {
        let idx = index();
        let l = Lookup::new(&idx);
        // `file_name`: the files, by their basename.
        assert_eq!(l.file_name(b"notes.txt").len(), 1, "the file is found by its name");
        assert_eq!(l.file_name(b"readme.md").len(), 1);
        assert_eq!(l.file_name(b"nosuchfile").len(), 0, "a name nobody has is not found");
        // `folder_name`: the folders, by their basename -- and the folders
        // are not in the file index.
        assert_eq!(l.folder_name(b"docs").len(), 1);
        assert_eq!(l.folder_name(b"notes").len(), 1);
        assert_eq!(l.folder_name(b"notes.txt").len(), 0, "a file is not a folder");
        assert_eq!(l.file_name(b"docs").len(), 0, "a folder is not a file");
        // `folder_backend`: the volume that owns a folder.
        let docs = idx.lookup(b"/docs").expect("the folder resolves");
        assert_eq!(l.folder_backend(docs), Some(Backend(0)), "the folder belongs to the volume");
        // `file_path` and `folder_path`: the full path.
        let notes = idx.lookup(b"/docs/notes.txt").expect("the file resolves");
        assert_eq!(l.file_path(notes), b"/docs/notes.txt".to_vec());
        assert_eq!(l.folder_path(docs), b"/docs".to_vec());
        // The counts say the two name indexes are apart. The root is a
        // folder, so it is in the folder index.
        let (files, folders) = l.counts();
        assert_eq!((files, folders), (2, 3), "two files and three folders -- the root is one");
    }

    /// The name indexes hold the names, not the entries: a name several
    /// entries carry is one name in the index.
    #[test]
    fn a_name_several_entries_carry_is_one_name() {
        let names = b"a.txt".to_vec();
        let a = RawEnt { name_off: 0, name_len: 5, kind: KIND_FILE, size: 1, mtime: 1, child: NONE };
        let b = RawEnt { name_off: 0, name_len: 5, kind: KIND_FILE, size: 1, mtime: 1, child: NONE };
        let idx = Index::build(vec![Listing { id: 0, names, ents: vec![a, b] }], 0, 1, b"/");
        let l = Lookup::new(&idx);
        // The name index holds the distinct names.
        assert_eq!(l.counts().0, 1, "one distinct name");
        // The entries behind it come from the index's own table.
        assert_eq!(l.file_name(b"a.txt").len(), 2, "both entries carry the name");
    }

    /// A name a file and a folder share is in both indexes, and each
    /// index answers with its own kind.
    ///
    /// The name table holds every entry that carries the name, files and
    /// folders alike, and the file index is not supposed to answer with a
    /// folder -- or the folder index with a file.
    #[test]
    fn a_name_a_file_and_a_folder_share_is_in_both_indexes() {
        let listings = vec![Listing {
            id: 0,
            names: b"docs".to_vec(),
            ents: vec![
                RawEnt { name_off: 0, name_len: 4, kind: KIND_DIR, size: 0, mtime: 1, child: NONE },
                RawEnt { name_off: 0, name_len: 4, kind: KIND_FILE, size: 1, mtime: 1, child: NONE },
            ],
        }];
        let idx = Index::build(listings, 0, 1, b"/");
        let l = Lookup::new(&idx);
        let files = l.file_name(b"docs");
        assert_eq!(files.len(), 1, "the file index answers with the file");
        assert_eq!(idx.kind()[files[0] as usize] & 3, KIND_FILE);
        let folders = l.folder_name(b"docs");
        assert_eq!(folders.len(), 1, "the folder index answers with the folder");
        assert_eq!(idx.kind()[folders[0] as usize] & 3, KIND_DIR);
    }

    /// A file owns no volume, and an entry that is not there is not an
    /// answer.
    #[test]
    fn a_file_owns_no_volume_and_a_missing_entry_is_not_an_answer() {
        let idx = index();
        let l = Lookup::new(&idx);
        let docs = idx.lookup(b"/docs").expect("the folder resolves");
        let notes = idx.lookup(b"/docs/notes.txt").expect("the file resolves");
        assert_eq!(l.folder_backend(docs), Some(Backend(0)), "the folder belongs to the volume");
        assert_eq!(l.folder_backend(notes), None, "a file owns no volume of its own");
        assert_eq!(l.folder_backend(u32::MAX), None, "an entry that is not there is not an answer");
    }
}
