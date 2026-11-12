// SPDX-License-Identifier: Apache-2.0

//! Real text: the launcher's glyphs, rasterised by FreeType at runtime.
//!
//! The window used to spell itself with an 8x8 bitmap font carried in the
//! source — honest, legible, and not typography: one size, no antialiasing,
//! no real letter shapes. The text is rendered by **FreeType** now, which is
//! what every desktop uses, and the crate still links nothing: the library is
//! opened with `dlopen` on the first call (`libfreetype.so.6`, present on
//! every desktop Linux and installed from `print/freetype2` on FreeBSD) and
//! the seven functions the launcher makes are declared by hand, exactly as the
//! Xlib seam in `crate::x` declares its own.
//!
//! # The fallback is the whole design
//!
//! A machine with no font file is a machine with no text, and a window that
//! draws no text is not a launcher. So [`Font::load`] answers `None` for every
//! reason there is — no library, no symbol, no readable file, a struct layout
//! this build does not understand — and the caller keeps the built-in bitmap
//! font it had before. The worst answer any of this can give is the font the
//! launcher used to have; nothing here can take it down.
//!
//! # Reading a struct by offset
//!
//! FreeType's public structs are a stable ABI, but they are structs, and
//! reading one means naming byte offsets. Every offset used here is written
//! down once, in `rec`, with the C field it belongs to and the walk that
//! produced it, and every read goes through one guarded reader: a face whose
//! numbers cannot be a font's, or a glyph slot whose bitmap cannot be a
//! character's, makes the load fail closed rather than return a slice of
//! somebody else's memory. The guard is the reason a layout that drifts costs
//! a fallback instead of a frame.
//!
//! The numbers are the LP64 layout, the only one this crate targets: `long` is
//! 8 bytes, a pointer 8, an `int` and an `unsigned int` 4, a `short` 2. They
//! are not 32-bit numbers and this module does not pretend they are.

use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::{c_char, c_int, c_long, c_short, c_uint, c_ulong, c_ushort, c_void};
use core::ptr;
use core::sync::atomic::{AtomicI8, Ordering};

use crate::x::{dlopen, dlsym, getenv, strlen};

/// The pixel size the launcher's text is set at: thirteen pixels.
///
/// The layout this draws into has a 59-pixel field and 34-pixel rows, and 13
/// pixels of DejaVu — ascender 1901 units of 2048, descender 483 — is a line of
/// about 15 pixels, which is the largest size that still leaves the row its own
/// air above and below. At one pixel per layout unit it is also the size at
/// which DejaVu's lowercase stays crisp on a 1:1 X server.
pub const SIZE: u32 = 13;

/// The pixel size of the small line under a name: eleven pixels.
///
/// One step down from [`SIZE`], so the name reads first and the line under it
/// is a whisper rather than a second heading. Eleven keeps a descender and a
/// two-pixel stem, which is what a line that small needs to stay readable.
pub const SMALL_SIZE: u32 = 11;

/// The font files the loader tries, in a fixed order, the first hit winning.
///
/// The order is deterministic on purpose: two machines with the same fonts load
/// the same face, and a machine with different ones loads the first face it
/// has. DejaVu comes first because it is the desktop default on Linux and
/// FreeBSD and the face the launcher was measured with; the two DejaVu
/// directories FreeBSD's ports tree and a hand-installed tree use follow; then
/// Liberation, Debian's metric twin of Arial, and Noto, which is what a Fedora
/// install carries instead.
pub fn candidate_paths() -> &'static [&'static str] {
    &[
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/local/share/fonts/dejavu/DejaVuSans.ttf",
        "/usr/local/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
    ]
}

/// The bold faces, in the same directories and the same order.
///
/// [`load_bold`] walks this list and then falls back to the regular search, so
/// a machine that has no bold file still gets text.
pub fn bold_candidate_paths() -> &'static [&'static str] {
    &[
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans-Bold.ttf",
        "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
        "/usr/local/share/fonts/dejavu/DejaVuSans-Bold.ttf",
        "/usr/local/share/fonts/TTF/DejaVuSans-Bold.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf",
    ]
}

/// The environment variable that names the font file to load.
const RGY_FONT: &[u8] = b"RGY_FONT\0";
/// The long spelling of `RGY_FONT`, asked second.
const RGY_FONT_PATH: &[u8] = b"RGY_FONT_PATH\0";

/// The font file the environment names, if it names one.
///
/// `RGY_FONT` is asked first and `RGY_FONT_PATH` second, and the first
/// non-empty answer wins: the short name is the one to document, and the long
/// one is kept because the launcher's other path settings spell out what they
/// name. An empty value is the same as no value, which is what a shell does
/// with `RGY_FONT=`.
///
/// A value that is not UTF-8 is treated as no value: the launcher takes a path
/// as text everywhere else, and a font file named by bytes this module cannot
/// spell is a name it cannot honestly hand on.
pub fn font_from_env() -> Option<String> {
    for name in [RGY_FONT, RGY_FONT_PATH] {
        // SAFETY: `name` is a NUL-terminated literal, and `getenv` only reads
        // the environment; the bytes are copied out before this returns.
        let raw = unsafe { getenv(name.as_ptr().cast()) };
        if raw.is_null() {
            continue;
        }
        // SAFETY: `getenv` returns a NUL-terminated string owned by libc, and
        // `strlen` says where it ends.
        let len = unsafe { strlen(raw) };
        if len == 0 {
            continue;
        }
        // SAFETY: the pointer and the length name the same string, which lives
        // in libc's environment for as long as nothing rewrites it.
        let bytes = unsafe { core::slice::from_raw_parts(raw.cast::<u8>(), len) };
        if let Ok(text) = core::str::from_utf8(bytes) {
            return Some(String::from(text));
        }
    }
    None
}

