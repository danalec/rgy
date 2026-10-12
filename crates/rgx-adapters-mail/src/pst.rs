//! PST/OST adapter: Outlook Personal Storage Table and Offline Storage
//! Table files become searchable text (folder tree, message metadata,
//! bodies).
//!
//! The reader is hand-rolled per [MS-PST] and cross-checked against
//! Microsoft's `outlook-pst-rs` independent implementation. Parsing strategy
//! mirrors the upstream `io::binary::pst` module so both stay in lockstep:
//! Unicode (v23) and ANSI (v14/15) databases, NBT/BBT page trees, data
//! trees (XBLOCK/XXBLOCK), subnode trees, heaps-on-node, BTH indices,
//! property and table contexts, and all three block encodings (none /
//! permute / cyclic). Page CRCs and block signatures are not enforced so
//! lightly damaged files still yield text.
//!
//! [MS-PST]: https://learn.microsoft.com/en-us/openspecs/office_file_formats/ms-pst/ef6a3a75-e0d4-4099-8bd0-00b58c9d4d46

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;
use core::fmt::Write as _;
use core::result::Result as StdResult;
use core::str;
use rgx_core::{bail, Result};
use rgx_core::{Adapter, Outcome};

const PAGE_SIZE: usize = 512;
const PAGE_TRAILER_UNICODE: usize = 16;
const PAGE_TRAILER_ANSI: usize = 12;
const BLOCK_TRAILER_UNICODE: usize = 16;
const BLOCK_TRAILER_ANSI: usize = 12;
const MAX_BLOCK_CB: usize = 8192;

const PT_I32: u16 = 0x0003;
const PT_BOOL: u16 = 0x000B;
const PT_UNICODE: u16 = 0x001F;
const PT_TIME: u16 = 0x0040;

const NID_ROOT_FOLDER: u32 = 0x122;
const NID_TYPE_NORMAL_FOLDER: u32 = 0x02;
const NID_TYPE_SEARCH_FOLDER: u32 = 0x03;
const NID_TYPE_HIERARCHY_TABLE: u32 = 0x0D;
const NID_TYPE_CONTENTS_TABLE: u32 = 0x0E;

const PROP_SUBJECT: u16 = 0x0037;
const PROP_CLIENT_SUBMIT_TIME: u16 = 0x0039;
const PROP_DISPLAY_NAME: u16 = 0x3001;
const PROP_SENDER_NAME: u16 = 0x0C1A;
const PROP_SENDER_EMAIL: u16 = 0x0C1F;
const PROP_DISPLAY_TO: u16 = 0x0E04;
const PROP_DELIVERY_TIME: u16 = 0x0E06;
const PROP_HAS_ATTACH: u16 = 0x0E1B;
const PROP_BODY: u16 = 0x1000;
const PROP_BODY_HTML: u16 = 0x1013;

#[inline]
fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
#[inline]
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
#[inline]
fn u64le(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes([
        b[o],
        b[o + 1],
        b[o + 2],
        b[o + 3],
        b[o + 4],
        b[o + 5],
        b[o + 6],
        b[o + 7],
    ])
}

