//! DFS directory-block vocabulary (the `discovery::Source` block type).
//!
//! Transported from fsearch `walk.rs`: the kinds/flags every backend lists
//! and the [`Listing`] block the name index lays out in DFS order. The
//! platform walks themselves stay in their crates (macOS `getattrlistbulk`,
//! Windows MFT, Linux `getdents64`).

/// Sentinel for "no child listing" (an entry with no directory table).
pub const NONE: u32 = u32::MAX;

pub const KIND_FILE: u8 = 0;
pub const KIND_DIR: u8 = 1;
pub const KIND_LINK: u8 = 2;
pub const KIND_OTHER: u8 = 3;

/// Entry flag bit: hidden (UF_HIDDEN on macOS, FILE_ATTRIBUTE_HIDDEN on
/// Windows; the backends agree on the bit).
pub const FLAG_HIDDEN: u8 = 1 << 2;
/// Directory that is a mount point the scan did not descend into.
pub const FLAG_MOUNT: u8 = 1 << 3;

/// One directory's entries as listed by a backend, names packed in one
/// buffer. Listing id 0 is the scan root.
#[derive(Clone, Copy)]
pub struct RawEnt {
    pub name_off: u32,
    pub name_len: u16,
    /// kind in the low 2 bits, FLAG_* above.
    pub kind: u8,
    pub size: u64,
    pub mtime: u32,
    /// Temp id of the listing for this directory, or [`NONE`].
    pub child: u32,
}

/// A DFS directory block: one directory's [`RawEnt`]s and packed names.
pub struct Listing {
    pub id: u32,
    pub names: Vec<u8>,
    pub ents: Vec<RawEnt>,
}

/// Folders a scan must not open (macOS Full-Disk-Access gated folders set
/// this; the portable walk honours it the same way).
pub static SKIP: std::sync::OnceLock<Vec<Vec<u8>>> = std::sync::OnceLock::new();

pub fn blocked(path: &[u8]) -> bool {
    SKIP.get().is_some_and(|v| {
        v.iter()
            .any(|s| path.starts_with(s) && (path.len() == s.len() || path[s.len()] == b'/'))
    })
}

fn kind_of(md: &std::fs::Metadata) -> u8 {
    let ft = md.file_type();
    if ft.is_symlink() {
        KIND_LINK
    } else if ft.is_dir() {
        KIND_DIR
    } else if ft.is_file() {
        KIND_FILE
    } else {
        KIND_OTHER
    }
}

fn hidden_flag(md: &std::fs::Metadata) -> u8 {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if md.file_attributes() & 0x2 != 0 {
            return FLAG_HIDDEN;
        }
    }
    #[cfg(not(windows))]
    {
        let _ = md;
    }
    0
}

fn list_dir(path: &std::path::Path) -> Option<Listing> {
    let rd = std::fs::read_dir(path).ok()?;
    let mut names = Vec::new();
    let mut ents = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name();
        let raw = name.to_string_lossy().as_bytes().to_vec();
        let md = e.path().symlink_metadata().ok();
        let Some(md) = md else { continue };
        let name_off = names.len() as u32;
        names.extend_from_slice(&raw);
        ents.push(RawEnt {
            name_off,
            name_len: raw.len() as u16,
            kind: kind_of(&md) | hidden_flag(&md),
            size: md.len(),
            mtime: md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs().min(u32::MAX as u64) as u32)
                .unwrap_or(0),
            child: NONE,
        });
    }
    Some(Listing { id: 0, names, ents })
}

/// Portable whole-tree scan: one [`Listing`] per directory, DFS pre-order,
/// listing 0 = `root`. This is the `discovery::Source` block stream — the
/// macOS getattrlistbulk fast path lives in `discovery-macos` and returns
/// the same blocks. `threads` is accepted for the fsearch call signature;
/// the portable walk is sequential.
pub fn scan(root: &[u8], threads: usize) -> Vec<Listing> {
    let _ = threads;
    let root_path = std::path::PathBuf::from(String::from_utf8_lossy(root).into_owned());
    let mut out: Vec<Listing> = Vec::new();
    let mut next_id = 1u32;
    // (path, parent_slot in out where the child id goes) — filled after the
    // child listing is built.
    let mut stack: Vec<(std::path::PathBuf, Option<(usize, usize)>)> =
        vec![(root_path.clone(), None)];
    while let Some((path, fix)) = stack.pop() {
        if blocked(path.to_string_lossy().as_bytes()) {
            continue;
        }
        let Some(mut listing) = list_dir(&path) else {
            continue;
        };
        let id = if fix.is_none() { 0 } else { next_id };
        if fix.is_some() {
            next_id += 1;
        }
        listing.id = id;
        if let Some((li, ei)) = fix {
            out[li].ents[ei].child = id;
        }
        // Collect subdirectories before pushing so the walk stays DFS.
        let mut kids = Vec::new();
        for (ei, ent) in listing.ents.iter().enumerate() {
            if ent.kind & 3 == KIND_DIR {
                let name = &listing.names
                    [ent.name_off as usize..(ent.name_off as usize + ent.name_len as usize)];
                let mut p = path.clone();
                p.push(String::from_utf8_lossy(name).into_owned());
                kids.push((p, ei));
            }
        }
        let li = out.len();
        out.push(listing);
        for (p, ei) in kids.into_iter().rev() {
            stack.push((p, Some((li, ei))));
        }
    }
    out
}

/// List a single directory (no recursion). Subdirectories come back with
/// `child == NONE`. Used by the live updater.
pub fn list_one(path: &[u8]) -> Option<Listing> {
    let p = String::from_utf8_lossy(path).into_owned();
    list_dir(std::path::Path::new(&p))
}
