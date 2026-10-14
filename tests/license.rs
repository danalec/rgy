//! Licensing invariant for the Apache-2.0-only trees.
//!
//! `COPYING` splits this workspace by what the code does: the Zen components
//! and the Tesla Valve pack store are Apache-2.0 only, everything else is
//! MIT OR Apache-2.0. Where a whole crate is Apache-only — `zen-rs`,
//! `search-ui`, `discovery-windows` - its manifest says so. Where the
//! Apache-only code is a subsystem inside a dual-licensed crate - every file
//! under `rgx-core`'s `tv`, `tv_runtime` and `bin/rgx-tv.rs` - the per-file
//! header is the whole signal, because `rgx-core` has to publish
//! `MIT OR Apache-2.0` for the remainder sitting beside it.
//!
//! One file without the header makes `COPYING` false and nothing else
//! notices: `rgx-core/src/tv_runtime/compact.rs` shipped exactly that way in
//! `rgx` and was then transported here with the header still missing.

use std::path::Path;
use std::process::Command;

/// Paths `COPYING` licenses under the Apache License 2.0 only. A prefix
/// ending in `/` names a tree; anything else is matched as a path prefix.
const APACHE_ONLY: &[&str] = &[
    "bins/zen-rs/",
    "bins/search-ui/",
    "crates/discovery-windows/",
    "crates/rgx-core/src/tv/",
    "crates/rgx-core/src/tv_runtime/",
    "crates/rgx-core/src/bin/rgx-tv.rs",
];

/// The header each of those files must carry. The crate manifests are not
/// audited here: they carry the SPDX identifier in their `license` field,
/// which is the machine-readable form for a package.
const SPDX: &str = "SPDX-License-Identifier: Apache-2.0";

/// Every source file under an Apache-2.0-only path must carry its
/// `SPDX-License-Identifier: Apache-2.0` header.
///
/// Skipped when `git` is missing or the tree is not a work tree, so it cannot
/// become a spurious failure somewhere else.
#[test]
fn every_apache_only_source_file_carries_its_spdx_header() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the e2e harness sits in a workspace subdirectory");
    let Ok(listed) = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output()
    else {
        eprintln!("git unavailable; skipping the SPDX audit");
        return;
    };
    if !listed.status.success() {
        eprintln!("not a git work tree; skipping the SPDX audit");
        return;
    }

    let files: Vec<&str> = std::str::from_utf8(&listed.stdout)
        .expect("git ls-files emits utf-8")
        .split('\0')
        .filter(|s| !s.is_empty())
        .collect();
    assert!(!files.is_empty(), "git ls-files returned nothing");

    let mut audited = 0usize;
    let mut unmarked = Vec::new();
    for rel in &files {
        if !rel.ends_with(".rs") || !APACHE_ONLY.iter().any(|p| rel.starts_with(p)) {
            continue;
        }
        audited += 1;
        let Ok(text) = std::fs::read_to_string(root.join(rel)) else {
            // An unreadable file is not an answer, and reporting it as
            // unmarked would blame the license rather than the filesystem.
            eprintln!("cannot read {rel}; skipping it in the SPDX audit");
            continue;
        };
        if !text.contains(SPDX) {
            unmarked.push(rel.to_string());
        }
    }

    // The path list is written by hand, so it can rot: assert the audit
    // actually looked at something instead of passing on an empty match.
    assert!(
        audited > 0,
        "the SPDX audit matched no source file at all; the APACHE_ONLY path list is stale"
    );
    assert!(
        unmarked.is_empty(),
        "these files sit under a path COPYING licenses Apache-2.0 only but carry no \
         {SPDX} header, so COPYING's claim about them is false:\n{}",
        unmarked.join("\n")
    );
}