/// 768-byte MPMAE key table from [MS-PST] 5.2 (`permute`/`cyclic`).
static KEY_DATA: [u8; 768] = [
    65, 54, 19, 98, 168, 33, 110, 187, 244, 22, 204, 4, 127, 100, 232, 93, 30, 242, 203, 42, 116,
    197, 94, 53, 210, 149, 71, 158, 150, 45, 154, 136, 76, 125, 132, 63, 219, 172, 49, 182, 72, 95,
    246, 196, 216, 57, 139, 231, 35, 59, 56, 142, 200, 193, 223, 37, 177, 32, 165, 70, 96, 78, 156,
    251, 170, 211, 86, 81, 69, 124, 85, 0, 7, 201, 43, 157, 133, 155, 9, 160, 143, 173, 179, 15,
    99, 171, 137, 75, 215, 167, 21, 90, 113, 102, 66, 191, 38, 74, 107, 152, 250, 234, 119, 83,
    178, 112, 5, 44, 253, 89, 58, 134, 126, 206, 6, 235, 130, 120, 87, 199, 141, 67, 175, 180, 28,
    212, 91, 205, 226, 233, 39, 79, 195, 8, 114, 128, 207, 176, 239, 245, 40, 109, 190, 48, 77, 52,
    146, 213, 14, 60, 34, 50, 229, 228, 249, 159, 194, 209, 10, 129, 18, 225, 238, 145, 131, 118,
    227, 151, 230, 97, 138, 23, 121, 164, 183, 220, 144, 122, 92, 140, 2, 166, 202, 105, 222, 80,
    26, 17, 147, 185, 82, 135, 88, 252, 237, 29, 55, 73, 27, 106, 224, 41, 51, 153, 189, 108, 217,
    148, 243, 64, 84, 111, 240, 198, 115, 184, 214, 62, 101, 24, 68, 31, 221, 103, 16, 241, 12, 25,
    236, 174, 3, 161, 20, 123, 169, 11, 255, 248, 163, 192, 162, 1, 247, 46, 188, 36, 104, 117, 13,
    254, 186, 47, 181, 208, 218, 61, 20, 83, 15, 86, 179, 200, 122, 156, 235, 101, 72, 23, 22, 21,
    159, 2, 204, 84, 124, 131, 0, 13, 12, 11, 162, 98, 168, 118, 219, 217, 237, 199, 197, 164, 220,
    172, 133, 116, 214, 208, 167, 155, 174, 154, 150, 113, 102, 195, 99, 153, 184, 221, 115, 146,
    142, 132, 125, 165, 94, 209, 93, 147, 177, 87, 81, 80, 128, 137, 82, 148, 79, 78, 10, 107, 188,
    141, 127, 110, 71, 70, 65, 64, 68, 1, 17, 203, 3, 63, 247, 244, 225, 169, 143, 60, 58, 249,
    251, 240, 25, 48, 130, 9, 46, 201, 157, 160, 134, 73, 238, 111, 77, 109, 196, 45, 129, 52, 37,
    135, 27, 136, 170, 252, 6, 161, 18, 56, 253, 76, 66, 114, 100, 19, 55, 36, 106, 117, 119, 67,
    255, 230, 180, 75, 54, 92, 228, 216, 53, 61, 69, 185, 44, 236, 183, 49, 43, 41, 7, 104, 163,
    14, 105, 123, 24, 158, 33, 57, 190, 40, 26, 91, 120, 245, 35, 202, 42, 176, 175, 62, 254, 4,
    140, 231, 229, 152, 50, 149, 211, 246, 74, 232, 166, 234, 233, 243, 213, 47, 112, 32, 242, 31,
    5, 103, 173, 85, 16, 206, 205, 227, 39, 59, 218, 186, 215, 194, 38, 212, 145, 29, 210, 28, 34,
    51, 248, 250, 241, 90, 239, 207, 144, 182, 139, 181, 189, 192, 191, 8, 151, 30, 108, 226, 97,
    224, 198, 193, 89, 171, 187, 88, 222, 95, 223, 96, 121, 126, 178, 138, 71, 241, 180, 230, 11,
    106, 114, 72, 133, 78, 158, 235, 226, 248, 148, 83, 224, 187, 160, 2, 232, 90, 9, 171, 219,
    227, 186, 198, 124, 195, 16, 221, 57, 5, 150, 48, 245, 55, 96, 130, 140, 201, 19, 74, 107, 29,
    243, 251, 143, 38, 151, 202, 145, 23, 1, 196, 50, 45, 110, 49, 149, 255, 217, 35, 209, 0, 94,
    121, 220, 68, 59, 26, 40, 197, 97, 87, 32, 144, 61, 131, 185, 67, 190, 103, 210, 70, 66, 118,
    192, 109, 91, 126, 178, 15, 22, 41, 60, 169, 3, 84, 13, 218, 93, 223, 246, 183, 199, 98, 205,
    141, 6, 211, 105, 92, 134, 214, 20, 247, 165, 102, 117, 172, 177, 233, 69, 33, 112, 12, 135,
    159, 116, 164, 34, 76, 111, 191, 31, 86, 170, 46, 179, 120, 51, 80, 176, 163, 146, 188, 207,
    25, 28, 167, 99, 203, 30, 77, 62, 75, 27, 155, 79, 231, 240, 238, 173, 58, 181, 89, 4, 234, 64,
    85, 37, 81, 229, 122, 137, 56, 104, 82, 123, 252, 39, 174, 215, 189, 250, 7, 244, 204, 142, 95,
    239, 53, 156, 132, 43, 21, 213, 119, 52, 73, 182, 18, 10, 127, 113, 136, 253, 157, 24, 65, 125,
    147, 216, 88, 44, 206, 254, 36, 175, 222, 184, 54, 200, 161, 128, 166, 153, 152, 168, 47, 14,
    129, 101, 115, 228, 194, 162, 138, 212, 225, 17, 208, 8, 139, 42, 242, 237, 154, 100, 63, 193,
    108, 249, 236,
];

#[derive(Debug)]
enum PstErr {
    Sig,
    Version,
    Corrupt(&'static str),
    NotFound(&'static str),
}

impl fmt::Display for PstErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sig => write!(f, "not a PST file (bad magic)"),
            Self::Version => write!(f, "unsupported PST version"),
            Self::Corrupt(m) => write!(f, "corrupt PST structure: {m}"),
            Self::NotFound(m) => write!(f, "not found in PST: {m}"),
        }
    }
}
#[cfg(feature = "std")]
impl std::error::Error for PstErr {}

type PstResult<T> = StdResult<T, PstErr>;

#[derive(Debug)]
enum Prop {
    I32(i32),
    Bool(bool),
    Time(u64),
    Str(String),
    Bin(Vec<u8>),
    Other,
}

impl Prop {
    fn text(&self) -> Option<String> {
        match self {
            Self::Str(s) => Some(s.clone()),
            // Numeric and binary property payloads were parsed and then
            // dropped, so those properties were unreachable to a search.
            // Surface them; `date` and `has_attach` still read Bool/Time
            // directly, so this only affects the string-keyed lookups.
            Self::I32(v) => Some(v.to_string()),
            Self::Bin(b) => str::from_utf8(b).ok().and_then(|s| {
                // only when it is mostly printable, so raw bytes do not
                // turn into noise in the searchable corpus. Count CHARS on
                // both sides: comparing a char count against a byte length
                // would reject every non-Latin script, which is exactly what
                // a search tool most needs to keep.
                let chars = s.chars().count();
                let printable = s
                    .chars()
                    .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
                    .count();
                (chars > 0 && printable * 4 >= chars * 3).then(|| s.to_string())
            }),
            _ => None,
        }
    }
}

#[derive(Clone, Copy)]
struct NodeRec {
    bid_data: u64,
    bid_sub: u64,
}

struct Heap {
    blocks: Vec<Vec<u8>>,
}

impl Heap {
    fn user_root(&self) -> Option<u32> {
        let b = self.blocks.first()?;
        if b.len() < 12 || b[2] != 0xEC {
            return None;
        }
        Some(u32le(b, 4))
    }

