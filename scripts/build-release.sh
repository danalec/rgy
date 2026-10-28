#!/usr/bin/env bash
# build-release.sh -- build the shippable rgy binaries and pack them.
#
# The Linux/macOS counterpart of scripts/build-release.ps1.
#
#   bash scripts/build-release.sh                      # the host's archive
#   bash scripts/build-release.sh --skip-tests         # iterate faster
#   bash scripts/build-release.sh --version 0.1.0
#   bash scripts/build-release.sh --target aarch64-unknown-linux-gnu
#   bash scripts/build-release.sh --variant znver4
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
# --- the platforms ---------------------------------------------------------
#
# The release covers the same ground the upstream rgx covers, and it is
# built the same way: one portable binary for every CPU, plus the tuned
# ones for the CPUs that can run them. The upstream's release is the
# reference; what differs here is the name of the portable archive, which
# the installers already point at.
#
#   target                          asset                    flavour
#   x86_64-pc-windows-msvc          x86_64-windows.zip       portable
#   x86_64-pc-windows-msvc          avx2-windows.zip         AVX2
#   x86_64-pc-windows-msvc          znver4-windows.zip       AVX-512, Zen 4
#   aarch64-pc-windows-msvc          aarch64-windows.zip      portable
#   x86_64-unknown-linux-gnu        x86_64-linux.tar.gz      portable
#   x86_64-unknown-linux-gnu        avx2-linux.tar.gz        AVX2
#   x86_64-unknown-linux-gnu        znver4-linux.tar.gz      AVX-512, Zen 4
#   x86_64-unknown-linux-musl       x86_64-linux-musl.tar.gz portable
#   aarch64-unknown-linux-gnu       aarch64-linux.tar.gz     portable
#   aarch64-apple-darwin            aarch64-apple-darwin.tar.gz  portable
#   x86_64-apple-darwin             x86_64-apple-darwin.tar.gz   portable
#
# The three x86_64 portable names are the ones the installers ask for
# (install.sh wants x86_64-linux.tar.gz, install.ps1 wants
# x86_64-windows.zip) and the ones the published 0.1.0 carries, so they do
# not move: a rename here would point the installers at an archive that
# does not exist.
#
# The AVX2 and the Zen 4 builds are for the CPUs that can run them and
# nobody else: the Zen 4 one starts with an illegal instruction on a
# baseline x86_64 CPU, and the AVX2 one on a CPU older than Haswell.
#
# What it produces (.dist/):
#
#   rgy-v<version>-<asset>.<zip|tar.gz>   the binaries and the documents
#   SHA256SUMS                            the digests, in sha256sum(1) format
#
# ONE DELIBERATE DIFFERENCE FROM THE WINDOWS TWIN: the archive format.
# Linux and macOS get a .tar.gz; Windows gets a .zip, because winget
# installs a zip and nothing else. The file list inside is the same on every
# platform, the log lines are the same, and the exit code is the same --
# see AGENTS.md on the twins.
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
target=""
variant="portable"

while [ $# -gt 0 ]; do
    case "$1" in
        --skip-tests) skip_tests=1; shift ;;
        --version) version=${2:-}; shift 2 ;;
        --target) target=${2:-}; shift 2 ;;
        --variant) variant=${2:-}; shift 2 ;;
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

# --- 2. the target and the flavour -----------------------------------------
#
# The host triple is the default, so the script keeps doing what it did
# before this option existed. The asset name is a property of the target
# and the flavour, never of the machine that happens to run the build: two
# machines building the same target produce the same archive name.
host=$(rustc -vV | sed -n 's/^host: //p')
[ -n "$target" ] || target="$host"

case "$variant" in
    portable) rustflags="" ;;
    avx2) rustflags="-C target-cpu=x86-64-v3" ;;
    znver4) rustflags="-C target-cpu=znver4" ;;
    *) echo "unknown variant: $variant (portable, avx2, znver4)" >&2; exit 2 ;;
esac

# The asset name of a portable build, per target.
asset_of() {
    case "$1" in
        x86_64-pc-windows-msvc) echo "x86_64-windows" ;;
        aarch64-pc-windows-msvc) echo "aarch64-windows" ;;
        x86_64-unknown-linux-gnu) echo "x86_64-linux" ;;
        x86_64-unknown-linux-musl) echo "x86_64-linux-musl" ;;
        aarch64-unknown-linux-gnu) echo "aarch64-linux" ;;
        aarch64-apple-darwin) echo "aarch64-apple-darwin" ;;
        x86_64-apple-darwin) echo "x86_64-apple-darwin" ;;
        *) echo "unknown target: $1" >&2; return 1 ;;
    esac
}

asset=$(asset_of "$target") || exit 2

# The tuned builds replace the architecture in the name with the flavour:
# znver4-windows is a Windows x86_64 build for Zen 4, and avx2-linux is
# the Linux x86_64 one for the CPUs that have AVX2 and not AVX-512. Only
# the x86_64 targets have a tuned build; the others are tuned already.
if [ "$variant" != "portable" ]; then
    case "$asset" in
        x86_64-*)
            asset="$variant-${asset#x86_64-}" ;;
        *)
            echo "the $variant build needs an x86_64 target, and $target is not one" >&2
            exit 2 ;;
    esac
fi

echo "rgy release build: $version ($asset)"
echo "target: $target"
echo "variant: $variant"

# --- 3. the build -----------------------------------------------------------
# The build comes before the tests, not after them. The e2e harness resolves
# the bins by path (target/release) and refuses to run against a binary
# older than its own sources -- which is the guard working, not a bug.
# `cargo test` builds the bins into deps/ but does not uplift them to
# target/release, so a test before a build would either exercise the
# previous release's binary or stop the release for a reason that looks
# like a test failure.
# The host's build keeps the --target off, so its binaries land in
# target/release the way they always have: the harness, the benchmark
# write-up and the mem-peak recipe all name that path. A build for
# another target lands in target/<triple>/release, and the archive is
# packed from there.
if [ "$target" = "$host" ]; then
    build_cmd="cargo build --release --locked --workspace"
    exe_dir="$repo/target/release"
else
    build_cmd="cargo build --release --locked --workspace --target $target"
    exe_dir="$repo/target/$target/release"
fi
echo "build: $build_cmd"
if [ -n "$rustflags" ]; then
    RUSTFLAGS="$rustflags" $build_cmd || { echo "the build failed" >&2; exit 1; }
else
    $build_cmd || { echo "the build failed" >&2; exit 1; }
fi

# --- 4. the gate ------------------------------------------------------------
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

# --- 5. the smoke -----------------------------------------------------------
#
# A binary that starts is not a binary that works. The smoke runs the two
# halves of the product against the repository's own sources: the name half
# (a fuzzy name query) and the content half (a literal search inside the
# files). Both must answer with a non-empty count.
#
# A binary built for another target cannot run here, so the smoke is for
# the host's build only: a cross build is checked by the suite on the
# machine that owns that CPU.
#
# The index goes to a file under target\, not to the user's cache: a release
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

if [ "$target" = "$host" ]; then
    smoke "name query over crates/" "walk ext:rs" || exit 1
    smoke "content query over crates/" "grep:Listing" || exit 1
else
    echo "smoke: skipped (a binary for $target does not run here)"
fi

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

archive_name="rgy-v$version-$asset.tar.gz"
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
awk '!seen[$2]++' "$tmp" | sort -k2 > "$sums"
rm -f "$tmp"
echo "checksum: .dist/SHA256SUMS ($sha  $archive_name)"

echo ""
echo "built $archive_name"
