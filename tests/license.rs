// SPDX-License-Identifier: Apache-2.0

//! Licensing invariant.
//!
//! `COPYING` splits this workspace in two: the product and the end-to-end
//! harness are Apache-2.0, and the tooling around them is MIT OR
//! Apache-2.0. The vendored rgx crates are Apache-2.0 as well, because they
//! are upstream rgx's files under upstream's licence and this project does
//! not relicense somebody else's work.
//!
//! The signal is the per-file header, and the check is that the header is
//! the FIRST line of the file — a licence stated in the middle of a file is
//! a licence nobody reads. One file without the header makes `COPYING`
//! false and nothing else notices: `rgx-core/src/tv_runtime/compact.rs`
//! shipped exactly that way in `rgx` and was then transported here with the
//! header still missing.
//!
//! Three rules, and each one has bitten this repository before:
//!
//!   1. The product's files say Apache-2.0 and nothing else. The README
//!      said so for a long time while `COPYING` said MIT OR Apache-2.0
//!      for the same files — two documents, two answers, and the audit
//!      that should have caught it only looked at the Zen trees.
//!   2. The vendored trees say Apache-2.0 too. They are upstream rgx's
//!      files under upstream's licence, and upstream offers Apache-2.0 and
//!      no other option. The composed expressions are still on the allowed
//!      list, so that a file carrying one is a report rather than a build
//!      failure — but there is not one.
//!   3. A vendored file may not claim the MIT option. Upstream offers
//!      Apache-2.0 and no other, so a vendored file that says "MIT OR
//!      Apache-2.0" makes a promise about terms this project cannot grant
//!      on upstream's behalf.

use std::path::Path;
use std::process::Command;

/// The product: Apache-2.0, and only Apache-2.0.
const PRODUCT: &[&str] = &[
    "crates/name-core/",
    "crates/par/",
    "crates/discovery/",
    "crates/discovery-windows/",
    "crates/discovery-linux/",
    "crates/discovery-macos/",
    "crates/discovery-freebsd/",
    "crates/discovery-dispatch/",
    "crates/engine/",
    "crates/proto/",
    "crates/autostart/",
    "crates/launcher/",
    "crates/launcher-windows/",
    "crates/launcher-x11/",
    "crates/pick/",
    "crates/adapters/",
    "bins/searchctl/",
    "bins/searchd/",
    "bins/search-ui/",
    "bins/zen-rs/",
    "bins/rgxy/",
];

/// The vendored rgx crates: upstream's own files, under upstream's terms.
const VENDORED: &[&str] = &[
    "crates/rgx-core/",
    "crates/rar-codec/",
    "crates/rgx-adapters-archive/",
    "crates/rgx-adapters-docs/",
    "crates/rgx-adapters-inspect/",
    "crates/rgx-adapters-llm/",
    "crates/rgx-adapters-mail/",
    "crates/rgx-adapters-media/",
    "crates/rgx-adapters-office/",
    "crates/rgx-adapters-pdf/",
    "crates/rgx-adapters-tabular/",
    "crates/rgx-adapters-threed/",
];

/// The harness: Apache-2.0, like the product.
const HARNESS: &[&str] = &["tests/"];

/// The trees that hold Rust code, and that the lists above must name.
///
/// Every Rust file under one of these is the product, the harness or
/// somebody else's code: there is no fourth answer, and a tree that none of
/// the lists names is a tree whose licence nobody has said out loud.
const RUST_TREES: &[&str] = &["crates/", "bins/"];

const APACHE: &str = "// SPDX-License-Identifier: Apache-2.0";
const MIT_OR_APACHE: &str = concat!("// SPDX-License-Identifier: MIT OR ", "Apache-2.0");

/// Files inside an Apache-2.0 tree that carry a licence of their own.
///
/// Empty on purpose: the three files that arrived from `fsearch` (MIT) were
/// each replaced by an independent Apache-2.0 implementation, and the
/// notice that travelled with them is gone. The list stays explicit rather
/// than a pattern, so a future file claiming another licence has to be
/// added here on purpose and the audit says so instead of quietly allowing
/// it.
const PRODUCT_EXCEPTIONS: &[(&str, &str)] = &[];

