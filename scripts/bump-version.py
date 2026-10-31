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

The installers are touched too, and they are touched for the same reason:
an installer names the release it installs, and a release whose installer
still names the previous one downloads the previous release's archive and
refuses it -- the digest is the new archive's and the file is the old
one.

**What it does not walk.** The tree's own build outputs, and the nested
checkouts: the release tree the chain cuts the archives from, and the
packaged crates the publish step already built. They are not this tree,
and rewriting their manifests would change bytes that belong to another
tree.

**Usage:**
    python3 scripts/bump-version.py 0.1.2
    python3 scripts/bump-version.py --check 0.1.2   # what would change
"""

from __future__ import annotations

import argparse
import io
import os
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

# The installers name the release they install, and the release's name is
# the tag: "rgy-v<version>". The tag is rewritten wherever it is written,
# and it is written in two files, one for each platform -- and in the unix
# one twice, once in the setting and once in the usage the file prints.
INSTALLER_TAG = {
    "install.sh": [
        re.compile(r'(?m)^(version=")rgy-v[0-9]+\.[0-9]+\.[0-9]+(")$'),
        re.compile(r'(?m)^(#   bash install\.sh --version )rgy-v[0-9]+\.[0-9]+\.[0-9]+$'),
    ],
    "install.ps1": [
        re.compile(r"(?m)^(\$Version = ')rgy-v[0-9]+\.[0-9]+\.[0-9]+('$)", re.IGNORECASE),
    ],
}


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
    """Every manifest in the tree, the vendored ones included.

    The tree's own build outputs and the nested checkouts are skipped. A
    nested checkout is not this tree: the release tree the chain cuts the
    archives from is the commit's own checkout, and the packaged crates
    under its target/ are what the publish step already built. Rewriting
    their manifests would change bytes that belong to another tree -- and
    silently, because a bump that says it changed thirty manifests and
    changed fifty-two is a bump nobody reads.
    """
    out: list[Path] = []
    for dirpath, dirnames, filenames in os.walk(root):
        here = Path(dirpath)
        rel = here.relative_to(root).as_posix()
        if rel == ".":
            rel = ""
        if rel and (here / ".git").exists():
            # A checkout of its own: the tree is not this one.
            dirnames[:] = []
            continue
        dirnames[:] = [d for d in dirnames if d not in ("target", ".git")]
        if "Cargo.toml" in filenames:
            out.append(here / "Cargo.toml")
    return sorted(out)


def rewrite_installer(path: Path, version: str, write: bool) -> bool:
    """The installer naming the release it installs, and whether it moved.

    The tag is the tree's version: an installer that names another
    release downloads that release's archive and refuses it, because the
    digest pinned inside it is the digest of the archive this release
    carries.
    """
    if not path.is_file():
        return False
    patterns = INSTALLER_TAG.get(path.name)
    if not patterns:
        return False
    with io.open(path, "r", encoding="utf-8", newline="") as handle:
        text = handle.read()
    out = text
    count = 0

    def with_tag(m: re.Match[str]) -> str:
        # The second group is the tail of the line, when the pattern names
        # one: the setting keeps the quote it had, the usage ends with the
        # tag itself.
        tail = m.group(2) if m.lastindex and m.lastindex >= 2 else ""
        return m.group(1) + "rgy-v" + version + tail

    for pattern in patterns:
        out, hits = pattern.subn(with_tag, out)
        count += hits
    # The text, and not the number of matches: a pattern that matches the
    # tag it is replacing is not a change, and a bump that says it changed
    # a file it did not change is a bump nobody believes.
    if count == 0 or out == text:
        return False
    if write:
        with io.open(path, "w", encoding="utf-8", newline="") as handle:
            handle.write(out)
        print(f"  changed {path.name} (installer tag -> rgy-v{version})")
    else:
        print(f"  would change {path.name} (installer tag -> rgy-v{version})")
    return True


def rewrite(text: str, version: str, packages: bool = True) -> tuple[str, int, int]:
    """The manifest with the new version, and how much changed.

    The package version is only rewritten when the package is published:
    the e2e harness keeps its 0.0.0, because a number that looks published
    is a number somebody will try to publish. Its requirements on our own
    crates are ours, and they follow the tree like every other requirement
    does.
    """
    packages_changed = 0
    dependencies = 0

    def package(m: re.Match[str]) -> str:
        nonlocal packages_changed
        packages_changed += 1
        return f'{m.group(1)}"{version}"'

    def dependency(m: re.Match[str]) -> str:
        nonlocal dependencies
        dependencies += 1
        return f'{m.group("head")}"{version}"'

    out = text
    if packages:
        out = PACKAGE_VERSION.sub(package, out, count=1)
    out = OUR_DEPENDENCY.sub(dependency, out)
    return out, packages_changed, dependencies


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
    manifests_changed = 0
    for manifest in manifests(root):
        if not is_ours(manifest, root):
            continue
        published = is_published(manifest)
        with io.open(manifest, "r", encoding="utf-8", newline="") as handle:
            text = handle.read()
        out, packages, dependencies = rewrite(text, version, packages=published)
        if out == text:
            continue
        rel = manifest.relative_to(root).as_posix()
        changed.append(rel)
        manifests_changed += 1
        if args.check:
            print(f"  would change {rel} ({packages} package, {dependencies} requirement)")
            continue
        with io.open(manifest, "w", encoding="utf-8", newline="") as handle:
            handle.write(out)
        print(f"  changed {rel} ({packages} package, {dependencies} requirement)")

    # The installers name the release they install, and that name is the
    # tree's version -- the same one the manifests carry.
    for name in sorted(INSTALLER_TAG):
        installer = root / name
        if rewrite_installer(installer, version, write=not args.check):
            changed.append(name)

    if not changed:
        print("bump-version: nothing to change")
        return 0
    if args.check:
        print(f"bump-version: {len(changed)} file(s) would change")
        return 0

    if manifests_changed:
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
    print(f"bump-version: {len(changed)} file(s) at {version}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