/// A loaded face, and the pixel size it is set at.
///
/// The face owns everything FreeType handed back: the library instance it was
/// opened from, the face itself and the rasteriser's scratch space. Dropping
/// the `Font` gives all of it back. One face per `Font` means a caller can hold
/// a regular face, a bold one and a small one at once without the three
/// overwriting each other's bitmaps.
///
/// It is not `Send` and not `Sync`: a FreeType face may be used by one thread
/// at a time, and the raw handles say so without a word of comment.
pub struct Font {
    /// The `FT_Library` the face was opened from.
    library: *mut c_void,
    /// The `FT_Face` itself.
    face: *mut c_void,
    /// The pixel size the face is set at.
    size: u32,
}

/// One rasterised character: its coverage, where to put it, how far it moves
/// the pen.
///
/// `coverage` is one byte per pixel — `0` for a pixel the glyph does not touch,
/// `255` for one it covers completely, and the antialiased edge in between.
/// Rows are `pitch` bytes apart, the first row is the top of the glyph, and the
/// pixel at `(x, y)` is `coverage[y * pitch + x]`; `width` is how many bytes of
/// each row are the glyph's, and the rest is padding.
///
/// The slice is FreeType's own memory, and it stays valid only until the next
/// glyph is loaded from the same face. The `&mut Font` borrow that produced it
/// is what enforces that: the borrow checker will not let a second
/// [`Font::glyph`] call exist while a `Glyph` from the first is alive, so the
/// bitmap cannot change under a caller that is blending it.
///
/// The positions are relative to the pen, in whole pixels: `left` is where the
/// bitmap's left edge goes, `top` is how far above the baseline its top edge
/// is, and `advance` is how far the pen moves afterwards.
pub struct Glyph<'a> {
    /// The coverage map, `height * pitch` bytes, top row first.
    pub coverage: &'a [u8],
    /// The width of the glyph in pixels.
    pub width: u32,
    /// The height of the glyph in pixels.
    pub height: u32,
    /// The row stride of `coverage` in bytes, always positive.
    pub pitch: u32,
    /// The horizontal bearing: the bitmap's left edge, from the pen.
    pub left: i32,
    /// The vertical bearing: the bitmap's top edge, above the baseline.
    pub top: i32,
    /// How far the pen moves after this glyph, in whole pixels.
    pub advance: i32,
}

/// Load a bold face, falling back to the regular search.
///
/// [`bold_candidate_paths`] first, then [`Font::load`]: the launcher sets the
/// query and the names in the strong face, and a machine that has no bold file
/// still deserves text. `RGY_FONT` is consulted by the fallback only — a file
/// named by the environment is a file the user chose, and substituting it for
/// the bold face would be choosing in their place.
pub fn load_bold() -> Option<Font> {
    for path in bold_candidate_paths() {
        if let Some(font) = Font::from_path(path) {
            return Some(font);
        }
    }
    Font::load()
}

impl Font {
    /// Load the first face that answers, at [`SIZE`].
    ///
    /// [`font_from_env`] is asked first — a user who names a file means it —
    /// and then [`candidate_paths`] is walked in its fixed order. `None` means
    /// no text: no library, no readable font file, or a face whose own numbers
    /// did not survive the guards, and the caller draws with the built-in
    /// bitmap font instead.
    pub fn load() -> Option<Font> {
        if let Some(font) = font_from_env().and_then(|path| Font::from_path(&path)) {
            return Some(font);
        }
        for path in candidate_paths() {
            if let Some(font) = Font::from_path(path) {
                return Some(font);
            }
        }
        None
    }

    /// Load the bold face, or the regular one standing in for it.
    ///
    /// The associated form of [`load_bold`], for the callers that reach the
    /// faces through the type.
    pub fn load_bold() -> Option<Font> {
        crate::font::load_bold()
    }

    /// Open one font file at [`SIZE`].
    ///
    /// `None` means the file is not there, is not a font, or produced a face
    /// this build does not trust: FreeType's structs are read by offset here,
    /// and the guards in `face_is_a_font` and `glyph_slot_is_a_font` are what
    /// turn an offset this build got wrong into a fallback instead of a slice
    /// of the wrong memory.
    pub fn from_path(path: &str) -> Option<Font> {
        Font::from_path_size(path, SIZE)
    }

