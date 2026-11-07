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

**What it touches.** Every manifest in the tree: the version of the
package, and the requirement of every dependency that names one of our own
crates. The vendored trees are in that list now. They used to be out of it,
because the upstream published them and a version here would have been a
claim about somebody else's crate; since the rename they are published by
this repository, under this repository's names (`rgy-vendored-core` and the
`rgy-vendored-adapters-*` family), and a package this repository publishes
at a number this repository does not carry is a package crates.io refuses.
The vendored trees' *sources* are still upstream's, byte for byte; what the
repository publishes is the upstream's code with this tree's patches on top,
and the version of that package is ours.

A requirement is ours when the entry names one of our packages: either the
key is one of ours (`rgy-...`), or the entry carries `package = "rgy-..."`,
which is how a renamed dependency says which package it is. The upstream's
own requirements -- on crates that are not vendored -- are left alone, and
so are the requirement lines the upstream's manifests carry on the upstream's
crates.

**What it does not touch.** The e2e harness crate (`publish = false`)
keeps its `0.0.0`: it is not a published crate, and a number that
looks published is a number somebody will try to publish.

**What has to follow it.** The vendored manifests' patches. The vendored
manifests are upstream's files plus a patch, and the patch is what carries
this repository's version and this repository's package names. A bump that
changes a vendored manifest changes the difference between it and upstream's,
so `scripts/derive-vendored-patches.py` re-derives the twelve patches
afterwards, and `scripts/check-drift.sh` says whether the two agree.

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

