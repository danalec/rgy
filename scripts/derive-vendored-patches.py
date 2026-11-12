#!/usr/bin/env python3
"""derive-vendored-patches.py -- re-derive the vendored manifests' patches.

**What this is for.** The vendored trees are upstream's trees, byte for byte,
and this repository's divergences from them are kept as patches under
`scripts/rgx-local-patches/`. A patch is therefore not a hand-written file: it
is the difference between two files, and the two files are

    the upstream's file at the pinned commit   (the `-` side)
    this repository's file in the tree         (the `+` side)

which is exactly what the patch means. This script writes that difference, and
nothing else writes it: a patch edited by hand stops being a record of the
divergence and becomes a guess, and a guess is a patch that stops applying
without anybody noticing why.

**Why it has to run.** The divergence is not only the code. The vendored
manifests carry this repository's *package names* and this repository's
*version*, and both move: the rename to `rgy-vendored-*` was a divergence, and
so is every release that bumps the number. The patch is what carries them, and
a patch that does not carry them is a patch that leaves the next sync to
overwrite the name and the number with upstream's -- silently, because the
sync copies upstream's file and applies a patch that no longer says what the
tree holds.

So the order is: bump the number, derive the patches, and the drift gate
(`scripts/check-drift.sh`) says whether the two agree.

**What it reads.** The upstream checkout, at the commit the manifest pins --
the blob, not the working tree, because a checkout is not a commit. And the
tree's own file, which is the copy the patch is supposed to reproduce.

**What it writes.** One patch per vendored manifest, and nothing else: the
source patches are hand-written divergences from upstream and are not derived
from anything.

**Usage:**
    python3 scripts/derive-vendored-patches.py --upstream ../rgx
    python3 scripts/derive-vendored-patches.py --upstream ../rgx --check
"""

from __future__ import annotations

import argparse
import difflib
import pathlib
import re
import subprocess
import sys

REPO = pathlib.Path(__file__).resolve().parent.parent
MANIFEST = REPO / "scripts" / "vendored-sources.tsv"
PATCH_DIR = REPO / "scripts" / "rgx-local-patches"

# The pin: the upstream commit the copy was taken from. The manifest carries
# it in every row, and the header line says which commit it is.
PIN_RE = re.compile(r"(?<![0-9a-f])([0-9a-f]{40})(?![0-9a-f])")


def read_pin() -> str:
    """The upstream commit every row is pinned to.

    The header says which commit it is, in words and in the hash; the rows
    carry it too, but a row is a row and the header is the sentence.
    """
    for line in MANIFEST.read_text(encoding="utf-8").split("\n"):
        if not line.startswith("#"):
            continue
        m = PIN_RE.search(line)
        if m:
            return m.group(1)
    sys.exit("derive-vendored-patches: the pin is not in scripts/vendored-sources.tsv")


def manifest_rows() -> list[tuple[str, str]]:
    """The patch rows of the manifest: (patch path, the file it patches)."""
    rows: list[tuple[str, str]] = []
    for line in MANIFEST.read_text(encoding="utf-8").split("\n"):
        if not line.strip() or line.startswith("#"):
            continue
        fields = line.split("\t")
        if len(fields) < 4:
            continue
        patch_rel, target_rel, _pin, kind = fields[0], fields[1], fields[2], fields[3]
        if kind != "patch":
            continue
        if not target_rel.endswith("/Cargo.toml"):
            # The source patches are hand-written divergences from upstream:
            # they are not a difference between two files, and there is
            # nothing to derive them from.
            continue
        rows.append((patch_rel, target_rel))
    return rows


def upstream_blob(upstream: pathlib.Path, pin: str, rel: str) -> bytes:
    out = subprocess.run(
        ["git", "-C", str(upstream), "show", f"{pin}:{rel}"],
        capture_output=True,
    )
    if out.returncode != 0:
        sys.exit(f"derive-vendored-patches: the upstream has no {rel} at {pin}")
    return out.stdout


def derive(up_bytes: bytes, tree_bytes: bytes, rel: str) -> str:
    """The difference between the upstream's file and the tree's, as a patch."""
    diff = difflib.unified_diff(
        up_bytes.decode("utf-8").splitlines(keepends=True),
        tree_bytes.decode("utf-8").splitlines(keepends=True),
        fromfile=f"a/{rel}",
        tofile=f"b/{rel}",
        n=3,
    )
    text = "".join(diff)
    if text and not text.endswith("\n"):
        text += "\n"
    return text


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--upstream", required=True, help="the upstream checkout")
    ap.add_argument(
        "--check",
        action="store_true",
        help="say what would change, and change nothing",
    )
    args = ap.parse_args()

    upstream = pathlib.Path(args.upstream).resolve()
    if not (upstream / ".git").exists():
        sys.exit(f"derive-vendored-patches: {upstream} is not a git checkout")
    pin = read_pin()
    rows = manifest_rows()
    if not rows:
        sys.exit("derive-vendored-patches: the manifest names no manifest patch")

    changed = 0
    for patch_rel, target_rel in rows:
        up = upstream_blob(upstream, pin, target_rel)
        tree_path = REPO / target_rel
        if not tree_path.exists():
            sys.exit(f"derive-vendored-patches: the tree has no {target_rel}")
        tree = tree_path.read_bytes()
        text = derive(up, tree, target_rel)

        patch_path = REPO / patch_rel
        old = patch_path.read_text(encoding="utf-8") if patch_path.exists() else ""
        if old == text:
            continue
        changed += 1
        if args.check:
            print(f"  would change {patch_rel}")
            continue
        patch_path.parent.mkdir(parents=True, exist_ok=True)
        patch_path.write_text(text, encoding="utf-8", newline="")
        print(f"  changed {patch_rel}")

    if not changed:
        print("derive-vendored-patches: nothing to change")
        return 0
    if args.check:
        print(f"derive-vendored-patches: {changed} patch(es) would change")
        return 0
    print(f"derive-vendored-patches: {changed} patch(es) derived")
    return 0


if __name__ == "__main__":
    sys.exit(main())