    /// Open one font file at a pixel size of the caller's choosing.
    ///
    /// As [`Font::from_path`], with `size` as the height of an em. A size of
    /// zero is refused: `FT_Set_Pixel_Sizes` takes a pixel height, and a font
    /// set at nothing is a font that draws nothing.
    ///
    /// The face is proved at load time, not at the first character: the probe
    /// in `glyph_slot_is_a_font` loads one letter and reads the slot back
    /// through the same guard a glyph read uses, so a layout this build got
    /// wrong costs the font and not the frame.
    pub fn from_path_size(path: &str, size: u32) -> Option<Font> {
        if size == 0 {
            return None;
        }
        let file = c_path(path)?;

        let mut library: *mut c_void = ptr::null_mut();
        // SAFETY: `library` is a local out-parameter, which is what
        // FT_Init_FreeType takes.
        let error = unsafe { FT_Init_FreeType(&mut library) };
        if error != 0 || library.is_null() {
            return None;
        }

        let mut face: *mut c_void = ptr::null_mut();
        // SAFETY: the library is live, `file` is a NUL-terminated path that
        // outlives the call, and `face` is the out-parameter FT_New_Face fills.
        let error = unsafe { FT_New_Face(library, file.as_ptr().cast(), 0, &mut face) };
        if error != 0 || face.is_null() {
            // SAFETY: the library is live, and no face came out of it to give
            // back.
            unsafe {
                FT_Done_FreeType(library);
            }
            return None;
        }

        // SAFETY: `face` is a live face, which is what the guard reads.
        if !unsafe { face_is_a_font(face) } {
            // SAFETY: both handles are live and are given back here, once.
            unsafe {
                FT_Done_Face(face);
                FT_Done_FreeType(library);
            }
            return None;
        }

        // The Unicode charmap, so that a character is a code point rather than
        // a byte of a legacy 8-bit encoding. A refusal is not fatal: a face may
        // carry exactly one charmap and it may already be Unicode, which is
        // DejaVu's default, so asking is only worth it for the faces that
        // carry more than one.
        // SAFETY: `face` is live.
        let _ = unsafe { FT_Select_Charmap(face, UNICODE) };

        // SAFETY: `face` is live and has no pixel size yet.
        let error = unsafe { FT_Set_Pixel_Sizes(face, 0, size) };
        if error != 0 {
            // SAFETY: both handles are live and are given back here, once.
            unsafe {
                FT_Done_Face(face);
                FT_Done_FreeType(library);
            }
            return None;
        }

        // SAFETY: the face is live, its charmap is chosen and its pixel size is
        // set, which is what the probe needs.
        if !unsafe { glyph_slot_is_a_font(face) } {
            // SAFETY: both handles are live and are given back here, once.
            unsafe {
                FT_Done_Face(face);
                FT_Done_FreeType(library);
            }
            return None;
        }

        Some(Font {
            library,
            face,
            size,
        })
    }

    /// The pixel size this face is set at.
    pub fn size(&self) -> u32 {
        self.size
    }

    /// Rasterise one character at the face's size.
    ///
    /// `None` means FreeType refused the character, or the slot it filled is
    /// not a bitmap this module would read — see `slot_fields`, which is the
    /// one place the offsets meet reality. A character with no ink, a space,
    /// answers `Some` with an empty `coverage` and its real `advance`, so that
    /// a drawing loop steps over it and [`Font::measure`] counts it.
    pub fn glyph(&mut self, ch: char) -> Option<Glyph<'_>> {
        // SAFETY: the face is live, and `ch as c_ulong` is the character's own
        // code point, which is what FT_Load_Char's `FT_ULong` wants.
        let error = unsafe { FT_Load_Char(self.face, ch as c_ulong, FT_LOAD_RENDER) };
        if error != 0 {
            return None;
        }
        // SAFETY: the face is live and the load above has just filled its slot.
        let slot = unsafe { slot_fields(self.face) }?;
        // `slot_fields` bounds both numbers, so the product cannot wrap.
        let len = (slot.rows as usize).saturating_mul(slot.stride as usize);
        let coverage: &[u8] = if len == 0 {
            // A blank glyph has no buffer to borrow, and a zero-length slice
            // out of a null pointer would be undefined behaviour even so.
            &[]
        } else {
            // SAFETY: the slot's bitmap owns `len` bytes and FreeType keeps it
            // alive until the next FT_Load_Char on this face, which the
            // `&mut self` borrow marks as the end of the slice's life.
            unsafe { core::slice::from_raw_parts(slot.buffer.cast::<u8>(), len) }
        };
        Some(Glyph {
            coverage,
            width: slot.width,
            height: slot.rows,
            pitch: slot.stride,
            left: slot.left,
            top: slot.top,
            advance: slot.advance,
        })
    }

    /// The width of `s` at the face's size, in whole pixels.
    ///
    /// The sum of the advances a drawing loop steps the pen by, so that a
    /// measured string and a drawn string end at the same x. A character the
    /// face does not have contributes the half em the drawing loop steps over
    /// it with, which is the same number both sides of that agreement use.
    pub fn measure(&mut self, s: &str) -> i32 {
        let mut total: i32 = 0;
        for ch in s.chars() {
            total = total.saturating_add(self.advance_of(ch));
        }
        total
    }

    /// The pen advance of one character, in whole pixels.
    ///
    /// The size is read before the face is borrowed, so the fallback for a
    /// character the face does not have costs no second borrow of the face.
    fn advance_of(&mut self, ch: char) -> i32 {
        let mut advance = (self.size / 2) as i32;
        if let Some(glyph) = self.glyph(ch) {
            advance = glyph.advance;
        }
        advance
    }
}

