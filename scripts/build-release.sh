#!/usr/bin/env bash
# build-release.sh -- build the shippable rgy binaries and pack them.
#
# The Linux/macOS counterpart of scripts/build-release.ps1.
#
#   bash scripts/build-release.sh                # clippy gate + tests + the archive
#   bash scripts/build-release.sh --skip-tests   # iterate faster
#   bash scripts/build-release.sh --version 0.1.0
#
# Unless --skip-tests, the build fails on any clippy warning
# (`cargo clippy --release --locked --workspace --all-targets -- -D warnings`)
# and on any failing test. The workspace stays warning-free by policy, and a
# release is not the moment to relax that.
#
# Every cargo call carries `--locked`: the lockfile is what this repository
# says it builds with, and a release that silently updates it is a release
# of something the repository does not describe.
#
# What it produces (.dist/):
#
#   rgy-v<version>-<platform>.tar.gz   the binaries and the documents
#   SHA256SUMS                         the digests, in sha256sum(1) format
#
# The platform tag comes from the rustc host triple, so the same script on
# three machines produces three archives and never overwrites another.
#
# ONE DELIBERATE DIFFERENCE FROM THE WINDOWS TWIN: the archive format.
# Linux and macOS get a .tar.gz; Windows gets a .zip, because winget
# installs a zip and nothing else. The file list inside is the same on
# every platform, the log lines are the same, and the exit code is the
# same -- see AGENTS.md on the twins.
#
# The version comes from the binary crates' manifests, and all four have to
# agree: a release whose binaries disagree about their version is a release
# nobody can describe.
set -u

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/.." && pwd)
cd "$repo"

skip_tests=0
version=""

while [ $# -gt 0 ]; do
    case "$1" in
        --skip-tests) skip_tests=1; shift ;;
        --version) version=${2:-}; shift 2 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done

command -v cargo > /dev/null 2>&1 || {
    echo "cargo not found; install Rust: https://rustup.rs" >&2
    exit 1
}

# --- 1. the version ---------------------------------------------------------
#
# The workspace root is virtual: it has no version of its own. The crates
# that carry a version have to agree -- and `rgy-autostart` is one of them,
# because the release ships it: the archive carries searchctl, searchd and
# rgy-autostart, so a version drift in the autostart crate is a release
# carrying three binaries that do not agree about which release they are.
read_manifest_version() {
    sed -n 's/^version = "\(.*\)"/\1/p' "$1" | head -1
}

manifests="bins/searchctl/Cargo.toml bins/searchd/Cargo.toml bins/search-ui/Cargo.toml bins/zen-rs/Cargo.toml crates/autostart/Cargo.toml"
if [ -z "$version" ]; then
    version=$(read_manifest_version bins/searchctl/Cargo.toml)
fi
[ -n "$version" ] || { echo "no version in bins/searchctl/Cargo.toml" >&2; exit 1; }

for m in $manifests; do
    v=$(read_manifest_version "$m")
    if [ "$v" != "$version" ]; then
        echo "$m says version $v, but the release is $version" >&2
        echo "the crates must agree about their version" >&2
        exit 1
    fi
done

# --- 2. the platform tag ----------------------------------------------------
#
# From the rustc host triple, because that is the thing that says what the
# binaries will run on: x86_64-pc-windows-msvc -> x86_64-windows,
# x86_64-unknown-linux-gnu -> x86_64-linux, aarch64-apple-darwin ->
# aarch64-apple-darwin.
host=$(rustc -vV | sed -n 's/^host: //p')
case "$host" in
    *-windows*) platform=$(printf '%s' "$host" | cut -d- -f1)-windows ;;
    *-linux*) platform=$(printf '%s' "$host" | cut -d- -f1)-linux ;;
    *-apple-darwin*) platform="$host" ;;
    *) platform="$host" ;;
esac

echo "rgy release build: $version ($platform)"

