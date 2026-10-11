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