impl Drop for Font {
    fn drop(&mut self) {
        // SAFETY: both handles belong to this font, are live, and are given
        // back exactly once, because `Drop` runs once.
        unsafe {
            if !self.face.is_null() {
                FT_Done_Face(self.face);
            }
            if !self.library.is_null() {
                FT_Done_FreeType(self.library);
            }
        }
    }
}

/// `path` as a NUL-terminated C string, or `None` when it carries a NUL.
///
/// A path with an interior NUL is not a path: libc would see a shorter name
/// than the caller wrote and open a different file, so the honest answer is no
/// font rather than the wrong one.
fn c_path(path: &str) -> Option<Vec<u8>> {
    if path.as_bytes().contains(&0) {
        return None;
    }
    let mut bytes = Vec::with_capacity(path.len() + 1);
    bytes.extend_from_slice(path.as_bytes());
    bytes.push(0);
    Some(bytes)
}

// --- FreeType, resolved at runtime -------------------------------------------
//
// The same arrangement as the Xlib seam in `crate::x`, and for the same reason:
// the development symlink `libfreetype.so` is not on the CI runner's image and
// is not on a musl or aarch64 cross build, while the soname `libfreetype.so.6`
// is what a desktop machine that opens a font file already has. The `dl*`
// helpers come from `crate::x`, which declares them against libc: this module
// declares no second copy of libc, only FreeType.
//
// The order of `FT_SYMBOLS` is a contract: entry `i` is resolved into
// `FT_PTRS[i]`, and the `ftfn!` wrapper with index `i` is the only reader of
// that slot.

/// The runtime soname of FreeType 2; the trailing NUL makes it a C string.
const FREETYPE: &[u8] = b"libfreetype.so.6\0";
/// `RTLD_LAZY`, the same flag the Xlib seam opens its library with.
/// `RTLD_LOCAL` is 0 on Linux, so it is left out.
const RTLD_LAZY: c_int = 1;
/// The number of FreeType calls the launcher resolves.
const FT_COUNT: usize = 7;
/// The answer of a missing library or symbol.
///
/// Every FreeType call returns an `FT_Error`, and zero is success, so an absent
/// function must answer with a nonzero code: a wrapper that answered zero when
/// the library was missing would report a font that was never opened.
const ABSENT: c_int = 1;

/// The seven FreeType symbols the launcher uses, in a FIXED order: entry `i`
/// belongs to the `ftfn!` wrapper whose index is `i`. Each is a NUL-terminated
/// C string exactly matching its C symbol name.
#[rustfmt::skip]
const FT_SYMBOLS: [&[u8]; FT_COUNT] = [
    b"FT_Init_FreeType\0", // 0
    b"FT_New_Face\0", // 1
    b"FT_Set_Pixel_Sizes\0", // 2
    b"FT_Load_Char\0", // 3
    b"FT_Select_Charmap\0", // 4
    b"FT_Done_Face\0", // 5
    b"FT_Done_FreeType\0", // 6
];

/// The length of `FT_SYMBOLS`, used as the pointer table's length.
const FT: usize = FT_SYMBOLS.len();
// A missing wrapper is a compile error: the count is fixed at seven.
const _: () = assert!(FT_SYMBOLS.len() == FT_COUNT);

// `FT_PTRS` is a `static mut` for the same reason `crate::x`'s table is: this
// crate is `no_std`, and FreeType is resolved once, before any face exists.
// `STATE` gates it — only the thread that wins the 0 -> 2 compare-exchange
// writes the slots, and every reader waits for `STATE` to leave 2 before
// touching them, so there is no concurrent write and no torn read.
static mut FT_PTRS: [*mut c_void; FT] = [ptr::null_mut(); FT];
/// Load state: 0 = not tried, 2 = loading, 1 = ready, -1 = failed.
static STATE: AtomicI8 = AtomicI8::new(0);

/// Resolve every FreeType symbol once, on the first call.
fn ensure_loaded() {
    let state = STATE.load(Ordering::Acquire);
    if state == 1 || state == -1 {
        return;
    }
    if STATE
        .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        // Another thread is loading (or has already finished); wait only while
        // it is still in the loading state, then return.
        while STATE.load(Ordering::Acquire) == 2 {
            core::hint::spin_loop();
        }
        return;
    }
    // We own the 0 -> 2 transition: load every symbol, then publish.
    let ok = unsafe { load_all() };
    STATE.store(if ok { 1 } else { -1 }, Ordering::Release);
}

