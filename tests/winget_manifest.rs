// SPDX-License-Identifier: Apache-2.0
//
//! The winget manifests against the release they name.
//!
//! The three manifests are release data: the package version, the URL of
//! the archive the installer downloads, the page the release notes point
//! at and the executables the archive holds all name one release. The
//! publisher rewrites the version, the URL, the digest and the date with a
//! `sed` that matches a line beginning with a key, so a field that is a
//! list item -- or a second file that no sed touched -- is the shape of a
//! silent half-rewrite: the manifest keeps the number of the release before
//! and installs nothing, saying nothing about why.
//!
//! The check is internal on purpose, the way the paired upstream's is. The
//! manifests name the release they install, and that release is the one the
//! last publish made. Comparing them with `Cargo.toml` would fail the gate
//! of the release that is about to fix them: the crates are 0.1.10 (three
//! numbers, the registry takes three) while the release and its binary
//! assets carry 0.1.10.0 (four, the fix count of the line). So the test
//! holds the manifests to one another and to their own tag, and leaves the
//! crate version out of it.
//!
//! rgy differs from the paired upstream in one place: its nested installer
//! files are flat names (`searchctl.exe`, ...) and carry no version, so
//! there is no stale release number to hide inside a `RelativeFilePath`.
//! What is checked instead is that the manifest names the whole product --
//! the four binaries the installers copy and the `rgy` alias -- and that
//! every field that DOES name a release names the same one.

use std::path::{Path, PathBuf};

/// The workspace root. The e2e harness is its own crate in `tests/`, so the
/// `.winget/` directory is one level up, not beside this file's manifest.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the e2e harness sits in a workspace subdirectory")
        .to_path_buf()
}

fn manifest(name: &str) -> String {
    let path = workspace_root().join(".winget").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!(".winget/{name} is there: {e}"))
}

/// The value of a `key: value` line.
///
/// A list item is a line that begins with a dash, and the key of a list
/// item is the key after the dash -- so a field that only read lines that
/// begin with the key would not see the list items at all.
fn field(text: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}: ");
    for line in text.lines() {
        let trimmed = line.trim();
        let trimmed = trimmed.strip_prefix("- ").unwrap_or(trimmed);
        if let Some(rest) = trimmed.strip_prefix(prefix.as_str()) {
            return Some(rest.trim().to_string());
        }
    }
    None
}

/// Every `key: value` line, list items included. rgy names five nested
/// installer files, so a check that read only the first would miss four.
fn fields(text: &str, key: &str) -> Vec<String> {
    let prefix = format!("{key}: ");
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        let trimmed = trimmed.strip_prefix("- ").unwrap_or(trimmed);
        if let Some(rest) = trimmed.strip_prefix(prefix.as_str()) {
            out.push(rest.trim().to_string());
        }
    }
    out
}

#[test]
fn the_manifests_name_one_release_and_the_files_in_its_archive() {
    let version_manifest = manifest("danalec.rgy-search.yaml");
    let installer = manifest("danalec.rgy-search.installer.yaml");
    let locale = manifest("danalec.rgy-search.locale.en-US.yaml");

    let version = field(&version_manifest, "PackageVersion").expect("a version");
    assert_eq!(
        field(&installer, "PackageVersion").as_deref(),
        Some(version.as_str()),
        "the installer names another version than the version manifest: {version}"
    );
    assert_eq!(
        field(&locale, "PackageVersion").as_deref(),
        Some(version.as_str()),
        "the locale names another version than the version manifest: {version}"
    );

    // rgy tags its releases `rgy-v<version>`.
    let tag = format!("rgy-v{version}");

    // The archive the installer downloads, and the page the notes point at,
    // are the same release the version names.
    let url = field(&installer, "InstallerUrl").expect("the installer names the archive");
    assert!(
        url.ends_with(&format!("releases/download/{tag}/{tag}-x86_64-windows.zip")),
        "the installer downloads {url}, which is not the archive of {tag}"
    );

    let notes = field(&locale, "ReleaseNotesUrl").expect("the locale names the release notes");
    assert!(
        notes.ends_with(&format!("releases/tag/{tag}")),
        "the locale names the release notes at {notes}, which is not the release {tag}"
    );

    // The five executables the Windows zip holds at its root: the four
    // product binaries and the `rgy` alias. The paths are flat and carry no
    // version, so unlike a version-numbered directory there is no stale
    // number to hide in them -- what is checked is that the manifest names
    // the whole product the installers copy.
    let mut nested = fields(&installer, "RelativeFilePath");
    nested.sort();
    let mut expected = vec![
        "rgy-autostart.exe".to_string(),
        "rgy.exe".to_string(),
        "rgxy.exe".to_string(),
        "searchctl.exe".to_string(),
        "searchd.exe".to_string(),
    ];
    expected.sort();
    assert_eq!(
        nested, expected,
        "the installer's nested files do not name the product's binaries"
    );
}