    fn client_sig(&self) -> Option<u8> {
        let b = self.blocks.first()?;
        if b.len() < 12 || b[2] != 0xEC {
            return None;
        }
        Some(b[3])
    }

    fn item(&self, hid: u32) -> Option<&[u8]> {
        if hid == 0 {
            return None;
        }
        let block_index = (hid >> 16) as usize;
        let index = ((hid >> 5) & 0x7FF) as usize; // 1-based
        if index == 0 {
            return None;
        }
        let block = self.blocks.get(block_index)?;
        // Every HN block flavor leads with ibHnpm at offset 0.
        let pm_off = u16le(block, 0) as usize;
        if pm_off + 4 > block.len() {
            return None;
        }
        let c_alloc = u16le(block, pm_off) as usize;
        if index > c_alloc {
            return None;
        }
        let table = pm_off + 4;
        if table + index * 2 + 2 > block.len() {
            return None;
        }
        let start = u16le(block, table + (index - 1) * 2) as usize;
        let end = u16le(block, table + index * 2) as usize;
        if end < start || end > block.len() {
            return None;
        }
        Some(&block[start..end])
    }
}

struct Pst<'a> {
    data: &'a [u8],
    unicode: bool,
    crypt: u8,
    nbt_root: (u64, u64),
    bbt_root: (u64, u64),
}

impl<'a> Pst<'a> {
    fn parse(data: &'a [u8]) -> PstResult<Self> {
        if data.len() < PAGE_SIZE || u32le(data, 0) != 0x2142_444E {
            return Err(PstErr::Sig);
        }
        if u16le(data, 8) != 0x4D53 {
            return Err(PstErr::Sig);
        }
        let unicode = match u16le(data, 10) {
            14 | 15 => false,
            23 => true,
            _ => return Err(PstErr::Version),
        };
        if data.len() < 514 {
            return Err(PstErr::Corrupt("header truncated"));
        }
        let crypt = data[513];
        if crypt > 2 {
            return Err(PstErr::Corrupt("unknown crypt method"));
        }
        let root_off = if unicode { 180 } else { 164 };
        if root_off + 40 > data.len() {
            return Err(PstErr::Corrupt("header truncated"));
        }
        let (nbt_root, bbt_root) = if unicode {
            (
                (u64le(data, root_off + 32), u64le(data, root_off + 40)),
                (u64le(data, root_off + 48), u64le(data, root_off + 56)),
            )
        } else {
            (
                (
                    u32le(data, root_off + 16) as u64,
                    u32le(data, root_off + 20) as u64,
                ),
                (
                    u32le(data, root_off + 24) as u64,
                    u32le(data, root_off + 28) as u64,
                ),
            )
        };
        Ok(Self {
            data,
            unicode,
            crypt,
            nbt_root,
            bbt_root,
        })
    }

    fn page_trailer(&self) -> usize {
        if self.unicode {
            PAGE_TRAILER_UNICODE
        } else {
            PAGE_TRAILER_ANSI
        }
    }

    fn block_trailer(&self) -> usize {
        if self.unicode {
            BLOCK_TRAILER_UNICODE
        } else {
            BLOCK_TRAILER_ANSI
        }
    }

    fn page(&self, ib: u64) -> PstResult<(u8, &'a [u8], usize, u8)> {
        let ib = ib as usize;
        if ib + PAGE_SIZE > self.data.len() {
            return Err(PstErr::Corrupt("page out of range"));
        }
        let page = &self.data[ib..ib + PAGE_SIZE];
        let tsize = self.page_trailer();
        let body = &page[..PAGE_SIZE - tsize];
        if body.len() < 8 {
            return Err(PstErr::Corrupt("page header truncated"));
        }
        let c_ent = body[0] as usize;
        let cb_ent = body[2] as usize;
        let ptype = page[PAGE_SIZE - tsize];
        if cb_ent == 0 {
            return Err(PstErr::Corrupt("zero page entry size"));
        }
        let entries = &body[8..];
        if c_ent * cb_ent > entries.len() {
            return Err(PstErr::Corrupt("page entries out of range"));
        }
        Ok((body[3], &entries[..c_ent * cb_ent], cb_ent, ptype))
    }

    fn entry_key(&self, entries: &[u8], i: usize, cb_ent: usize) -> u64 {
        let o = i * cb_ent;
        if self.unicode {
            u64le(entries, o)
        } else {
            u32le(entries, o) as u64
        }
    }

    fn btree_leaf_page(&self, root: (u64, u64), key: u64, ptype: u8) -> PstResult<u64> {
        let mut cur = root.0;
        for _ in 0..32 {
            let (level, entries, cb_ent, got) = self.page(cur)?;
            if got != ptype {
                return Err(PstErr::Corrupt("unexpected btree page type"));
            }
            if level == 0 {
                return Ok(cur);
            }
            let count = entries.len() / cb_ent;
            let key_size = if self.unicode { 8 } else { 4 };
            let mut pick = None;
            for i in 0..count {
                if self.entry_key(entries, i, cb_ent) <= key {
                    pick = Some(i);
                } else {
                    break;
                }
            }
            let i = pick.ok_or(PstErr::NotFound("btree path"))?;
            let o = i * cb_ent + key_size;
            cur = if self.unicode {
                u64le(entries, o)
            } else {
                u32le(entries, o) as u64
            };
        }
        Err(PstErr::Corrupt("btree too deep"))
    }