/// `dlopen` the runtime soname and resolve every symbol into `FT_PTRS`.
///
/// # Safety
///
/// Only the `STATE == 2` winner calls this, once, so `FT_PTRS` is written by
/// exactly one thread before `STATE` is published as ready.
unsafe fn load_all() -> bool {
    // SAFETY: `FREETYPE` and every `FT_SYMBOLS` entry are NUL-terminated C
    // strings, and `handle` is a live library handle for the whole loop.
    unsafe {
        let handle = dlopen(FREETYPE.as_ptr().cast(), RTLD_LAZY);
        if handle.is_null() {
            return false;
        }
        for (i, name) in FT_SYMBOLS.iter().enumerate() {
            FT_PTRS[i] = dlsym(handle, name.as_ptr().cast());
            if FT_PTRS[i].is_null() {
                // A symbol is missing. Leave the library loaded: the process
                // may still call a wrapper, which must answer `ABSENT` rather
                // than jump through a stale pointer into an unloaded library.
                return false;
            }
        }
    }
    // Success: deliberately do NOT `dlclose`. The pointers stay valid for the
    // process's whole life, which is exactly how long a face opened from this
    // library can live.
    true
}

/// Return the resolved pointer for symbol `i`, or null when FreeType is absent.
fn sym(i: usize) -> *mut c_void {
    ensure_loaded();
    if STATE.load(Ordering::Acquire) != 1 {
        return ptr::null_mut();
    }
    // SAFETY: `STATE == 1` means every `FT_PTRS` slot was filled by `load_all`.
    unsafe { FT_PTRS[i] }
}

// Transmuting a data pointer to a function pointer needs equal sizes; this
// holds on every target this crate is built for.
const _: () = assert!(core::mem::size_of::<*mut c_void>() == core::mem::size_of::<usize>());

/// Generate one FreeType wrapper: resolve symbol `$idx` and call it, or answer
/// `ABSENT` when the library is not there.
macro_rules! ftfn {
    ($idx:expr, $name:ident ( $($a:ident : $t:ty),* )) => {
        #[allow(non_snake_case, clippy::too_many_arguments)]
        #[doc = concat!(
            "The FreeType `", stringify!($name), "` call, resolved at runtime.\n\n",
            "# Safety\n\n",
            "The arguments must satisfy FreeType's own contract for `",
            stringify!($name),
            "`, and every pointer argument must name a live object; call it ",
            "on the thread that owns the face. Returns a nonzero `FT_Error` ",
            "when no FreeType library is present, so a caller that tests for ",
            "zero falls back to the built-in bitmap font.",
        )]
        unsafe fn $name($($a: $t),*) -> c_int {
            const _: () = assert!(($idx as usize) < FT);
            let p = sym($idx);
            if p.is_null() {
                // No FreeType on this machine, or this symbol is not in it:
                // the nonzero answer is the platform's own "it did not work".
                return ABSENT;
            }
            // SAFETY: `p` came from `dlsym` for exactly this symbol, and the
            // function-pointer type below was declared by hand against
            // FreeType's header; a data and a function pointer are the same
            // size here (asserted above).
            unsafe {
                let f: unsafe extern "C" fn($($t),*) -> c_int =
                    core::mem::transmute::<*mut c_void, unsafe extern "C" fn($($t),*) -> c_int>(p);
                f($($a),*)
            }
        }
    };
}

// One wrapper per symbol, in FT_SYMBOLS' order. The signatures are FreeType's C
// prototypes verbatim: `FT_Library`, `FT_Face`, `FT_Size` and `FT_GlyphSlot`
// are opaque handles, so they are spelled `*mut c_void`, `FT_Long` and `FT_ULong`
// are `c_long` and `c_ulong`, and `FT_Int32` is a `c_int`. rustfmt is told to
// leave each on one line rather than wrap it into a shape the macro does not
// match, which also keeps the formatting identical across rustfmt versions.
#[rustfmt::skip]
ftfn!(0, FT_Init_FreeType(library: *mut *mut c_void));
#[rustfmt::skip]
ftfn!(1, FT_New_Face(library: *mut c_void, path: *const c_char, face_index: c_long, face: *mut *mut c_void));
#[rustfmt::skip]
ftfn!(2, FT_Set_Pixel_Sizes(face: *mut c_void, pixel_width: c_uint, pixel_height: c_uint));
#[rustfmt::skip]
ftfn!(3, FT_Load_Char(face: *mut c_void, char_code: c_ulong, load_flags: c_int));
#[rustfmt::skip]
ftfn!(4, FT_Select_Charmap(face: *mut c_void, encoding: c_ulong));
#[rustfmt::skip]
ftfn!(5, FT_Done_Face(face: *mut c_void));
#[rustfmt::skip]
ftfn!(6, FT_Done_FreeType(library: *mut c_void));

// --- the ABI, by offset ------------------------------------------------------

