// SPDX-License-Identifier: Apache-2.0

//! Coverage-guided fuzzing of the full adapter pipeline.
//!
//! libFuzzer steers arbitrary bytes through every registered adapter by
//! rotating the claimed extension (first bytes of input pick it), so deep
//! parser states unreachable by the deterministic mutator get explored.
//! Panics, index-out-of-bounds and hangs are the failure modes under test —
//! `process` must never propagate them.
//!
//! Run:  cargo +nightly fuzz run process -- -max_total_time=300

#![no_main]

use libfuzzer_sys::fuzz_target;

/// Extensions with a claiming adapter, one per family. The first byte of
/// input selects which parser gets the rest of the bytes.
///
/// Every adapter the registry can reach by extension belongs here. A missing
/// entry is not a smaller hole than it looks: the fuzzer is the only thing in
/// the tree that drives the parsers with bytes no fixture ever produces, so an
/// unlisted extension is an adapter whose deep states nothing can reach.
const EXTS: &[&str] = &[
    // archive
    "zip", "7z", "rar", "tar", "gz", "bz2", "xz", "zst", "cpio", "ar", "deb",
    "br", "lz4", "lzma", "tbz", "txz", "tzst",
    // backups
    "bundle", "dump",
    // disk images (block containers and the catalogues they carry)
    "iso", "img", "wim", "vhd", "vhdx", "qcow2", "vmdk", "vdi", "erofs",
    "apfs", "hfs", "hfsplus", "dmg", "sqsh", "squashfs",
    // office
    "docx", "xlsx", "pptx", "odt", "ods", "odp", "odg", "odi", "doc", "xls",
    "ppt", "epub", "vsd", "vsdx", "xps", "rtf", "abw", "wpd", "wps", "wfb", "hwd",
    // markup renderers (Reach::Render, reached through process regardless)
    "md", "markdown", "html", "htm", "xhtml", "org", "textile", "wiki",
    "mediawiki", "man", "t2t", "fb2", "latex", "ltx",
    // threed
    "glb", "stl", "ply", "fbx", "pcd", "las", "vtk", "blend", "3ds", "laz",
    "mrc", "nii",
    // llm
    "gguf", "safetensors", "npy", "pb", "onnx", "model", "mlmodel", "tiktoken",
    "keras", "pt", "pt2", "pth", "pkl", "pickle",
    // media
    "jpg", "png", "tiff", "tif", "mp3", "flac", "wav", "fits", "dcm", "ttf",
    "woff", "mobi", "mkv", "avi", "asf", "flv", "mpeg", "mpg", "ts", "bmp", "gif",
    // tabular
    "sqlite", "dbf", "dbc", "shp", "parquet", "arrow", "feather", "mat", "nc",
    "cdf", "h5", "hdf5", "slk", "dif", "wk1", "wk2", "wk3", "wk4", "wks", "123",
    "mdb", "accdb", "accde", "accdt", "bson", "rdb", "duckdb",
    // firebird / interbase
    "fdb", "gdb",
    // compound files (OLE2/CFB)
    "hwp", "one", "nsf", "mpp", "cdr", "wab", "rvt",
    // apple property lists and ISO-BMFF
    "bplist", "icloud", "mobileconfig", "heic", "avif", "3gp", "mp4", "m4a", "mov",
    // raw rasters and djvu
    "nef", "dng", "cr2", "djvu", "djv",
    // genomics
    "bam", "bcf", "cram", "sam",
    // cad
    "dwg", "x_t", "x_b", "sab", "dgn",
    // scientific
    "grb", "grb1", "grb2", "grib", "bufr", "segy", "sgy", "root", "n5", "n5z",
    // docs
    "pdf", "rst", "tex",
    // mail
    "eml", "emlx", "mbox", "msg", "pst", "ost", "mmdf", "msf", "mboxindex",
    // wasm + inspect (magic-based; ext is a fallback hint)
    "wasm", "bin", "dat", "exe", "dll", "dylib", "elf", "dex", "rsrc",
];

fn registry() -> &'static rgx_core::Registry {
    static REG: std::sync::OnceLock<rgx_core::Registry> = std::sync::OnceLock::new();
    REG.get_or_init(|| {
        let mut v: Vec<Box<dyn rgx_core::Adapter>> = Vec::new();
        v.extend(rgx_adapters_archive::adapters());
        v.extend(rgx_adapters_office::adapters());
        v.extend(rgx_adapters_threed::adapters());
        v.extend(rgx_adapters_llm::adapters());
        v.extend(rgx_adapters_media::adapters());
        v.extend(rgx_adapters_tabular::adapters());
        v.extend(rgx_adapters_docs::adapters());
        v.extend(rgx_adapters_mail::adapters());
        v.extend(rgx_adapters_inspect::adapters());
        v.extend(rgx_adapters_pdf::adapters());
        rgx_core::Registry::new(v)
    })
}

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }
    let ctx = rgx_core::Ctx {
        max_depth: 4,
        max_members: 256,
        max_file_size: 1 << 20,
        filter: None,
        facts_sink: None,
    };
    // rotate extensions across runs: byte 0 picks, rest is the payload
    let ext = EXTS[data[0] as usize % EXTS.len()];
    let path = format!("fuzz/sample.{ext}");
    let mut out = Vec::new();
    rgx_core::process(registry(), &path, &data[1..], 0, &ctx, &mut out);
    // touch the output so unit extraction isn't dead-stripped
    std::hint::black_box(out.len());
});
