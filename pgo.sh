#!/bin/sh
# Release build optimized with a profile of rgy's own searches (PGO).
# fsearch's pgo.sh pattern: name searches measured 10-35% faster after
# profile-guided codegen. The trainer is the name bench — it exercises the
# same Searcher path the daemon does, over a deterministic corpus.
# Needs: rustup component add llvm-tools
# (Windows: run under a POSIX shell, or drive the same two cargo builds
# with RUSTFLAGS by hand — see docs/insights-from-rgx.md.)
set -e
# Build from the workspace root no matter where the script is invoked
# from — `cargo -p rgy-engine` is meaningless outside it.
cd "$(dirname "$0")"
profdata=$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-profdata 2>/dev/null | head -1)
[ -x "$profdata" ] || { echo "pgo.sh: needs llvm-profdata: rustup component add llvm-tools" >&2; exit 1; }
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
RUSTFLAGS="-Cprofile-generate=$tmp/prof" cargo build --release -p rgy-engine --example name_bench --target-dir "$tmp/target"
"$tmp/target/release/examples/name_bench" 50000 >/dev/null
"$profdata" merge -o "$tmp/rgy.profdata" "$tmp/prof"
RUSTFLAGS="-Cprofile-use=$tmp/rgy.profdata" cargo build --release
echo "pgo.sh: release build in target/release uses the trained profile"
