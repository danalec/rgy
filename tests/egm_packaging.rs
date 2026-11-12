// SPDX-License-Identifier: Apache-2.0

//! The published crates must not carry the EGM contracts.
//!
//! The contracts are tracked working documents, one per directory, and
//! about the code rather than of it. They sit inside the crates' source
//! directories, so a tracked `EGM.md` would be carried into the registry
//! by `cargo package`. The registry is for the code.
//!
//! The rule is a negation in each manifest's `exclude` list, and the list
//! is per package: a crate added later would carry the contracts unless
//! somebody remembered. This asks cargo what each package would carry,
//! which is the only answer that counts, and also holds every owned
//! manifest to the exclusion itself.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The workspace members named by the root manifest, in the order they are
/// listed. A virtual workspace has no root package of its own.
fn members(root: &Path) -> Vec<String> {
    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).expect("the root manifest");
    let mut inside = false;
    let mut out = Vec::new();
    for line in manifest.lines() {
        let line = line.trim();
        if line == "members = [" {
            inside = true;
            continue;
        }
        if inside && line == "]" {
            break;
        }
        if !inside {
            continue;
        }
        let quoted = line.split('"').nth(1).unwrap_or("");
        if !quoted.is_empty() {
            out.push(quoted.to_string());
        }
    }
    assert!(!out.is_empty(), "the workspace names no members");
    out
}

fn package_name(dir: &Path) -> String {
    let manifest = std::fs::read_to_string(dir.join("Cargo.toml")).expect("a member manifest");
    for line in manifest.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix("name = ") {
            return name.trim().trim_matches('"').to_string();
        }
    }
    panic!("{} has no package name", dir.display());
}

fn is_vendored(member: &str) -> bool {
    member.starts_with("crates/rgx-") || member.starts_with("crates/rar-codec")
}

/// The files cargo would put in the package. `--allow-dirty` is required:
/// the working tree carries ignored scratch that cargo would otherwise
/// refuse to package at all, and the list is the question here.
fn listed(name: &str, root: &Path) -> Vec<String> {
    let out = Command::new(env!("CARGO"))
        .args(["package", "--list", "-p", name, "--allow-dirty"])
        .current_dir(root)
        .output()
        .expect("cargo runs");
    assert!(
        out.status.success(),
        "cargo package --list failed for {name}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.to_string())
        .collect()
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the e2e harness sits in a workspace subdirectory")
        .to_path_buf()
}

/// Every owned crate names the exclusion, so a crate added later cannot
/// quietly start shipping the contracts. The vendored trees are excluded
/// from this rule: they are upstream's, and their EGM.md files are ignored.
#[test]
fn every_owned_manifest_names_the_exclusion() {
    let root = workspace_root();
    let mut missing = Vec::new();
    for member in members(&root) {
        if is_vendored(&member) || member == "tests" {
            continue;
        }
        let manifest = std::fs::read_to_string(root.join(&member).join("Cargo.toml"))
            .unwrap_or_else(|e| panic!("{member}: {e}"));
        if !manifest.contains("exclude = [\"**/EGM.md\"]") {
            missing.push(member);
        }
    }
    assert!(
        missing.is_empty(),
        "these owned crates do not exclude EGM.md from their packages: {}",
        missing.join(", ")
    );
}

/// What cargo says it would publish is the authority. A manifest rule that
/// drifted from cargo's behaviour is not an exclusion.
#[test]
fn no_package_carries_a_contract() {
    let root = workspace_root();
    let mut offenders = Vec::new();
    for member in members(&root) {
        let dir = root.join(&member);
        let name = package_name(&dir);
        for file in listed(&name, &root) {
            if file.ends_with("EGM.md") {
                offenders.push(format!("{name}: {file}"));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "the contracts are working documents and must not go to the registry, \
         but {} would carry one:\n  {}",
        offenders.len(),
        offenders.join("\n  ")
    );
}