/// The licence expressions a vendored file may carry.
///
/// Upstream offers Apache-2.0 and nothing else, and the five files whose
/// layout was learned from a third-party implementation say the same: a
/// layout is knowledge, not code, and the code that encodes it was written
/// for the project. `NOTICE` records where that knowledge came from; no
/// second licence composes into these files.
///
/// The composed expressions are listed here anyway, so that a file that
/// still carries one is not a build failure but a report: the audit would
/// rather say that the tree is not uniform than that it is wrong.
const VENDORED_ALLOWED: &[&str] = &[
    APACHE,
    "// SPDX-License-Identifier: Apache-2.0 AND MIT",
    "// SPDX-License-Identifier: Apache-2.0 AND BSD-3-Clause",
    "// SPDX-License-Identifier: Apache-2.0 AND BSD-2-Clause AND BSD-3-Clause",
    "// SPDX-License-Identifier: Apache-2.0 AND OLDAP-2.8",
    "// SPDX-License-Identifier: Apache-2.0 AND PostgreSQL",
];

/// Every Rust file the workspace tracks, relative to the workspace root.
///
/// The list comes from git rather than from a directory walk, so the audit
/// sees what the repository holds and not what the checkout happens to
/// have on disk.
fn tracked_files(root: &Path) -> Vec<String> {
    match git_files(root) {
        Some(files) => files,
        // No git here: the tree is a copy of a checkout, or a source tree
        // somebody unpacked. The walk is the list, and it is the stricter
        // one -- it sees the files that git does not know about too.
        None => walk_files(root),
    }
}

/// The files git holds, when there is a work tree to ask.
///
/// `None` when there is no git, or no work tree: the caller then walks the
/// tree instead. The audit does not skip itself either way -- a gate that
/// skips is not a gate.
fn git_files(root: &Path) -> Option<Vec<String>> {
    let listed = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output()
        .ok()?;
    if !listed.status.success() {
        return None;
    }
    let files: Vec<String> = std::str::from_utf8(&listed.stdout)
        .expect("git ls-files emits utf-8")
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    assert!(!files.is_empty(), "git ls-files returned nothing");
    Some(files)
}

/// The folders that are not source: the build's own output, the
/// repository's metadata, and the caches the tools leave behind.
const NOT_SOURCE: &[&str] = &[".git", "target", ".dist", ".remember", "__pycache__"];

/// The files under `root`, relative to it, for a tree with no git.
fn walk_files(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    walk(root, root, &mut files);
    files.sort();
    assert!(
        !files.is_empty(),
        "the SPDX audit cannot run: {} holds no file at all",
        root.display()
    );
    files
}

/// The walk itself.
fn walk(root: &Path, dir: &Path, files: &mut Vec<String>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| {
        panic!(
            "the SPDX audit cannot run: {} cannot be read ({e})",
            dir.display()
        )
    });
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if NOT_SOURCE.contains(&name.as_ref()) {
                continue;
            }
            walk(root, &path, files);
            continue;
        }
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        files.push(rel.to_string_lossy().replace('\\', "/"));
    }
}

fn under(rel: &str, roots: &[&str]) -> bool {
    roots.iter().any(|p| rel.starts_with(p))
}

/// The first line of a tracked file, or `None` when it cannot be read.
fn first_line(root: &Path, rel: &str) -> Option<String> {
    let text = std::fs::read_to_string(root.join(rel)).ok()?;
    text.lines().next().map(|l| l.to_string())
}

/// Every source file must carry the licence its tree is licensed under, and
/// it must be the first line of the file.
#[test]
fn every_source_file_carries_the_licence_its_tree_is_licensed_under() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the e2e harness sits in a workspace subdirectory");
    let files = tracked_files(root);

    let mut audited = 0usize;
    let mut wrong = Vec::new();
    for rel in &files {
        if !rel.ends_with(".rs") {
            continue;
        }
        let (expected, why): (&[&str], &str) = if under(rel, PRODUCT) {
            let exception = PRODUCT_EXCEPTIONS.iter().find(|(path, _)| path == rel);
            match exception {
                Some((_, expression)) => (
                    &[*expression][..],
                    "the file keeps the licence it arrived with",
                ),
                None => (&[APACHE], "the product is Apache-2.0"),
            }
        } else if under(rel, VENDORED) {
            (
                VENDORED_ALLOWED,
                "a vendored tree carries upstream's licence",
            )
        } else if under(rel, HARNESS) {
            (&[APACHE], "the harness is Apache-2.0, like the product")
        } else if under(rel, RUST_TREES) {
            // A Rust file in a tree that no list names. The lists are the
            // rule, and a file that is in none of them is a hole in the
            // rule: nobody has said what that tree's licence is.
            wrong.push(format!("{rel}: no list says what this tree's licence is"));
            continue;
        } else {
            // The tooling under scripts/, tools/ and benchmark/ carries no
            // SPDX header: it is not Rust, and its licence is in COPYING.
            continue;
        };
        audited += 1;
        let Some(first) = first_line(root, rel) else {
            // A file the audit cannot read is a file the audit knows
            // nothing about, and a licence nobody read is a licence nobody
            // checked.
            wrong.push(format!("{rel}: the audit could not read it"));
            continue;
        };
        if !expected.contains(&first.as_str()) {
            wrong.push(format!("{rel}: first line is {first:?} ({why})"));
        }
    }

    // The path list is written by hand, so it can rot: assert the audit
    // actually looked at something instead of passing on an empty match.
    assert!(
        audited > 0,
        "the SPDX audit matched no source file at all; the path lists are stale"
    );
    assert!(
        wrong.is_empty(),
        "these files do not start with the licence expression their tree is licensed \
         under, so COPYING's claim about them is false:\n{}",
        wrong.join("\n")
    );
}

