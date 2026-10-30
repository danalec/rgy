#!/usr/bin/env python3
"""bump-version.py -- set this repository's own crates to one version.

**Why this exists.** The crates on crates.io carry the code of the day
they were published, and crates.io does not replace a published
version: a release that changes a crate publishes it under a new
number. So a release is a number, decided once, and every crate of ours
carries it -- the crates and the binaries and the requirement lines
between them.

That last part is the one that is easy to leave out. The dependency
lines are what the resolver reads when somebody installs the tree, and
a requirement left at the old number lets a stale crate back into a
build that needs the new one.

**What it touches.** Every manifest of ours: the version of the
package, and the requirement of every dependency that names one of our
own crates. The vendored trees are not touched at all -- they are
upstream rgx's crates, they carry upstream's versions, and upstream
publishes them. Their *requirement* lines in our manifests are ours and
are left alone too: a version bump is not a place to change what
upstream's crate is called.

**What it does not touch.** The e2e harness crate (`publish = false`)
keeps its `0.0.0`: it is not a published crate, and a number that
looks published is a number somebody will try to publish.

**Usage:**
    python3 scripts/bump-version.py 0.1.1
    python3 scripts/bump-version.py --check 0.1.1   # what would change
"""

from __future__ import annotations

import argparse
import io
import re
import subprocess
import sys
from pathlib import Path

# The vendored trees: upstream rgx's crates. Their manifests carry
# upstream's versions, and upstream publishes them -- a bump here would
# be a claim about somebody else's crate.
VENDORED_DIRS = ("crates/rgx-", "crates/rar-codec")

# Our own crates: the names that start with this are ours, and a
# requirement on one of them is ours to write.
OUR_PREFIX = "rgy-"

# The package version, in the `[package]` table.
PACKAGE_VERSION = re.compile(r'^(version\s*=\s*)"([^"]+)"', re.MULTILINE)

# A dependency on one of our crates, with its requirement:
#   rgy-proto = { path = "../../crates/proto", version = "0.1.0" }
OUR_DEPENDENCY = re.compile(
    r'^(?P<head>\s*' + OUR_PREFIX + r'[a-z0-9-]*\s*=\s*\{[^}]*?version\s*=\s*)"(?P<version>[^"]+)"',
    re.MULTILINE,
)


def repo_root() -> Path:
    return Path(__file__).resolve().parent.parent


def is_ours(manifest: Path, root: Path) -> bool:
    """Is this manifest one of ours? The vendored trees are not."""
    rel = manifest.relative_to(root).as_posix()
    return not any(rel.startswith(prefix) for prefix in VENDORED_DIRS)


def is_published(manifest: Path) -> bool:
    """Is this package one that goes to the registry?

    The e2e harness is not: it carries `publish = false`, and it is
    never on crates.io, so the number it carries is not a number
    anybody can install.
    """
    with io.open(manifest, "r", encoding="utf-8", newline="") as handle:
        return not re.search(r"^publish\s*=\s*false", handle.read(), re.MULTILINE)


def manifests(root: Path) -> list[Path]:
    """Every manifest in the tree, the vendored ones included."""
    out = []
    for path in sorted(root.rglob("Cargo.toml")):
        rel = path.relative_to(root).as_posix()
        if rel.startswith("target/") or rel.startswith(".git/"):
            continue
        out.append(path)
    return out


def rewrite(text: str, version: str) -> tuple[str, int, int]:
    """The manifest with the new version, and how much changed."""
    packages = 0
    dependencies = 0

    def package(m: re.Match[str]) -> str:
        nonlocal packages
        packages += 1
        return f'{m.group(1)}"{version}"'

    def dependency(m: re.Match[str]) -> str:
        nonlocal dependencies
        dependencies += 1
        return f'{m.group("head")}"{version}"'

    out = PACKAGE_VERSION.sub(package, text, count=1)
    out = OUR_DEPENDENCY.sub(dependency, out)
    return out, packages, dependencies


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("version", help="the version every crate of ours carries")
    ap.add_argument(
        "--check",
        action="store_true",
        help="say what would change, and change nothing",
    )
    args = ap.parse_args()

    version = args.version
    if not re.fullmatch(r"\d+\.\d+\.\d+", version):
        print(
            f"bump-version: {version} is not a version of three numbers",
            file=sys.stderr,
        )
        return 2

    root = repo_root()
    changed: list[str] = []
    for manifest in manifests(root):
        if not is_ours(manifest, root):
            continue
        if not is_published(manifest):
            # The e2e harness is not a published crate: its number is
            # not one anybody can install, and a number that looks
            # published is a number somebody will try to publish.
            continue
        with io.open(manifest, "r", encoding="utf-8", newline="") as handle:
            text = handle.read()
        out, packages, dependencies = rewrite(text, version)
        if out == text:
            continue
        rel = manifest.relative_to(root).as_posix()
        changed.append(rel)
        if args.check:
            print(f"  would change {rel} ({packages} package, {dependencies} requirement)")
            continue
        with io.open(manifest, "w", encoding="utf-8", newline="") as handle:
            handle.write(out)
        print(f"  changed {rel} ({packages} package, {dependencies} requirement)")

    if not changed:
        print("bump-version: nothing to change")
        return 0
    if args.check:
        print(f"bump-version: {len(changed)} manifest(s) would change")
        return 0

    # The lockfile is cargo's own record of the tree, and the tree just
    # changed: `cargo update --workspace` rewrites the members' entries
    # and leaves every other dependency alone.
    lock = subprocess.run(
        ["cargo", "update", "--workspace", "--offline"],
        cwd=str(root),
        capture_output=True,
        text=True,
    )
    if lock.returncode != 0:
        print("bump-version: the lockfile could not be refreshed", file=sys.stderr)
        print(lock.stderr.strip()[:800], file=sys.stderr)
        return 1
    print(f"bump-version: {len(changed)} manifest(s) at {version}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