**The release number and the crate number.** The registry takes a version
of three numbers and nothing else, and a release that will carry fixes for
a while wants a fourth -- the fix count of the line. So the version this
script takes may have three numbers or four: with four, the crates carry
the first three and the release tag (the installers' `rgy-v<version>`)
carries all four. `0.1.10.0` is a release whose crates are `0.1.10`, and
the fixes that follow it are `0.1.10.1`, `0.1.10.2`, until the line turns
and both numbers say `0.2.0`.

**The source-only release.** A bump of the crates does not itself build the
multi-platform archives. When the new number is a source-only release, the
installers keep pinning the last tag that shipped archives with verified
digests; `--check` will offer to move that pin, and the answer is no until
a release with assets exists. This was the paired upstream's 0.1.10 policy
and this tree's while 0.1.10 was source-only. When archives are built, a
four-number bump (0.1.10.0) moves the installers to the new tag without
moving the crates, and their digests stay placeholders until the publish
step rewrites them from the real SHA256SUMS.

**Usage:**
    python3 scripts/bump-version.py 0.1.2
    python3 scripts/bump-version.py 0.1.10.0  # crates 0.1.10, tag rgy-v0.1.10.0
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

# Our own crates: the names that start with this are ours, and a
# requirement on one of them is ours to write.
OUR_PREFIX = "rgy-"

# The packages this repository publishes under a name of its own: the
# vendored trees, carried from the upstream's code and published under this
# repository's name. A dependency that names one of them is ours to write,
# and the entry says so with `package = "rgy-vendored-..."`.
VENDORED_PREFIX = "rgy-vendored-"

# The package version, in the `[package]` table.
PACKAGE_VERSION = re.compile(r'^(version\s*=\s*)"([^"]+)"', re.MULTILINE)

# The sections a dependency can live in. A target-specific table is one of
# them -- `[target.'cfg(windows)'.dependencies]` -- because a dependency the
# platform gates is a dependency all the same, and a requirement left at the
# old number in one of them breaks `cargo build --locked` on exactly the
# platform the table names. The section is read by its END: every
# dependencies table, plain or gated, ends with the word before its bracket.
DEPENDENCY_SECTION = "dependencies]"

# A dependency entry: the key and the table that opens it. The entry can
# span lines -- a `features = [...]` list does -- so it is read to its
# closing brace and not to the end of its first line.
ENTRY_START = re.compile(r'^(?P<indent>\s*)(?P<key>[A-Za-z0-9_-]+)\s*=\s*\{')

# The requirement inside an entry, and the package a renamed entry names.
ENTRY_VERSION = re.compile(r'(?P<head>\bversion\s*=\s*)"(?P<version>[^"]+)"')
ENTRY_PACKAGE = re.compile(r'\bpackage\s*=\s*"(?P<package>[^"]+)"')

# The installers name the release they install, and the release's name is
# the tag: "rgy-v<version>". The tag is rewritten wherever it is written,
# and it is written in two files, one for each platform -- and in the unix
# one twice, once in the setting and once in the usage the file prints.
INSTALLER_TAG = {
    "install.sh": [
        re.compile(r'(?m)^(version=")rgy-v[0-9]+(?:\.[0-9]+){2,3}(")$'),
        re.compile(r'(?m)^(#   bash install\.sh --version )rgy-v[0-9]+(?:\.[0-9]+){2,3}$'),
    ],
    "install.ps1": [
        re.compile(r"(?m)^(\$Version = ')rgy-v[0-9]+(?:\.[0-9]+){2,3}('$)", re.IGNORECASE),
    ],
}


def repo_root() -> Path:
    return Path(__file__).resolve().parent.parent


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


def rewrite_requirements(text: str, version: str) -> tuple[str, int]:
    """The manifest with our requirements on the new version, and how many.

    A requirement is ours when the entry names one of our packages: the key
    is one of ours (`rgy-...`), or the entry carries `package =
    "rgy-vendored-..."`, which is how a renamed dependency says which
    package it is. The key of a renamed dependency is the upstream's --
    `rgx-core` -- so the key alone does not say whose requirement it is.

    The entries are read one at a time, to their closing brace: a
    dependency whose `features = [...]` list spans lines has its
    requirement on the first of them, and an entry read to the end of its
    line is an entry whose second half nobody looked at.
    """
    lines = text.split("\n")
    out: list[str] = []
    section = ""
    count = 0
    i = 0
    while i < len(lines):
        line = lines[i]
        stripped = line.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            section = stripped
            out.append(line)
            i += 1
            continue
        start = ENTRY_START.match(line)
        if start is None or not section.endswith(DEPENDENCY_SECTION):
            out.append(line)
            i += 1
            continue
        # The entry, to its closing brace.
        depth = line.count("{") - line.count("}")
        last = i
        while depth > 0 and last + 1 < len(lines):
            last += 1
            depth += lines[last].count("{") - lines[last].count("}")
        entry = "\n".join(lines[i:last + 1])

        package = ENTRY_PACKAGE.search(entry)
        ours = start.group("key").startswith(OUR_PREFIX) or (
            package is not None
            and package.group("package").startswith(VENDORED_PREFIX)
        )
        version_match = ENTRY_VERSION.search(entry) if ours else None
        if version_match is not None:
            entry = (
                entry[:version_match.start("version")]
                + version
                + entry[version_match.end("version"):]
            )
            count += 1
        out.extend(entry.split("\n"))
        i = last + 1
    return "\n".join(out), count


def rewrite(text: str, version: str, packages: bool = True) -> tuple[str, int, int]:
    """The manifest with the new version, and how much changed.

    The package version is only rewritten when the package is published:
    the e2e harness keeps its 0.0.0, because a number that looks published
    is a number somebody will try to publish. Its requirements on our own
    crates are ours, and they follow the tree like every other requirement
    does.
    """
    packages_changed = 0

    def package(m: re.Match[str]) -> str:
        nonlocal packages_changed
        packages_changed += 1
        return f'{m.group(1)}"{version}"'

    out = text
    if packages:
        out = PACKAGE_VERSION.sub(package, out, count=1)
    out, dependencies = rewrite_requirements(out, version)
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
    if not re.fullmatch(r"\d+\.\d+\.\d+(?:\.\d+)?", version):
        print(
            f"bump-version: {version} is not a version of three or four numbers",
            file=sys.stderr,
        )
        return 2
    # The release number is what the argument said. The crates' number is
    # the first three of it: the registry takes three numbers and nothing
    # else, and the fourth -- when there is one -- is the fix count of the
    # line, which the release tag carries and no manifest does.
    crate_version = ".".join(version.split(".")[:3])
    tag_version = version

    root = repo_root()
    changed: list[str] = []
    manifests_changed = 0
    for manifest in manifests(root):
        published = is_published(manifest)
        with io.open(manifest, "r", encoding="utf-8", newline="") as handle:
            text = handle.read()
        out, packages, dependencies = rewrite(text, crate_version, packages=published)
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
    # release's number -- all four of it when the line carries four, so a
    # fix release moves the tag without moving the crates' number.
    for name in sorted(INSTALLER_TAG):
        installer = root / name
        if rewrite_installer(installer, tag_version, write=not args.check):
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
    print(
        f"bump-version: {len(changed)} file(s) at {tag_version}"
        + (f" (crates at {crate_version})" if crate_version != tag_version else "")
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
