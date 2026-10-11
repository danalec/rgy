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
const EXTS: &[&str] = &[
    // archive
    "zip", "7z", "rar", "tar", "gz", "bz2", "xz", "zst", "cpio",
    // office
    "docx", "xlsx", "pptx", "odt", "ods",
    // threed
    "glb", "stl", "ply", "fbx", "pcd", "las", "vtk", "blend", "3ds",
    // llm
    "gguf", "safetensors", "npy", "pb", "onnx", "tiktoken", "ipynb", "keras", "pt", "pkl",
    // media
    "jpg", "png", "tiff", "mp3", "flac", "wav", "fits", "dcm", "ttf", "woff", "mobi",
    // tabular
    "sqlite", "dbf", "shp", "parquet", "mat", "nc", "h5",
    // docs
    "pdf", "rst", "tex",
    // mail
    "eml", "mbox", "msg",
    // wasm + inspect (magic-based; ext is a fallback hint)
    "wasm", "bin", "dat", "exe", "dll",
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
    };
    // rotate extensions across runs: byte 0 picks, rest is the payload
    let ext = EXTS[data[0] as usize % EXTS.len()];
    let path = format!("fuzz/sample.{ext}");
    let mut out = Vec::new();
    let mut expanded = 0usize;
    rgx_core::process(registry(), &path, &data[1..], 0, &ctx, &mut expanded, &mut out);
    // touch the output so unit extraction isn't dead-stripped
    std::hint::black_box(out.len());
});