# --- 3. the gate ------------------------------------------------------------
if [ "$skip_tests" -eq 0 ]; then
    echo "gate: clippy, then the test suite"
    cargo clippy --release --locked --workspace --all-targets -- -D warnings || {
        echo "clippy warnings found; the workspace must stay warning-free" >&2
        exit 1
    }
    cargo test --release --locked --workspace || { echo "tests failed" >&2; exit 1; }
else
    echo "gate: skipped (--skip-tests)"
fi

# --- 4. the build -----------------------------------------------------------
echo "build: cargo build --release --locked --workspace"
cargo build --release --locked --workspace || { echo "the build failed" >&2; exit 1; }

exe_dir="$repo/target/release"

# --- 5. the smoke -----------------------------------------------------------
#
# A binary that starts is not a binary that works. The smoke runs the two
# halves of the product against the repository's own sources: the name half
# (a fuzzy name query) and the content half (a literal search inside the
# files). Both must answer with a non-empty count.
#
# The index goes to a file under target/, not to the user's cache: a release
# build has no business writing into somebody's home directory.
smoke_index_dir="$repo/target/release-smoke"
mkdir -p "$smoke_index_dir"

smoke() {
    local label="$1" query="$2" out count
    out=$("$exe_dir/searchctl" --root "$repo/crates" --index "$smoke_index_dir/rgy.idx" --count "$query" 2> /dev/null) || {
        echo "smoke: $label failed (exit $?)" >&2
        return 1
    }
    count=$(printf '%s' "$out" | tr -dc '0-9')
    if [ -z "$count" ] || [ "$count" -eq 0 ]; then
        echo "smoke: $label answered with no hits" >&2
        return 1
    fi
    echo "smoke: $label -> $count hits"
    return 0
}

smoke "name query over crates/" "walk ext:rs" || exit 1
smoke "content query over crates/" "grep:Listing" || exit 1

# --- 6. the archive ---------------------------------------------------------
#
# Flat layout: the binaries and the documents at the root of the archive, so
# that unzipping it next to a PATH directory is the whole install.
out="$repo/.dist"
mkdir -p "$out"
stage="$repo/target/release-stage"
rm -rf "$stage"
mkdir -p "$stage"

cp "$exe_dir/searchctl" "$stage/"
cp "$exe_dir/searchd" "$stage/"
cp "$exe_dir/rgy-autostart" "$stage/"
for doc in README.md MANUAL.md FORMATS.md COPYING NOTICE LICENSE-APACHE LICENSE-MIT LICENSE-MIT-FSEARCH; do
    [ -f "$repo/$doc" ] && cp "$repo/$doc" "$stage/"
done

archive_name="rgy-v$version-$platform.tar.gz"
archive="$out/$archive_name"
tar -czf "$archive" -C "$stage" searchctl searchd rgy-autostart README.md MANUAL.md FORMATS.md COPYING NOTICE LICENSE-APACHE LICENSE-MIT LICENSE-MIT-FSEARCH
echo "packed: .dist/$archive_name"

# --- 7. the digest ----------------------------------------------------------
#
# SHA256SUMS holds every platform's digest, not just this one's: three
# machines build three archives into the same directory, and the release
# publishes them together.
sums="$out/SHA256SUMS"
tmp="$sums.tmp"
: > "$tmp"
if [ -f "$sums" ]; then
    grep -v "  $archive_name\$" "$sums" >> "$tmp" || true
fi
sha=$(sha256sum "$archive" | cut -d' ' -f1)
printf '%s  %s\n' "$sha" "$archive_name" >> "$tmp"
# One line per asset, and the first one wins: a digests file that names the
# same archive twice is a file nobody can read -- the reader takes the
# digest and finds two of them.
awk '!seen[$2]++' "$tmp" | sort -k2 > "$sums"
rm -f "$tmp"
echo "checksum: .dist/SHA256SUMS ($sha  $archive_name)"

echo ""
echo "built $archive_name"
