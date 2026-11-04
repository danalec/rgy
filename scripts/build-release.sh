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
#   x86_64-unknown-freebsd          x86_64-freebsd.tar.gz        portable
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
# platform, the log lines are the same, and the exit code is the same.
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

# The options, read by the helper the two twins share: the long spelling
# and the PowerShell one are the same option, and an option neither of
# them knows is an error on both platforms -- not a silent "do the real
# thing", which in this directory means building an archive that a release
# then publishes.
. "$here/lib/options.sh"
. "$here/lib/digest.sh"
. "$here/lib/version.sh"
rgy_read_options "skiptests=skip_tests" \
                 "version=version target=target variant=variant" "" "$@"

command -v cargo > /dev/null 2>&1 || {
    echo "cargo not found; install Rust: https://rustup.rs" >&2
    exit 1
}

# --- 1. the version ---------------------------------------------------------
#
# The workspace root is virtual: it has no version of its own. The crates
# that carry a version have to agree -- and every one of them has to, not
# only the four the release ships: the archive carries searchctl, searchd,
# rgy-autostart and rgxy, so a version drift in one of those crates is a
# release carrying binaries that do not agree about which release they are.
#
# Every manifest this repository owns is checked, and not only the shipped
# ones. A crate left at the old number is a crate the resolver can take
# from the registry instead of from this tree, and `cargo build --locked`
# -- which is the release build itself -- then refuses the lockfile. The
# check names the manifest AND the line, because a bump that missed one
# line of twenty manifests is found by the reader, not by the script.
#
# The manifests are the workspace's own: the crates this repository
# publishes and the binaries it ships -- and the vendored trees
# (crates/rgx-*, crates/rar-codec) too, because this repository publishes
# them, under this repository's names, at this repository's version. The e2e
# harness is not published either, and keeps its 0.0.0.
#
# The version and the line it stands on are read the same way the PowerShell
# twin reads them: the [package] table is the first table in every manifest
# of ours, so the first `version = "..."` line is the one. The quoted value
# ends at the first quote after it -- a greedy read would swallow the rest
# of the line, and the two twins would then read two different versions
# out of the same manifest.
read_manifest_version() {
    awk 'match($0, /^version[ \t]*=[ \t]*"[^"]*"/) {
             v = substr($0, RSTART, RLENGTH)
             sub(/^[^"]*"/, "", v)
             sub(/"$/, "", v)
             print v
             exit
         }' "$1"
}

# The line the version stands on, so a drift is found by its place and not
# only by its number.
manifest_version_line() {
    awk '/^version[ \t]*=[ \t]*"[^"]*"/ { print NR; exit }' "$1"
}

# Every manifest this repository owns. The vendored trees are in it: this
# repository publishes them, at this repository's version. The harness is
# not published either.
manifests=$(ls -1 crates/*/Cargo.toml bins/*/Cargo.toml 2>/dev/null \
    | LC_ALL=C sort)
if [ -z "$version" ]; then
    version=$(read_manifest_version bins/searchctl/Cargo.toml)
fi
[ -n "$version" ] || { echo "no version in bins/searchctl/Cargo.toml" >&2; exit 1; }

# The page the tree carries says which release of the line this is.
#
# The manifests hold three numbers; the release may carry a fourth, the fix
# count of the line, and the fourth is in the page's name. Reading the manifest
# alone picked the three-number page and the archive step stopped on a page the
# tree does not carry -- on every row of the CI -- while the release's own page
# was there under the name the tag has.
version=$(rgy_release_version "$repo" "$version") || exit 1

# The release's number and the crates' number. The registry takes a version
# of three numbers and nothing else, and a release line that will carry
# fixes for a while wants a fourth -- the fix count. With four numbers the
# release carries all of them and every manifest carries the first three,
# the same rule scripts/bump-version.py bumps by, so the agreement checks
# below read the three the manifests hold and the archive carries the four
# the tag is made of.
crate_version=$(printf '%s\n' "$version" | cut -d. -f1-3)

for m in $manifests; do
    v=$(read_manifest_version "$m")
    if [ "$v" != "$crate_version" ]; then
        echo "$m:$(manifest_version_line "$m") says version $v, but the release is $version" >&2
        echo "the crates must agree about their version" >&2
        exit 1
    fi
done

# Every requirement on one of our crates has to ask for the version the
# release carries. A dependency that still asks for the old one breaks
# `cargo build --locked`, and that is the release build itself.
#
# A requirement is ours when the entry names one of our packages: the key
# is one of ours (`rgy-...`), or the entry names the package with
# `package = "rgy-vendored-..."` -- the same rule scripts/bump-version.py
# bumps by, so the check reads the lines the bump writes, and a requirement
# the bump would not have touched is a requirement the check does not read
# either. The key of a renamed dependency is the upstream's, so the key
# alone does not say whose requirement the line carries; the package does.
# The requirement of a crate that is not ours is not ours to write -- it
# names somebody else's version, and it stays where it was put.
#
# A dependency entry can span lines, and the requirement can be on one of
# them, so the entry is read to its closing brace: a requirement the check
# cannot see is a requirement it cannot vouch for.
#
# The findings come out as "line:dependency:requirement", and the shell
# turns the first one into the sentence the reader reads.
for m in $manifests; do
    for finding in $(awk -v release="$crate_version" '
        function req_of(line,   rest, v) {
            if (!match(line, /version[ \t]*=[ \t]*/)) return ""
            rest = substr(line, RSTART + RLENGTH)
            if (!match(rest, /^"[^"]*"/)) return ""
            v = substr(rest, 2, RLENGTH - 2)
            return v
        }
        function pkg_of(line,   rest, v) {
            if (!match(line, /package[ \t]*=[ \t]*/)) return ""
            rest = substr(line, RSTART + RLENGTH)
            if (!match(rest, /^"[^"]*"/)) return ""
            v = substr(rest, 2, RLENGTH - 2)
            return v
        }
        { lines[NR] = $0 }
        END {
            dep = ""
            for (n = 1; n <= NR; n++) {
                line = lines[n]
                if (dep == "") {
                    if (!match(line, /^[ \t]*[A-Za-z0-9_-]+[ \t]*=/)) continue
                    key = substr(line, RSTART, RLENGTH)
                    sub(/[ \t]*=$/, "", key)
                    sub(/^[ \t]*/, "", key)
                    pkg = pkg_of(line)
                    if (key !~ /^rgy-/ && pkg !~ /^rgy-vendored-/) continue
                    dep = (pkg != "") ? pkg : key
                    req = req_of(line)
                    if (req != "") {
                        if (req != release && req != "^" release) print n ":" dep ":" req
                        dep = ""
                    } else if (index(line, "{") > 0 && index(line, "}") == 0) {
                        # The entry spans lines: the requirement is on one of them.
                    } else {
                        dep = ""
                    }
                    continue
                }
                req = req_of(line)
                if (req != "") {
                    if (req != release && req != "^" release) print n ":" dep ":" req
                    dep = ""
                } else if (index(line, "}") > 0) {
                    dep = ""
                }
            }
        }
    ' "$m"); do
        line=${finding%%:*}
        rest=${finding#*:}
        dep=${rest%%:*}
        req=${rest#*:}
        echo "$m:$line asks for $dep $req where the release is $version" >&2
        echo "the dependency lines have to be bumped with the manifests" >&2
        exit 1
    done
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
        x86_64-unknown-freebsd) echo "x86_64-freebsd" ;;
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
# The host's PLAIN build keeps the --target off, so its binaries land in
# target/release the way they always have: the harness, the benchmark
# write-up and the mem-peak recipe all name that path. A build for
# another target lands in target/<triple>/release, and the archive is
# packed from there.
#
# The TUNED builds are the exception, and the reason is the build
# scripts. A build script is a program that runs on THIS machine, and
# cargo compiles it for the target it is building: a script compiled for
# Zen 4 dies with an illegal instruction on a runner that has no
# AVX-512, and the release stops with a message about mimalloc, which
# says nothing about the tree. Naming the target is what tells cargo
# that the build scripts are the host's and not the target's, and the
# flags then reach the target's own artifacts and not the scripts. A
# tuned build therefore lands in target/<triple>/release like any other
# build for a named target, and the plain build keeps target/release.
if [ "$target" = "$host" ] && [ -z "$rustflags" ]; then
    build_cmd="cargo build --release --locked --workspace"
    exe_dir="$repo/target/release"
else
    build_cmd="cargo build --release --locked --workspace --target $target"
    exe_dir="$repo/target/$target/release"
fi
echo "build: $build_cmd"
if [ -n "$rustflags" ]; then
    # The per-target flags, and not RUSTFLAGS: the per-target ones are
    # the flags of the target's own artifacts, and RUSTFLAGS reaches
    # every compilation cargo does. The name is the triple, uppercased,
    # with the dashes turned into underscores: x86_64-unknown-linux-gnu
    # is CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS.
    rustflags_var="CARGO_TARGET_$(printf '%s' "$target" | tr 'a-z-' 'A-Z_')_RUSTFLAGS"
    echo "flags: $rustflags_var=$rustflags"
    env "$rustflags_var=$rustflags" $build_cmd || { echo "the build failed" >&2; exit 1; }
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
# A binary built for another target cannot run on the machine that built
# it, so the smoke is for the host's build only -- with two exceptions the
# CI covers: the musl build is static, so it starts on the runner that
# built it, and the Intel Mac's build is checked for the architecture it
# was built for, because there is no Intel Mac in the matrix.
#
# The index goes to a file under target\, not to the user's cache: a release
# build has no business writing into somebody's home directory.
smoke_index_dir="$repo/target/release-smoke"
mkdir -p "$smoke_index_dir"

smoke() {
    local label="$1" query="$2" out count err
    # The stderr of the binary goes to a file next to the index, and the
    # file is printed when the smoke fails: a smoke that says only
    # "failed (exit 1)" says nothing about why, and the reason is the
    # whole of the report.
    err="$smoke_index_dir/stderr.txt"
    out=$("$exe_dir/searchctl" --root "$repo/crates" --index "$smoke_index_dir/rgy.idx" --count "$query" 2> "$err") || {
        echo "smoke: $label failed (exit $?)" >&2
        cat "$err" >&2
        return 1
    }
    # The count is the first number in the answer, and the PowerShell twin
    # reads it the same way. `tr -dc '0-9'` kept every digit of every number
    # in the answer, so an answer carrying two numbers was counted as their
    # digits run together -- a count neither twin can vouch for.
    count=$(printf '%s' "$out" | awk '{ if (match($0, /[0-9]+/)) { print substr($0, RSTART, RLENGTH); exit } }')
    if [ -z "$count" ] || [ "$count" -eq 0 ]; then
        echo "smoke: $label answered with no hits" >&2
        return 1
    fi
    echo "smoke: $label -> $count hits"
    return 0
}

# The instructions the machine has.
#
# The smoke runs the binary this machine just built, so it can only run
# where the CPU has the instructions the build asked for. The AVX2 build
# asks for AVX2 and the Zen 4 build asks for AVX-512, and a machine
# without them answers with SIGILL: the smoke would fail on a runner that
# never had the instructions, and a red run that says nothing about the
# tree is worse than no run.
#
# Linux keeps the CPU's features in /proc/cpuinfo; macOS keeps them in
# the kernel's answer to sysctl.
cpu_has_avx2() {
    if [ -r /proc/cpuinfo ]; then
        grep -q 'avx2' /proc/cpuinfo 2> /dev/null
    else
        sysctl -n machdep.cpu.leaf7_features 2> /dev/null | grep -q 'AVX2'
    fi
}

cpu_has_avx512() {
    if [ -r /proc/cpuinfo ]; then
        grep -q 'avx512' /proc/cpuinfo 2> /dev/null
    else
        sysctl -n machdep.cpu.leaf7_features 2> /dev/null | grep -q 'AVX512'
    fi
}

if [ "$target" = "$host" ]; then
    if [ "$variant" = "znver4" ] && ! cpu_has_avx512; then
        echo "smoke: skipped (this machine has no AVX-512, and the Zen 4 build asks for it)"
    elif [ "$variant" = "avx2" ] && ! cpu_has_avx2; then
        echo "smoke: skipped (this machine has no AVX2, and the AVX2 build asks for it)"
    else
        smoke "name query over crates/" "walk ext:rs" || exit 1
        smoke "content query over crates/" "grep:Listing" || exit 1
    fi
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
# The launcher: the window the user opens with two Control presses. It
# travels with the release because a launcher that is not installed is a
# feature nobody can use.
cp "$exe_dir/rgxy" "$stage/"
# The command the project is named after: a link to the one searchctl.
#
# It travels with the release, so every path that unpacks the archive --
# the one-line installer, a package manager, a tarball by hand -- gives
# the user the name the project has. A copy of the binary would be a
# second binary to keep in step with the first.
ln -sf searchctl "$stage/rgy"
# The documents the release carries, in ONE list: the build copies them into
# the archive, and the build writes down their digests beside it. A list that
# lives in two places is a list that drifts.
documents="README.md MANUAL.md FORMATS.md COPYING NOTICE LICENSE-APACHE LICENSE-MIT LICENSE-MIT-FSEARCH"
for doc in $documents; do
    [ -f "$repo/$doc" ] && cp "$repo/$doc" "$stage/"
done

# Every document the release promises is in the stage, or the build stops.
#
# The copy above is conditional -- a document the tree does not have is not
# packed -- and the archive's member list names the documents either way, so
# a tree without one produced an archive without it. And tar's complaint was
# ignored: this script says "set -u" and not "set -e", so the build carried
# on and announced "packed" over an archive that was missing a licence file.
#
# A release archive without a licence file is a release whose licence story
# is broken, and it was announced as if it were whole.
for doc in $documents; do
    if [ ! -f "$stage/$doc" ]; then
        echo "the release carries $doc and this tree does not have it: the archive would not" >&2
        echo "  copy the document into the tree, or take it out of the release" >&2
        exit 1
    fi
done

# The picture the README shows travels with it.
#
# The archive carries the README, and the README points at the picture by
# a relative path, so an archive without the picture has a README with a
# hole in it: the link is broken exactly where the reader is offline.
# The path inside the archive is the one the README uses, so the link
# works where the README is.
# The pictures and the clips the README points at.
#
# The archive carries the README, and the README points at these by a
# relative path, so an archive without them has a README with holes in it:
# the links are broken exactly where the reader is offline. The path inside
# the archive is the one the README uses, so the links work where the README
# is.
#
# ONE list, and three parts of the build read it: the copy below, the
# archive's member list, and the digests the publish step compares with the
# tree. A list that lives in three places is a list that drifts.
media="docs/screenshots/rgxy.png docs/screenshots/gui-filters.png docs/videos/gui-filters.mp4 docs/videos/term-tui.mp4"
for media_file in $media; do
    if [ ! -f "$repo/$media_file" ]; then
        echo "the release carries $media_file and this tree does not have it: the archive would not" >&2
        echo "  the README points at it, and an archive without it is a README with a hole in it" >&2
        exit 1
    fi
    mkdir -p "$stage/$(dirname "$media_file")"
    cp "$repo/$media_file" "$stage/$media_file"
done

# The page of the release the README calls the changelog.
#
# The README points at it by a relative path, and the name of that page
# carries the version -- so the page that travels with the archive is the
# page of the release being built, and not the page of an older one. A tree
# without it has a README with a hole in it.
release_page="docs/release-$version.md"
if [ ! -f "$repo/$release_page" ]; then
    echo "the release carries $release_page and this tree does not have it: the archive would not" >&2
    echo "  the README points at it, and an archive without it is a README with a hole in it" >&2
    exit 1
fi
mkdir -p "$stage/docs"
cp "$repo/$release_page" "$stage/$release_page"

# The benchmark folder the README points at.
#
# The README links the folder itself and not one file of it, so the folder
# has to be inside the archive -- and the rows the README quotes live in it.
benchmark_dir="benchmark"
if [ ! -d "$repo/$benchmark_dir" ]; then
    echo "the release carries $benchmark_dir/ and this tree does not have it: the archive would not" >&2
    echo "  the README points at it, and an archive without it is a README with a hole in it" >&2
    exit 1
fi
cp -R "$repo/$benchmark_dir" "$stage/"

# Every relative link in every document that travels resolves inside the stage.
#
# The README and the manual are what the reader has, and they point at their
# neighbours by relative paths: the picture, the manual, the benchmark
# folder, the page of the release. A link that does not resolve inside the
# archive is a link that works on the forge and nowhere else -- and the
# reader of the archive is the one who cannot follow it.
#
# Every document in the stage is checked, and not only the README and the
# manual: the benchmark folder travels too, and its own documents point at
# each other. The order is the ordinal one, so the two twins read the same
# document first and say the same thing about it.
#
# The links are read the way the reader reads them: the text between the
# parentheses. The absolute ones (http:, https:, mailto:) are the reader's
# own business, and the pure anchors (#...) stay inside the document.
for doc_path in $(cd "$stage" && find . -type f -name '*.md' | sed 's|^\./||' | LC_ALL=C sort); do
    # A relative link is resolved against the folder of the document that
    # carries it, and not against the root of the archive: a link in
    # benchmark/README.md to baseline-names.md is the file beside it.
    doc_dir=$(dirname "$doc_path")
    [ "$doc_dir" = "." ] && doc_dir=""
    links=$(grep -o ']([^)]*)' "$stage/$doc_path" | sed -e 's/^](//' -e 's/)$//')
    while IFS= read -r link; do
        [ -n "$link" ] || continue
        case "$link" in
            \#*) continue ;;
            //*) continue ;;
        esac
        # A scheme means a URL and not a path: http:, https:, mailto:.
        if printf '%s' "$link" | grep -q '^[A-Za-z][A-Za-z0-9+.-]*:'; then
            continue
        fi
        # The path: what is before the anchor, and what is before the title.
        path=${link%%#*}
        if [ "${path#<}" != "$path" ]; then
            path=${path#<}
            path=${path%%>*}
        else
            path=${path%% *}
        fi
        # The trailing slash is how the README spells the folder, and not part
        # of the folder's name: the folder is looked up by its name.
        check=$(printf '%s' "$path" | sed 's:/*$::')
        [ -n "$check" ] || continue
        if [ -n "$doc_dir" ]; then
            target="$doc_dir/$check"
        else
            target="$check"
        fi
        if [ ! -e "$stage/$target" ]; then
            echo "the $doc_path points at $link and the archive does not carry it: the link is broken exactly where the reader is offline" >&2
            exit 1
        fi
    done <<< "$links"
done

archive_name="rgy-v$version-$asset.tar.gz"
archive="$out/$archive_name"
files="searchctl searchd rgy-autostart rgxy rgy README.md MANUAL.md FORMATS.md COPYING NOTICE LICENSE-APACHE LICENSE-MIT LICENSE-MIT-FSEARCH"
files="$files $media"
files="$files benchmark $release_page"

# Every member of the list is in the stage, or the archive is packed
# without it -- and tar's complaint is not a sentence the reader can act on.
for member in $files; do
    if [ ! -e "$stage/$member" ]; then
        echo "the archive carries $member and the stage does not have it: the archive would not" >&2
        echo "  carry what this build promised" >&2
        exit 1
    fi
done

if ! tar -czf "$archive" -C "$stage" $files; then
    echo "the archive was not packed: tar failed, and a release without its archive is no release" >&2
    exit 1
fi
echo "packed: .dist/$archive_name"

# The digests of the documents that went into the archive.
#
# The archive's own digest proves the archive is the one this build made. It
# says nothing about what is INSIDE it: a release built before the README's
# last edit carries the older README, and its digest matches perfectly.
#
# So the build writes down what it packed -- the digest of every document it
# copied from the tree -- beside the archive, and the publish step reads it
# back and compares each digest with the tree's. The digests are of the
# tree's files, and the tree's files are the same on every machine: the text
# files are LF (the .gitattributes say so) and the picture is binary. So the
# file this build wrote here is the file the other platform's build wrote
# there.
docs_dir="$out/documents"
mkdir -p "$docs_dir"
docs_file="$docs_dir/$archive_name.sha256"
docs_tmp="$docs_file.tmp"
: > "$docs_tmp"
for doc in $documents; do
    [ -f "$repo/$doc" ] || continue
    printf '%s  %s\n' "$(rgy_sha256 "$repo/$doc")" "$doc" >> "$docs_tmp"
done
for media_file in $media; do
    printf '%s  %s\n' "$(rgy_sha256 "$repo/$media_file")" "$media_file" >> "$docs_tmp"
done
# The page of the release and the benchmark folder travel with the archive,
# so the record names them too: a record that lists only the documents is a
# record of a part of the archive, and the publish step compares the record
# with the tree -- so a stale page, or a benchmark row of an older run,
# would pass a check that is about the whole.
printf '%s  %s\n' "$(rgy_sha256 "$repo/$release_page")" "$release_page" >> "$docs_tmp"
for bench_file in $(cd "$repo/$benchmark_dir" && find . -type f | sed 's|^\./||'); do
    printf '%s  %s\n' "$(rgy_sha256 "$repo/$benchmark_dir/$bench_file")" "$benchmark_dir/$bench_file" >> "$docs_tmp"
done
# The ordinal order, so the two machines write the same bytes.
LC_ALL=C sort -k2 "$docs_tmp" > "$docs_file"
rm -f "$docs_tmp"
docs_count=$(wc -l < "$docs_file" | tr -d '[:space:]')
echo "documents: .dist/documents/$archive_name.sha256 ($docs_count files)"

# --- 7. the digest ----------------------------------------------------------
#
# SHA256SUMS holds every platform's digest, not just this one's: three
# machines build three archives into the same directory, and the release
# publishes them together.
sums="$out/SHA256SUMS"
tmp="$sums.tmp"
: > "$tmp"
if [ -f "$sums" ]; then
    # The carriage return: the file is shared with the Windows twin, and a
    # line written there can end with one. The name is the second field, so
    # the return would be part of it.
    # The digests file belongs to ONE release: the rows of the archives
    # this release carries, and nothing else. A row left over from an older
    # build in the same .dist/ names an archive the release does not
    # publish, and `sha256sum -c` then fails on a file the user never got.
    version_re=$(printf '%s' "$version" | sed 's/[.[\*^$\\]/\\&/g')
    grep "  rgy-v$version_re-" "$sums" | tr -d '\r' | grep -v "  $archive_name[[:space:]]*\$" >> "$tmp" || true
fi
sha=$(rgy_sha256 "$archive")
printf '%s  %s\n' "$sha" "$archive_name" >> "$tmp"
# The order is the ordinal one, and not the locale's: the file is read by
# `sha256sum -c` everywhere and compared byte for byte between the
# platforms, so the two scripts have to write the same lines in the same
# order whatever the machine's language says.
awk '{ sub(/\r$/, "", $2) } !seen[$2]++' "$tmp" | LC_ALL=C sort -k2 > "$sums"
rm -f "$tmp"
echo "checksum: .dist/SHA256SUMS ($sha  $archive_name)"

echo ""
echo "built $archive_name"