/// The byte offsets this module reads, each named for the C field it is.
///
/// FreeType's headers are not in this tree and the crate carries no binding
/// crate, so the layout is spelled out here, once. These are LP64 numbers:
/// `long` 8 bytes, a pointer 8, `int` and `unsigned int` 4, a `short` 2.
///
/// The walk, so that a reader can check a number instead of trusting it:
///
/// `FT_FaceRec`: `num_faces` 0, `face_index` 8, `face_flags` 16,
/// `style_flags` 24, `num_glyphs` 32, `family_name` 40, `style_name` 48,
/// `num_fixed_sizes` 56 (an `int`, padded to 64), `available_sizes` 64,
/// `num_charmaps` 72, `charmaps` 80, `generic` 88 (two pointers), `bbox` 104
/// (four `long`s), `units_per_EM` 136, `ascender` 138, `descender` 140,
/// `height` 142, `max_advance_width` 144, `max_advance_height` 146,
/// `underline_position` 148, `underline_thickness` 150, `glyph` 152, `size`
/// 160, `charmap` 168.
///
/// `FT_GlyphSlotRec`: `library` 0, `face` 8, `next` 16, `glyph_index` 24 (an
/// `int`, padded to 32), `generic` 32, `metrics` 48 (eight `long`s),
/// `linearHoriAdvance` 112, `linearVertAdvance` 120, `advance` 128, and
/// `format` 144, which is an enum and therefore four bytes. The bitmap starts
/// at **152**, not 148, because `FT_Bitmap` holds a pointer and a struct with a
/// pointer in it is aligned to 8; the four bytes at 148 are that alignment's
/// padding. Inside it `rows` 152, `width` 156, `pitch` 160, `buffer` 168 (the
/// four bytes between `pitch` and the pointer are padding again), `num_grays`
/// 176, `pixel_mode` 178, `palette_mode` 179, `palette` 184 — which ends the
/// struct at 192, so `bitmap_left` is 192 and `bitmap_top` 196.
mod rec {
    /// `FT_FaceRec.num_faces`, the first field: an `FT_Long`.
    pub(super) const NUM_FACES: usize = 0;
    /// `FT_FaceRec.num_glyphs`, after the four counting `FT_Long`s.
    pub(super) const NUM_GLYPHS: usize = 32;
    /// `FT_FaceRec.family_name`, an `FT_String*` right after `num_glyphs`.
    pub(super) const FAMILY_NAME: usize = 40;
    /// `FT_FaceRec.units_per_EM`, an `FT_UShort` after the `FT_BBox`.
    pub(super) const UNITS_PER_EM: usize = 136;
    /// `FT_FaceRec.ascender`, an `FT_Short` after `units_per_EM`.
    pub(super) const ASCENDER: usize = 138;
    /// `FT_FaceRec.descender`, an `FT_Short` after `ascender`.
    pub(super) const DESCENDER: usize = 140;
    /// `FT_FaceRec.glyph`, the `FT_GlyphSlot` a load fills.
    pub(super) const GLYPH_SLOT: usize = 152;

    /// `FT_GlyphSlotRec.advance.x`, an `FT_Pos` in 26.6 fixed point.
    pub(super) const ADVANCE_X: usize = 128;
    /// `FT_GlyphSlotRec.bitmap.rows`, an `FT_UInt`.
    pub(super) const ROWS: usize = 152;
    /// `FT_GlyphSlotRec.bitmap.width`, an `FT_UInt`.
    pub(super) const WIDTH: usize = 156;
    /// `FT_GlyphSlotRec.bitmap.pitch`, an `FT_Int`, negative for a bottom-up
    /// bitmap.
    pub(super) const PITCH: usize = 160;
    /// `FT_GlyphSlotRec.bitmap.buffer`, the coverage map itself.
    pub(super) const BUFFER: usize = 168;
    /// `FT_GlyphSlotRec.bitmap_left`, an `FT_Int`: the horizontal bearing.
    pub(super) const BITMAP_LEFT: usize = 192;
    /// `FT_GlyphSlotRec.bitmap_top`, an `FT_Int`: the vertical bearing.
    pub(super) const BITMAP_TOP: usize = 196;
}

/// `FT_LOAD_RENDER`: rasterise the glyph into the slot's 8-bit gray bitmap as
/// well as loading its metrics. Without it the slot has metrics and no pixels.
const FT_LOAD_RENDER: c_int = 4;

/// `FT_ENCODING_UNICODE`, the tag `unic` (`0x756E_6963`), as
/// `FT_Select_Charmap` takes it.
const UNICODE: c_ulong = 0x756e_6963;

/// The largest bitmap side a glyph at the launcher's sizes may have, in pixels.
///
/// A 13-pixel face's widest glyph is about 20 pixels across and a substituted
/// face's is not much wider, so this is an order of magnitude of headroom for a
/// real glyph and far too little for a pointer read as a width. It is a guard,
/// not a limit the launcher ever meets.
const MAX_SIDE: u32 = 96;

/// The largest row stride a glyph bitmap may have, in bytes.
///
/// Two orders of magnitude above the 20-odd bytes a 13-pixel row needs, and far
/// below any address this process holds: a pointer read where a stride was
/// expected fails this check on the first try.
const MAX_STRIDE: u32 = 512;

/// The fields of a filled glyph slot, guarded and converted to pixels.
struct Slot {
    /// The bitmap's height, in rows.
    rows: u32,
    /// The bitmap's width, in pixels.
    width: u32,
    /// The bitmap's row stride, in bytes, always positive.
    stride: u32,
    /// FreeType's buffer for the coverage map.
    buffer: *mut c_void,
    /// The horizontal bearing, in pixels.
    left: i32,
    /// The vertical bearing, in pixels.
    top: i32,
    /// The advance, in whole pixels.
    advance: i32,
}