/// A vendored file may not claim the MIT option.
#[test]
fn the_vendored_trees_do_not_claim_the_mit_option() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the e2e harness sits in a workspace subdirectory");
    let files = tracked_files(root);

    let mut audited = 0usize;
    let mut retired = Vec::new();
    for rel in &files {
        if !rel.ends_with(".rs") || !under(rel, VENDORED) {
            continue;
        }
        audited += 1;
        let Ok(text) = std::fs::read_to_string(root.join(rel)) else {
            eprintln!("cannot read {rel}; skipping it in the SPDX audit");
            continue;
        };
        if text.contains(MIT_OR_APACHE) {
            retired.push(rel.to_string());
        }
    }

    assert!(
        audited > 0,
        "the MIT-option audit matched no vendored source file at all; the VENDORED \
         path list is stale"
    );
    assert!(
        retired.is_empty(),
        "these vendored files still name a licence the upstream project does not \
         offer ({MIT_OR_APACHE}), which this project cannot grant on its behalf:\n{}",
        retired.join("\n")
    );
}

/// Every exception has to be an exception still.
///
/// An exception list rots quietly: the file is renamed, or it changes its
/// licence, and the list keeps granting it a licence the file no longer
/// carries. This checks both halves — that the file is still there, and
/// that it still says what the list says it says.
#[test]
fn every_licence_exception_is_an_exception_still() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the e2e harness sits in a workspace subdirectory");

    for (rel, expression) in PRODUCT_EXCEPTIONS {
        let path = root.join(rel);
        assert!(
            path.exists(),
            "{rel} is listed as a licence exception but the file is gone; the exception \
             list is stale"
        );
        let first = first_line(root, rel)
            .unwrap_or_else(|| panic!("cannot read {rel}; the exception list cannot be checked"));
        assert_eq!(
            first, *expression,
            "{rel} is listed as a licence exception for {expression}, but its first line \
             says {first:?}. Either the file changed its licence or the list is stale."
        );
    }
}

/// The two licence claims in `COPYING` have to name the same trees the
/// audit checks. A `COPYING` that says "Apache-2.0" about a tree whose
/// files say otherwise is a false statement, and this is the check that
/// keeps the document and the headers in step.
#[test]
fn copying_names_every_tree_the_audit_checks() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the e2e harness sits in a workspace subdirectory");
    let copying = std::fs::read_to_string(root.join("COPYING")).expect("COPYING exists");

    for tree in PRODUCT.iter().chain(VENDORED.iter()) {
        let name = tree.trim_end_matches('/');
        assert!(
            copying.contains(name),
            "COPYING does not mention {name}, but the audit licenses that tree"
        );
    }
}

/// `COPYING` and `NOTICE` say the fsearch notice was removed with the last
/// MIT source file. The per-file SPDX audit reads `.rs` only, so a stray
/// copy of the notice in a crate directory would contradict both documents
/// and pass every other check. It did: `crates/name-core/` carried one
/// until this guard was written.
#[test]
fn no_third_party_notice_file_is_tracked() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the e2e harness sits in a workspace subdirectory");
    let offenders: Vec<String> = tracked_files(root)
        .into_iter()
        .filter(|rel| {
            rel.ends_with("LICENSE-MIT-FSEARCH")
                // A tree with no git falls back to a directory walk, which
                // sees build scratch that a checkout would never track.
                && !rel.starts_with(".release-tree/")
                && !rel.starts_with("target/")
                // `git ls-files` reads the index, which still names a file
                // deleted on disk but not yet staged. The claim is about a
                // notice that is actually present, so require the bytes.
                && root.join(rel).exists()
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "a third-party MIT notice is still tracked, but COPYING and NOTICE say \
         it was removed with the last MIT source file:\n  {}",
        offenders.join("\n  ")
    );
}