    fn node(&self, nid: u32) -> PstResult<NodeRec> {
        let leaf = self.btree_leaf_page(self.nbt_root, nid as u64, 0x81)?;
        let (_, entries, cb_ent, _) = self.page(leaf)?;
        for i in 0..entries.len() / cb_ent {
            if self.entry_key(entries, i, cb_ent) == nid as u64 {
                let o = i * cb_ent;
                let (bid_data, bid_sub) = if self.unicode {
                    (u64le(entries, o + 8), u64le(entries, o + 16))
                } else {
                    (u32le(entries, o + 4) as u64, u32le(entries, o + 8) as u64)
                };
                return Ok(NodeRec { bid_data, bid_sub });
            }
        }
        Err(PstErr::NotFound("node"))
    }

    fn block_ref(&self, bid: u64) -> PstResult<(u64, usize)> {
        let want = bid & !1;
        let leaf = self.btree_leaf_page(self.bbt_root, want, 0x80)?;
        let (_, entries, cb_ent, _) = self.page(leaf)?;
        for i in 0..entries.len() / cb_ent {
            if (self.entry_key(entries, i, cb_ent) & !1) == want {
                let o = i * cb_ent;
                let (ib, cb) = if self.unicode {
                    (u64le(entries, o + 8), u16le(entries, o + 16) as usize)
                } else {
                    (u32le(entries, o + 4) as u64, u16le(entries, o + 8) as usize)
                };
                if cb == 0 || cb > MAX_BLOCK_CB {
                    return Err(PstErr::Corrupt("bad block size"));
                }
                return Ok((ib, cb));
            }
        }
        Err(PstErr::NotFound("block"))
    }