/// Read the glyph slot `face` holds, or `None` when it is not a bitmap a
/// character can have.
///
/// This is the one place the offsets meet reality: both the load-time probe and
/// every [`Font::glyph`] come through here, so there is one set of bounds to
/// read and one answer for a struct that does not look like a glyph.
///
/// # Safety
///
/// `face` must be a live `FT_Face` whose `FT_Load_Char` has just succeeded.
unsafe fn slot_fields(face: *const c_void) -> Option<Slot> {
    // SAFETY: the face is live, and the caller has just filled its slot.
    let slot = unsafe { read_at::<*mut c_void>(face, rec::GLYPH_SLOT) };
    if slot.is_null() {
        return None;
    }
    // SAFETY: `slot` is the face's own live glyph slot.
    let rows = unsafe { read_at::<c_uint>(slot, rec::ROWS) };
    // SAFETY: as above.
    let width = unsafe { read_at::<c_uint>(slot, rec::WIDTH) };
    // SAFETY: as above.
    let pitch = unsafe { read_at::<c_int>(slot, rec::PITCH) };

    if rows > MAX_SIDE || width > MAX_SIDE {
        return None;
    }
    let stride = pitch.unsigned_abs();
    if stride > MAX_STRIDE {
        return None;
    }
    // A stride narrower than the row it carries is not a bitmap: it is the
    // signature of a field read at the wrong offset.
    if width > 0 && stride < width {
        return None;
    }

    // SAFETY: `slot` is live, and the three fields below are its own.
    let buffer = unsafe { read_at::<*mut c_void>(slot, rec::BUFFER) };
    // SAFETY: as above.
    let left = unsafe { read_at::<c_int>(slot, rec::BITMAP_LEFT) };
    // SAFETY: as above.
    let top = unsafe { read_at::<c_int>(slot, rec::BITMAP_TOP) };
    // SAFETY: as above.
    let advance = unsafe { read_at::<c_long>(slot, rec::ADVANCE_X) };

    if rows > 0 && buffer.is_null() {
        return None;
    }
    // A bearing a whole bitmap away from the pen is not a bearing.
    if left.unsigned_abs() > MAX_SIDE || top.unsigned_abs() > MAX_SIDE {
        return None;
    }

    Some(Slot {
        rows,
        width,
        stride,
        buffer,
        left,
        top,
        advance: pixels(advance),
    })
}

/// Whether the numbers `face` reports are numbers a font can have.
///
/// This is the guard that makes the face offsets safe: if `num_faces`,
/// `num_glyphs`, `units_per_EM`, the ascender and the descender all land in
/// ranges a real face uses, the layout is the one this build expects.
///
/// # Safety
///
/// `face` must be a live `FT_Face` returned by `FT_New_Face`.
unsafe fn face_is_a_font(face: *const c_void) -> bool {
    // SAFETY: the caller guarantees a live FT_FaceRec, so every offset below
    // names a field of it.
    unsafe {
        let num_faces = read_at::<c_long>(face, rec::NUM_FACES);
        let num_glyphs = read_at::<c_long>(face, rec::NUM_GLYPHS);
        let family = read_at::<*mut c_void>(face, rec::FAMILY_NAME);
        let units_per_em = read_at::<c_ushort>(face, rec::UNITS_PER_EM);
        let ascender = read_at::<c_short>(face, rec::ASCENDER);
        let descender = read_at::<c_short>(face, rec::DESCENDER);
        let slot = read_at::<*mut c_void>(face, rec::GLYPH_SLOT);
        num_faces >= 1
            && num_glyphs > 0
            && (16..=8192).contains(&units_per_em)
            && ascender > 0
            && descender < 0
            && !family.is_null()
            && !slot.is_null()
    }
}

/// Whether `face`'s glyph slot holds a bitmap and an advance a character can
/// have.
///
/// The probe loads `A` — a letter every text face carries — and reads the slot
/// back through `slot_fields`, the same guard a glyph read goes through. A face
/// that fails it is one this build cannot read, and failing at load time is
/// what sends the caller to the bitmap font instead of leaving it with a face
/// that draws nothing.
///
/// # Safety
///
/// `face` must be a live `FT_Face` with a charmap and a pixel size set.
unsafe fn glyph_slot_is_a_font(face: *mut c_void) -> bool {
    // SAFETY: the face is live, and `A` is a code point FreeType can map.
    let error = unsafe { FT_Load_Char(face, b'A' as c_ulong, FT_LOAD_RENDER) };
    if error != 0 {
        return false;
    }
    // SAFETY: the load above filled the face's slot.
    let slot = unsafe { slot_fields(face) };
    // A letter that rasterised to nothing, or to a stride no bitmap has, is a
    // slot this build cannot read.
    slot.is_some_and(|slot| slot.width > 0 && slot.rows > 0 && slot.advance > 0)
}