    fn block(&self, bid: u64) -> PstResult<&'a [u8]> {
        let (ib, cb) = self.block_ref(bid)?;
        let span = (cb + self.block_trailer()).div_ceil(64) * 64;
        let ib = ib as usize;
        if ib + span > self.data.len() {
            return Err(PstErr::Corrupt("block out of range"));
        }
        Ok(&self.data[ib..ib + cb])
    }

    fn block_decoded(&self, bid: u64) -> PstResult<Vec<u8>> {
        let raw = self.block(bid)?;
        if bid & 0x2 != 0 {
            // Internal blocks are never encoded.
            return Ok(raw.to_vec());
        }
        let mut out = raw.to_vec();
        match self.crypt {
            0 => {}
            1 => {
                let table = &KEY_DATA[512..];
                for b in out.iter_mut() {
                    *b = table[*b as usize];
                }
            }
            2 => {
                let key32 = (bid & !1) as u32;
                let mut key = (key32 ^ (key32 >> 16)) as u16;
                let (r, s, it) = (&KEY_DATA[..256], &KEY_DATA[256..512], &KEY_DATA[512..]);
                for b in out.iter_mut() {
                    let low = key as u8;
                    let high = (key >> 8) as u8;
                    *b = b.wrapping_add(low);
                    *b = r[*b as usize];
                    *b = b.wrapping_add(high);
                    *b = s[*b as usize];
                    *b = b.wrapping_sub(high);
                    *b = it[*b as usize];
                    *b = b.wrapping_sub(low);
                    key = key.wrapping_add(1);
                }
            }
            _ => unreachable!(),
        }
        Ok(out)
    }

    fn data_tree(&self, bid: u64, depth: u32) -> PstResult<Vec<Vec<u8>>> {
        if bid == 0 {
            return Ok(Vec::new());
        }
        if depth > 4 {
            return Err(PstErr::Corrupt("data tree too deep"));
        }
        let block = self.block_decoded(bid)?;
        if bid & 0x2 != 0 {
            if block.len() < 8 || block[0] != 0x01 {
                return Err(PstErr::Corrupt("bad internal block type"));
            }
            let count = u16le(&block, 2) as usize;
            let step = if self.unicode { 8 } else { 4 };
            if 8 + count * step > block.len() {
                return Err(PstErr::Corrupt("internal block entries"));
            }
            let mut out = Vec::new();
            for i in 0..count {
                let child = if self.unicode {
                    u64le(&block, 8 + i * step)
                } else {
                    u32le(&block, 8 + i * step) as u64
                };
                out.extend(self.data_tree(child, depth + 1)?);
            }
            Ok(out)
        } else {
            Ok(vec![block])
        }
    }

    fn subnodes(&self, bid: u64, depth: u32) -> PstResult<Vec<(u32, u64, u64)>> {
        if bid == 0 {
            return Ok(Vec::new());
        }
        if depth > 4 {
            return Err(PstErr::Corrupt("subnode tree too deep"));
        }
        let block = self.block_decoded(bid)?;
        if block.len() < 4 || block[0] != 0x02 {
            return Err(PstErr::Corrupt("bad subnode block type"));
        }
        let level = block[1];
        let count = u16le(&block, 2) as usize;
        let header = if self.unicode { 8 } else { 4 };
        if level == 0 {
            let step = if self.unicode { 24 } else { 12 };
            if header + count * step > block.len() {
                return Err(PstErr::Corrupt("subnode leaf entries"));
            }
            let mut out = Vec::with_capacity(count);
            for i in 0..count {
                let o = header + i * step;
                let (n, bd, bs) = if self.unicode {
                    (
                        u64le(&block, o) as u32,
                        u64le(&block, o + 8),
                        u64le(&block, o + 16),
                    )
                } else {
                    (
                        u32le(&block, o),
                        u32le(&block, o + 4) as u64,
                        u32le(&block, o + 8) as u64,
                    )
                };
                out.push((n, bd, bs));
            }
            Ok(out)
        } else {
            let step = if self.unicode { 16 } else { 8 };
            if header + count * step > block.len() {
                return Err(PstErr::Corrupt("subnode intermediate entries"));
            }
            let mut out = Vec::new();
            for i in 0..count {
                let o = header + i * step;
                let child = if self.unicode {
                    u64le(&block, o + 8)
                } else {
                    u32le(&block, o + 4) as u64
                };
                out.extend(self.subnodes(child, depth + 1)?);
            }
            Ok(out)
        }
    }

    fn heap(&self, rec: &NodeRec) -> PstResult<Heap> {
        Ok(Heap {
            blocks: self.data_tree(rec.bid_data, 0)?,
        })
    }

    fn subnode_map(&self, rec: &NodeRec) -> PstResult<Vec<(u32, u64, u64)>> {
        if rec.bid_sub == 0 {
            return Ok(Vec::new());
        }
        self.subnodes(rec.bid_sub, 0)
    }

    fn bth_entries(&self, heap: &Heap, hid_user_root: u32) -> PstResult<Vec<(u64, Vec<u8>)>> {
        let header = heap
            .item(hid_user_root)
            .ok_or(PstErr::Corrupt("BTH header missing"))?;
        if header.len() < 8 || header[0] != 0xB5 {
            return Err(PstErr::Corrupt("bad BTH header"));
        }
        let cb_key = header[1] as usize;
        let cb_ent = header[2] as usize;
        if cb_key == 0 || cb_key > 8 || cb_ent == 0 {
            return Err(PstErr::Corrupt("bad BTH sizes"));
        }
        let read_key = |page: &[u8], o: usize| -> PstResult<u64> {
            match cb_key {
                2 => Ok(u16le(page, o) as u64),
                4 => Ok(u32le(page, o) as u64),
                8 => Ok(u64le(page, o)),
                _ => Err(PstErr::Corrupt("BTH key size")),
            }
        };
        let mut level = header[3];
        let mut pages = vec![u32le(header, 4)];
        while level > 0 {
            let mut next = Vec::new();
            for hid in pages {
                let page = heap.item(hid).ok_or(PstErr::Corrupt("BTH page missing"))?;
                let step = cb_key + 4;
                for i in 0..page.len() / step {
                    next.push(u32le(page, i * step + cb_key));
                }
            }
            pages = next;
            level -= 1;
        }
        let mut out = Vec::new();
        let step = cb_key + cb_ent;
        for hid in pages {
            let page = heap.item(hid).ok_or(PstErr::Corrupt("BTH leaf missing"))?;
            for i in 0..page.len() / step {
                let o = i * step;
                out.push((read_key(page, o)?, page[o + cb_key..o + step].to_vec()));
            }
        }
        Ok(out)
    }

    fn pc_records(&self, heap: &Heap) -> PstResult<Vec<(u16, u16, u32)>> {
        if heap.client_sig() != Some(0xBC) {
            return Err(PstErr::Corrupt("not a property context"));
        }
        let root_hid = heap.user_root().ok_or(PstErr::Corrupt("PC root HID"))?;
        let mut out = Vec::new();
        for (key, data) in self.bth_entries(heap, root_hid)? {
            if data.len() < 6 {
                continue;
            }
            out.push((key as u16, u16le(&data, 0), u32le(&data, 2)));
        }
        Ok(out)
    }

    fn resolve_value(&self, rec: &NodeRec, heap: &Heap, ptype: u16, value: u32) -> PstResult<Prop> {
        if value == 0 {
            return Ok(Prop::Other);
        }
        let eight_fixed = matches!(
            ptype,
            0x0014 | 0x0040 | 0x0005 | 0x0006 | 0x0007 | 0x0048 | 0x000D
        );
        let variable = matches!(ptype, 0x001E | 0x001F | 0x0102) || (ptype & 0x1000) != 0;
        if variable {
            if value & 0x1F == 0 {
                let item = heap
                    .item(value)
                    .ok_or(PstErr::Corrupt("PC heap value missing"))?;
                Ok(decode_value(item, ptype))
            } else {
                let subs = self.subnode_map(rec)?;
                let (_, bd, _) = subs
                    .iter()
                    .find(|(n, _, _)| *n == value)
                    .ok_or(PstErr::NotFound("PC subnode value"))?;
                let mut data = Vec::new();
                for b in self.data_tree(*bd, 0)? {
                    data.extend_from_slice(&b);
                }
                Ok(decode_value(&data, ptype))
            }
        } else if eight_fixed {
            let item = heap
                .item(value)
                .ok_or(PstErr::Corrupt("PC heap value missing"))?;
            Ok(decode_value(item, ptype))
        } else {
            match ptype {
                0x0003 | 0x000A => Ok(Prop::I32(value as i32)),
                0x000B => Ok(Prop::Bool(value & 0xFF != 0)),
                _ => Ok(Prop::Other),
            }
        }
    }

    fn read_pc_rec(&self, rec: &NodeRec) -> PstResult<Vec<(u16, Prop)>> {
        let heap = self.heap(rec)?;
        let mut out = Vec::new();
        for (id, ptype, value) in self.pc_records(&heap)? {
            out.push((id, self.resolve_value(rec, &heap, ptype, value)?));
        }
        Ok(out)
    }

    fn read_table_row_ids(&self, table_rec: &NodeRec) -> PstResult<Vec<u32>> {
        let heap = self.heap(table_rec)?;
        let root_hid = heap.user_root().ok_or(PstErr::Corrupt("TC root HID"))?;
        let info = heap.item(root_hid).ok_or(PstErr::Corrupt("TCINFO"))?;
        if info.len() < 22 || info[0] != 0x7C {
            return Err(PstErr::Corrupt("not a table context"));
        }
        let end_ceb = u16le(info, 8) as usize;
        if !(9..=0x1000).contains(&end_ceb) {
            return Err(PstErr::Corrupt("bad TC row size"));
        }
        let hnid_rows = u32le(info, 14);
        if hnid_rows == 0 {
            return Ok(Vec::new());
        }
        let matrix: Vec<u8> = if hnid_rows & 0x1F == 0 {
            heap.item(hnid_rows)
                .ok_or(PstErr::Corrupt("TC rows heap item"))?
                .to_vec()
        } else {
            let subs = self.subnode_map(table_rec)?;
            let (_, bd, _) = subs
                .iter()
                .find(|(n, _, _)| *n == hnid_rows)
                .ok_or(PstErr::NotFound("TC rows subnode"))?;
            let mut data = Vec::new();
            for b in self.data_tree(*bd, 0)? {
                data.extend_from_slice(&b);
            }
            data
        };
        if !matrix.len().is_multiple_of(end_ceb) {
            return Err(PstErr::Corrupt("TC rows size mismatch"));
        }
        Ok((0..matrix.len() / end_ceb)
            .map(|i| u32le(&matrix, i * end_ceb))
            .collect())
    }

    fn folder_text(
        &self,
        nid: u32,
        depth: u32,
        seen: &mut Vec<u32>,
        out: &mut String,
    ) -> PstResult<()> {
        if depth > 64 || seen.contains(&nid) {
            return Err(PstErr::Corrupt("folder cycle"));
        }
        seen.push(nid);
        let rec = self.node(nid)?;
        let props = self.read_pc_rec(&rec)?;
        let name = props
            .iter()
            .find(|(id, _)| *id == PROP_DISPLAY_NAME)
            .and_then(|(_, v)| v.text())
            .unwrap_or_default();
        let _ = writeln!(out, "{}[folder] {}", "  ".repeat(depth as usize), name);
        let mut content_tables = Vec::new();
        for (snid, sbid, sbs) in self.subnode_map(&rec)? {
            let table_rec = NodeRec {
                bid_data: sbid,
                bid_sub: sbs,
            };
            match snid & 0x1F {
                NID_TYPE_HIERARCHY_TABLE => {
                    if let Ok(child_nids) = self.read_table_row_ids(&table_rec) {
                        for child_nid in child_nids {
                            if child_nid & 0x1F == NID_TYPE_NORMAL_FOLDER
                                || child_nid & 0x1F == NID_TYPE_SEARCH_FOLDER
                            {
                                let _ = self.folder_text(child_nid, depth + 1, seen, out);
                            }
                        }
                    }
                }
                NID_TYPE_CONTENTS_TABLE => content_tables.push(table_rec),
                _ => {}
            }
        }
        for table in content_tables {
            if let Ok(msg_nids) = self.read_table_row_ids(&table) {
                for mnid in msg_nids {
                    if let Ok(text) = self.message_text(mnid) {
                        out.push_str(&text);
                    }
                }
            }
        }
        Ok(())
    }

    fn message_text(&self, nid: u32) -> PstResult<String> {
        let rec = self.node(nid)?;
        let props = self.read_pc_rec(&rec)?;
        let text = |id: u16| -> String {
            props
                .iter()
                .find(|(pid, _)| *pid == id)
                .and_then(|(_, v)| v.text())
                .unwrap_or_default()
        };
        let date = [PROP_DELIVERY_TIME, PROP_CLIENT_SUBMIT_TIME]
            .iter()
            .find_map(|id| match props.iter().find(|(pid, _)| pid == id) {
                Some((_, Prop::Time(t))) => Some(*t),
                _ => None,
            });
        let has_attach = matches!(
            props.iter().find(|(pid, _)| *pid == PROP_HAS_ATTACH),
            Some((_, Prop::Bool(true)))
        );
        // Prefer a *string* body. A PR_BODY holding an integer or binary
        // value now renders through `Prop::text()` too, so picking "the first
        // property that returns Some" would let a transport-header blob
        // shadow a real PR_HTML body. Fall through to HTML whenever PR_BODY
        // is not a string, not merely when it is empty.
        let mut body = props
            .iter()
            .find(|(pid, _)| *pid == PROP_BODY)
            .and_then(|(_, v)| match v {
                Prop::Str(s) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_default();
        if body.trim().is_empty() {
            body = text(PROP_BODY_HTML);
        }
        let mut out = String::new();
        let _ = writeln!(out, "subject: {}", text(PROP_SUBJECT));
        let sender_name = text(PROP_SENDER_NAME);
        let sender_email = text(PROP_SENDER_EMAIL);
        let from = if sender_name.is_empty() {
            sender_email
        } else if sender_email.is_empty() || sender_name == sender_email {
            sender_name
        } else {
            format!("{sender_name} <{sender_email}>")
        };
        let _ = writeln!(out, "from: {from}");
        let _ = writeln!(out, "to: {}", text(PROP_DISPLAY_TO));
        if let Some(t) = date {
            let _ = writeln!(out, "date: {t}");
        }
        if has_attach {
            out.push_str("attachments: yes\n");
        }
        if !body.trim().is_empty() {
            out.push_str(&body);
            out.push('\n');
        }
        Ok(out)
    }
}

fn decode_utf16_until_nul(data: &[u8]) -> String {
    let mut units = Vec::with_capacity(data.len() / 2);
    let mut p = 0;
    while p + 2 <= data.len() {
        let u = u16le(data, p);
        if u == 0 {
            break;
        }
        units.push(u);
        p += 2;
    }
    String::from_utf16_lossy(&units)
}

fn decode_value(data: &[u8], ptype: u16) -> Prop {
    match ptype {
        PT_UNICODE => Prop::Str(decode_utf16_until_nul(data)),
        0x001E => {
            let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
            Prop::Str(String::from_utf8_lossy(&data[..end]).into_owned())
        }
        0x0102 => Prop::Bin(data.to_vec()),
        PT_I32 | 0x000A => Prop::I32(if data.len() >= 4 {
            u32le(data, 0) as i32
        } else {
            0
        }),
        PT_BOOL => Prop::Bool(data.first().copied().unwrap_or(0) != 0),
        PT_TIME => Prop::Time(if data.len() >= 8 { u64le(data, 0) } else { 0 }),
        _ => Prop::Other,
    }
}

/// Outlook `.pst` / `.ost`: the full mailbox becomes one searchable text
/// document (folder tree, per-message headers, bodies).
pub struct PstAdapter;

impl Adapter for PstAdapter {
    fn name(&self) -> &'static str {
        "pst"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["pst", "ost"]
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        let pst = Pst::parse(data).map_err(|e| rgx_core::anyhow!(e))?;
        let mut out = String::new();
        pst.folder_text(NID_ROOT_FOLDER, 0, &mut Vec::new(), &mut out)
            .map_err(|e| rgx_core::anyhow!(e))?;
        if out.trim().is_empty() {
            bail!("no folders or messages found in PST/OST");
        }
        Ok(Outcome::Text(out.into_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bid(index: u64, internal: bool) -> u64 {
        (index << 2) | if internal { 2 } else { 0 }
    }
    fn nid(index: u32, ty: u32) -> u32 {
        (index << 5) | ty
    }
    fn hid(block_index: u32, index_1based: u32) -> u32 {
        (block_index << 16) | (index_1based << 5)
    }

    fn block_image(content: &[u8], index: u64, internal: bool) -> Vec<u8> {
        let mut v = content.to_vec();
        v.extend_from_slice(&(content.len() as u16).to_le_bytes());
        v.extend_from_slice(&0u16.to_le_bytes());
        v.extend_from_slice(&0u32.to_le_bytes());
        v.extend_from_slice(&bid(index, internal).to_le_bytes());
        while !v.len().is_multiple_of(64) {
            v.push(0);
        }
        v
    }

    fn heap_block(items: &[Vec<u8>], client_sig: u8, user_root: u32) -> Vec<u8> {
        let mut v = Vec::new();
        let mut offsets = Vec::new();
        v.extend_from_slice(&0u16.to_le_bytes());
        v.push(0xEC);
        v.push(client_sig);
        v.extend_from_slice(&user_root.to_le_bytes());
        v.extend_from_slice(&0u32.to_le_bytes());
        for item in items {
            offsets.push(v.len() as u16);
            v.extend_from_slice(item);
        }
        offsets.push(v.len() as u16);
        let ib_hnpm = v.len() as u16;
        v[0..2].copy_from_slice(&ib_hnpm.to_le_bytes());
        v.extend_from_slice(&(items.len() as u16).to_le_bytes());
        v.extend_from_slice(&0u16.to_le_bytes());
        for o in offsets {
            v.extend_from_slice(&o.to_le_bytes());
        }
        v
    }

    fn bth_header(cb_key: u8, cb_ent: u8, levels: u8, root: u32) -> Vec<u8> {
        let mut v = vec![0xB5, cb_key, cb_ent, levels];
        v.extend_from_slice(&root.to_le_bytes());
        v
    }

    fn pc_leaf_record(prop_id: u16, ptype: u16, value: u32) -> Vec<u8> {
        let mut v = prop_id.to_le_bytes().to_vec();
        v.extend_from_slice(&ptype.to_le_bytes());
        v.extend_from_slice(&value.to_le_bytes());
        v
    }

    fn utf16z(s: &str) -> Vec<u8> {
        let mut v = Vec::new();
        for u in s.encode_utf16() {
            v.extend_from_slice(&u.to_le_bytes());
        }
        v.extend_from_slice(&0u16.to_le_bytes());
        v
    }

    fn tcinfo_one_col(rows_hid: u32) -> Vec<u8> {
        let mut t = vec![0x7C, 0x01];
        t.extend_from_slice(&8u16.to_le_bytes());
        t.extend_from_slice(&8u16.to_le_bytes());
        t.extend_from_slice(&8u16.to_le_bytes());
        t.extend_from_slice(&9u16.to_le_bytes());
        t.extend_from_slice(&0u32.to_le_bytes());
        t.extend_from_slice(&rows_hid.to_le_bytes());
        t.extend_from_slice(&0u32.to_le_bytes());
        t.extend_from_slice(&PT_I32.to_le_bytes());
        t.extend_from_slice(&0x67F2u16.to_le_bytes());
        t.extend_from_slice(&0u16.to_le_bytes());
        t.push(4);
        t.push(0);
        t
    }

    fn build_fixture(crypt: u8) -> Vec<u8> {
        let root_folder_pc = heap_block(
            &[
                bth_header(2, 6, 0, hid(0, 2)),
                pc_leaf_record(PROP_DISPLAY_NAME, PT_UNICODE, hid(0, 3)),
                utf16z("Root Mailbox"),
            ],
            0xBC,
            hid(0, 1),
        );
        let mut records = Vec::new();
        for r in [
            pc_leaf_record(PROP_SUBJECT, PT_UNICODE, hid(0, 3)),
            pc_leaf_record(PROP_SENDER_EMAIL, PT_UNICODE, hid(0, 4)),
            pc_leaf_record(PROP_HAS_ATTACH, PT_BOOL, 1),
            pc_leaf_record(PROP_BODY, PT_UNICODE, hid(0, 5)),
        ] {
            records.extend_from_slice(&r);
        }
        let message_pc = heap_block(
            &[
                bth_header(2, 6, 0, hid(0, 2)),
                records,
                utf16z("Quarterly report"),
                utf16z("alice@example.com"),
                utf16z("Hello Bob, the report is attached."),
            ],
            0xBC,
            hid(0, 1),
        );
        let mut row = nid(8, 0x04).to_le_bytes().to_vec();
        row.extend_from_slice(&[0; 4]);
        row.push(0x80);
        let contents_tc = heap_block(&[tcinfo_one_col(hid(0, 2)), row], 0x7C, hid(0, 1));

        let ctc_nid = nid(5, NID_TYPE_CONTENTS_TABLE);
        let mut sl = vec![0x02u8, 0x00, 0x01, 0x00];
        sl.extend_from_slice(&0u32.to_le_bytes());
        sl.extend_from_slice(&(ctc_nid as u64).to_le_bytes());
        sl.extend_from_slice(&bid(8, false).to_le_bytes());
        sl.extend_from_slice(&0u64.to_le_bytes());

        let mut ext_blocks: Vec<(Vec<u8>, bool)> = vec![
            (root_folder_pc, false),
            (sl, true),
            (message_pc, false),
            (contents_tc, false),
        ];
        if crypt == 1 {
            let table = &KEY_DATA[..256];
            for (b, internal) in ext_blocks.iter_mut() {
                if *internal {
                    continue;
                }
                for x in b.iter_mut() {
                    *x = table[*x as usize];
                }
            }
        }

        // Blocks are 64-aligned; reserve 256 bytes per block slot.
        let block_offsets = [2048usize, 2304, 2560, 2816];
        let mut data = vec![0u8; 4096];
        data[0..4].copy_from_slice(&0x2142_444Eu32.to_le_bytes());
        data[8..10].copy_from_slice(&0x4D53u16.to_le_bytes());
        data[10..12].copy_from_slice(&23u16.to_le_bytes());
        data[12..14].copy_from_slice(&19u16.to_le_bytes());
        data[14] = 1;
        data[15] = 1;
        data[512] = 0x80;
        data[513] = crypt;
        let file_size = data.len() as u64;
        data[180..188].copy_from_slice(&file_size.to_le_bytes());
        data[212..220].copy_from_slice(&1024u64.to_le_bytes());
        data[220..228].copy_from_slice(&bid(1, false).to_le_bytes());
        data[228..236].copy_from_slice(&1536u64.to_le_bytes());
        data[236..244].copy_from_slice(&bid(2, false).to_le_bytes());

        let mut page = vec![0u8; PAGE_SIZE];
        page[0] = 2;
        page[1] = 15;
        page[2] = 32;
        page[3] = 0;
        for (i, (n, bd, bs)) in [
            (NID_ROOT_FOLDER, bid(4, false), bid(5, true)),
            (nid(8, 0x04), bid(6, false), 0u64),
        ]
        .into_iter()
        .enumerate()
        {
            let o = 8 + i * 32;
            page[o..o + 8].copy_from_slice(&(n as u64).to_le_bytes());
            page[o + 8..o + 16].copy_from_slice(&bd.to_le_bytes());
            page[o + 16..o + 24].copy_from_slice(&bs.to_le_bytes());
        }
        page[496] = 0x81;
        page[497] = 0x81;
        page[504..512].copy_from_slice(&bid(1, false).to_le_bytes());
        data[1024..1536].copy_from_slice(&page);

        let mut page = vec![0u8; PAGE_SIZE];
        page[0] = 4;
        page[1] = 20;
        page[2] = 24;
        page[3] = 0;
        for (i, (((idx, internal), (content, _)), off)) in
            [(4u64, false), (5, true), (6, false), (8, false)]
                .into_iter()
                .zip(ext_blocks.iter())
                .zip(block_offsets)
                .enumerate()
        {
            let o = 8 + i * 24;
            page[o..o + 8].copy_from_slice(&bid(idx, internal).to_le_bytes());
            page[o + 8..o + 16].copy_from_slice(&(off as u64).to_le_bytes());
            page[o + 16..o + 18].copy_from_slice(&(content.len() as u16).to_le_bytes());
            page[o + 18..o + 20].copy_from_slice(&1u16.to_le_bytes());
        }
        page[496] = 0x80;
        page[497] = 0x80;
        page[504..512].copy_from_slice(&bid(2, false).to_le_bytes());
        data[1536..2048].copy_from_slice(&page);

        for (i, ((content, internal), off)) in ext_blocks.iter().zip(block_offsets).enumerate() {
            let img = block_image(content, 4 + i as u64, *internal);
            data[off..off + img.len()].copy_from_slice(&img);
        }
        data
    }

    #[test]
    fn test_pst_adapter_plain() {
        let data = build_fixture(0);
        let out = PstAdapter.extract(&data, "mail.pst").unwrap();
        let Outcome::Text(t) = out else {
            panic!("text")
        };
        let s = String::from_utf8_lossy(&t);
        assert!(s.contains("[folder] Root Mailbox"), "{s}");
        assert!(s.contains("subject: Quarterly report"), "{s}");
        assert!(s.contains("from: alice@example.com"), "{s}");
        assert!(s.contains("Hello Bob, the report is attached."), "{s}");
    }

    #[test]
    fn test_pst_adapter_permute() {
        let data = build_fixture(1);
        let out = PstAdapter.extract(&data, "cache.ost").unwrap();
        let Outcome::Text(t) = out else {
            panic!("text")
        };
        let s = String::from_utf8_lossy(&t);
        assert!(s.contains("subject: Quarterly report"), "{s}");
    }

    #[test]
    fn test_pst_adapter_rejects_garbage() {
        assert!(PstAdapter.extract(&[0u8; 4096], "x.pst").is_err());
    }
}