/// An `FT_Pos`, which is 26.6 fixed point, as whole pixels.
///
/// FreeType reports an advance in 1/64 of a pixel; the launcher steps the pen
/// in whole pixels, and the half-pixel bias makes that step land on the pixel
/// the glyph's own width rounds to rather than falling a pixel short of it at
/// every character.
fn pixels(fixed: c_long) -> i32 {
    ((fixed + 32) >> 6) as i32
}

/// Read a `T` at `base + offset`.
///
/// `T` is the field's own C type: `c_long` for `FT_Long` and `FT_Pos`,
/// `c_uint` for `FT_UInt`, `c_int` for `FT_Int`, `c_ushort` for `FT_UShort`,
/// `c_short` for `FT_Short`, and `*mut c_void` for a handle. The read is
/// unaligned, so an offset the compiler cannot prove aligned is still a defined
/// read.
///
/// # Safety
///
/// `base` must point at a live struct, `offset` must be one of the constants in
/// `rec` for that struct, and `T` must be the C type the field has there.
unsafe fn read_at<T: Copy>(base: *const c_void, offset: usize) -> T {
    // SAFETY: the caller guarantees the field lies inside the object; the read
    // is unaligned, so only the extent of the object matters.
    unsafe { ptr::read_unaligned(base.cast::<u8>().add(offset).cast::<T>()) }
}

// --- tests -------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The search starts where every desktop keeps DejaVu.
    ///
    /// Pure data: this never calls `dlopen`, so it runs on a machine with no
    /// FreeType and no display.
    #[test]
    fn the_candidate_list_starts_with_dejavu() {
        let first = candidate_paths()[0];
        assert!(first.contains("DejaVuSans"), "{first}");
        assert!(bold_candidate_paths()[0].contains("DejaVuSans-Bold"));
    }

    /// Every candidate is an absolute path to a TrueType file.
    #[test]
    fn every_candidate_is_an_absolute_ttf_path() {
        for path in candidate_paths().iter().chain(bold_candidate_paths()) {
            assert!(path.starts_with('/'), "{path} is not absolute");
            assert!(path.contains('/'), "{path} has no separator");
            assert!(path.ends_with(".ttf"), "{path} is not a TrueType file");
        }
    }

    /// Both hosts' font roots are searched: Linux's system tree and FreeBSD's
    /// `/usr/local`.
    #[test]
    fn the_search_lists_linux_and_freebsd_locations() {
        let paths = candidate_paths();
        assert!(paths.iter().any(|p| p.starts_with("/usr/share/fonts/")));
        assert!(
            paths
                .iter()
                .any(|p| p.starts_with("/usr/local/share/fonts/"))
        );
        let bold = bold_candidate_paths();
        assert!(bold.iter().any(|p| p.starts_with("/usr/share/fonts/")));
        assert!(
            bold.iter()
                .any(|p| p.starts_with("/usr/local/share/fonts/"))
        );
    }

    /// The offsets are ordered the way the C fields are, which is the cheapest
    /// check that a re-edit kept the walk in `rec` in one piece.
    #[test]
    fn the_offsets_are_ordered_like_the_structs() {
        let face = [
            rec::NUM_FACES,
            rec::NUM_GLYPHS,
            rec::FAMILY_NAME,
            rec::UNITS_PER_EM,
            rec::ASCENDER,
            rec::DESCENDER,
            rec::GLYPH_SLOT,
        ];
        for pair in face.windows(2) {
            assert!(pair[0] < pair[1], "the face offsets must climb");
        }
        let slot = [
            rec::ADVANCE_X,
            rec::ROWS,
            rec::WIDTH,
            rec::PITCH,
            rec::BUFFER,
            rec::BITMAP_LEFT,
            rec::BITMAP_TOP,
        ];
        for pair in slot.windows(2) {
            assert!(pair[0] < pair[1], "the slot offsets must climb");
        }
        // The bitmap starts on an 8-byte boundary because it holds a pointer.
        let rows = rec::ROWS;
        assert_eq!(rows % 8, 0, "the bitmap must be pointer-aligned");
    }

    /// A path with an interior NUL is refused rather than truncated, and a good
    /// one comes back NUL-terminated.
    #[test]
    fn a_path_is_a_c_string_or_nothing() {
        let good = c_path("/usr/share/fonts/TTF/DejaVuSans.ttf");
        assert_eq!(good.as_deref().and_then(|b| b.last().copied()), Some(0));
        assert!(c_path("bad\0path.ttf").is_none());
    }

    /// The symbol table is well-formed: one entry per wrapper, each a
    /// NUL-terminated C string, and no two names equal. Pure data — it never
    /// calls `dlopen` either.
    #[test]
    fn the_ft_symbols_are_nul_terminated_and_distinct() {
        assert_eq!(FT_SYMBOLS.len(), FT_COUNT);
        for name in FT_SYMBOLS {
            assert_eq!(name.last(), Some(&0), "a symbol name needs its NUL");
        }
        for (i, name) in FT_SYMBOLS.iter().enumerate() {
            for other in FT_SYMBOLS.iter().skip(i + 1) {
                assert_ne!(name, other, "duplicate symbol at {i}");
            }
        }
    }
}
