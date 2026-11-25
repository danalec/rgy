#!/usr/bin/env python3
"""The order this repository's crates go to crates.io in.

**Why this exists.** The crates are published by hand, one `cargo publish` at
a time, and the order matters: a crate that depends on another one cannot be
published before the dependency is on the registry, because the registry
resolves the dependency when it *installs* the crate, not when it publishes
it. `rgy-name-core` depends on `rgy-par`, and `rgy-par` was not on the
registry at all: publishing `rgy-name-core` first would have shipped a crate
that no user could install, and nothing in the tree would have said so.

So the order is computed here, from the manifests themselves, and the
publish script follows it. The rule is a dependency order: a crate is
published after every crate it depends on. Ties are broken alphabetically,
so the same tree always produces the same plan -- with one exception, the
**faces go last**: the launcher is a client of the running daemon, so the
product behind it is on the registry before the window onto it is.

**What it reads.** `cargo metadata` for the workspace's packages and their
in-tree dependency edges, and crates.io for what the registry already has.
Nothing is published here: this script prints the plan, and
`scripts/publish-crates.sh` (and its `.ps1` twin) carry it out.

**What it refuses.** A crate that can never be published — one with
`publish = false`, or one that is not on the registry and is not ours to
put there — is a hole: any crate that depends on it would be published
broken. The plan says so and exits 1.

Usage:
    python3 scripts/publish-plan.py [--registry https://crates.io]
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import urllib.error
import urllib.request
from pathlib import Path

# The vendored trees are the upstream's crates and they go out under this
# repository's own names -- `rgy-vendored-core`, the
# `rgy-vendored-adapters-*` family and `rgy-vendored-rar-codec` -- with this
# tree's patches inside them. That is not a fork of somebody else's crate:
# the upstream is the same owner's project, and what is published here is
# the upstream's engine with this tree's patches on top. The name is this
# repository's because the upstream's names are the upstream's to publish:
# a name and a number is one thing on the registry, and two publishers
# cannot both take it.
#
# Their version is this repository's version, the same one every other
# crate of ours carries, because they are published from here. The vendored
# manifests are upstream's files plus a patch, and the patch is what carries
# the name and the number -- `scripts/derive-vendored-patches.py` writes it,
# and `scripts/check-drift.sh` says whether the patch and the tree agree.
#
# So nothing is withheld from the plan: the vendored trees are ours to
# publish, and the plan says so.
VENDORED: tuple[str, ...] = ()

# The faces of the product: the launcher is the window the user sees, and it
# is a **client of the running daemon** -- it talks to `searchd` over the wire
# rather than linking it. So the daemon's crates have to be on the registry
# before the launcher is: a launcher published first is a window onto a
# daemon that is not there yet.
#
# Nothing in the launcher's manifest says that -- it does not depend on
# `searchd`, it speaks to it -- so the rule is here, and it is the only
# place where the order of a release is decided by anything but a
# dependency.
FACES = ("rgxy",)

USER_AGENT = "rgy-publish-plan (crates.io publish order)"


def repo_root() -> Path:
    return Path(__file__).resolve().parent.parent


def workspace_packages(root: Path) -> list[dict]:
    """The workspace's packages, with their in-tree dependency edges."""
    out = subprocess.run(
        [
            "cargo",
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--manifest-path",
            str(root / "Cargo.toml"),
        ],
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    packages = json.loads(out)["packages"]
    names = {p["name"] for p in packages}
    for p in packages:
        # EVERY kind of edge constrains the order, and not only the ones an
        # installer resolves. A dev-dependency does not enter a binary, but
        # `cargo publish` packages the crate and then builds that package
        # to verify it, and the build carries the examples and the tests --
        # and the dev-dependencies they need, resolved from the registry
        # like any other.
        #
        # Measured on 2026-10-30, with the plan as it was: it put rgy-engine
        # before rgy-name-core, because the edge between them is a
        # dev-dependency (the examples' oracle), and the publish stopped
        # with "failed to select a version for the requirement
        # `rgy-name-core = "^0.1.1"` -- candidate versions found which
        # didn't match: 0.1.0". The dependency was not on the registry yet,
        # and the plan had put it after the crate that needs it.
        p["_deps"] = sorted(
            {
                d["name"]
                for d in p["dependencies"]
                # cargo metadata writes no `kind` at all for a normal
                # dependency, and "dev" or "build" for the other two.
                if d["name"] in names
            }
        )
        p["_publishable"] = p.get("publish") != []
        p["_vendored"] = p["name"] in VENDORED
    return packages


def publish_order(packages: list[dict]) -> list[dict]:
    """The packages, dependencies first, and the faces last.

    Kahn's algorithm, with the ready set kept in alphabetical order: the same
    tree always produces the same plan, and a plan that changed between two
    runs of the same tree would be a plan nobody could check.

    The **faces** go at the end: the launcher is a client of the running
    daemon, so the product behind it has to be on the registry before the
    window onto it is. Nothing in the launcher's manifest says that -- it
    does not depend on `searchd`, it speaks to it -- so the rule is here.
    A face that something else depends on cannot move: the dependency order
    wins, and the plan stays a plan a publisher can follow.
    """
    by_name = {p["name"]: p for p in packages}
    pending = {p["name"]: set(p["_deps"]) for p in packages}
    ordered: list[dict] = []
    while pending:
        ready = sorted(name for name, deps in pending.items() if not deps)
        if not ready:
            # A cycle: the workspace cannot be published in any order. Say so
            # rather than looping.
            raise SystemExit(
                "publish-plan: the dependency graph has a cycle: "
                + ", ".join(sorted(pending))
            )
        for name in ready:
            ordered.append(by_name[name])
            del pending[name]
        for deps in pending.values():
            deps.difference_update(ready)

    # Who depends on what: a face that something else needs cannot be moved
    # to the end, because the crate that needs it would then be published
    # first.
    dependents: dict[str, list[str]] = {}
    for pkg in packages:
        for dep in pkg["_deps"]:
            dependents.setdefault(dep, []).append(pkg["name"])

    movable = [
        p for p in ordered if p["name"] in FACES and not dependents.get(p["name"])
    ]
    if movable:
        ordered = [p for p in ordered if p not in movable] + movable
    return ordered


def registry_has(registry: str, name: str, version: str) -> bool | None:
    """Is `name` `version` on the registry? None when nobody asked."""
    url = f"{registry.rstrip('/')}/api/v1/crates/{name}/{version}"
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    try:
        with urllib.request.urlopen(req, timeout=20) as r:
            return r.status == 200
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return False
        raise SystemExit(f"publish-plan: the registry answered {e.code} for {name}") from e
    except urllib.error.URLError as e:
        raise SystemExit(f"publish-plan: the registry could not be reached: {e}") from e


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--registry", default="https://crates.io")
    ap.add_argument(
        "--wait",
        metavar="NAME:VERSION",
        help="wait until the registry carries this version, and report whether it does",
    )
    ap.add_argument("--timeout", type=int, default=300)
    args = ap.parse_args()

    if args.wait:
        name, _, version = args.wait.rpartition(":")
        if not name or not version:
            raise SystemExit("publish-plan: --wait wants NAME:VERSION")
        return wait_for(args.registry, name, version, args.timeout)

    root = repo_root()
    packages = workspace_packages(root)
    ordered = publish_order(packages)

    counts = {"publish": 0, "current": 0, "vendored": 0, "withheld": 0}
    holes: list[str] = []

    # Who depends on what, the other way round: a hole is about the crates
    # that depend on it.
    dependents: dict[str, list[str]] = {}
    for pkg in packages:
        for dep in pkg["_deps"]:
            dependents.setdefault(dep, []).append(pkg["name"])

    for pkg in ordered:
        name = pkg["name"]
        version = pkg["version"]
        if not pkg["_publishable"]:
            action = "withheld"
        elif pkg["_vendored"]:
            action = "vendored"
        else:
            action = "current" if registry_has(args.registry, name, version) else "publish"
        counts[action] += 1
        pkg["_action"] = action
        print(f"  {action:<9} {name} {version}")

        # A crate that can never be published is a hole in the plan -- but
        # only for the crates that depend on it. The end-to-end harness is
        # withheld on purpose and nothing depends on it, so it is a line in
        # the plan and not a failure.
        if action == "withheld":
            for dependent in dependents.get(name, []):
                holes.append(
                    f"{dependent} depends on {name}, which cannot be published"
                )

    print(
        f"  {counts['publish']} to publish, {counts['current']} current, "
        f"{counts['vendored']} vendored, {counts['withheld']} withheld"
    )

    if holes:
        for h in holes:
            print(f"publish-plan: FAIL: {h}", file=sys.stderr)
        return 1
    return 0


def wait_for(registry: str, name: str, version: str, timeout: int) -> int:
    """Wait until the registry carries `name` `version`.

    The index is built asynchronously, so a crate that has just been
    published cannot be depended on for a moment. A publish script that does
    not wait for this publishes a dependent against a dependency the registry
    has not listed yet, and the registry refuses with a message that reads
    like a broken manifest.
    """
    import time

    deadline = time.monotonic() + timeout
    while True:
        if registry_has(registry, name, version):
            print(f"  {name} {version} is on the registry")
            return 0
        if time.monotonic() >= deadline:
            print(
                f"publish-plan: FAIL: {name} {version} did not reach the registry "
                f"within {timeout}s",
                file=sys.stderr,
            )
            return 1
        time.sleep(5)


if __name__ == "__main__":
    sys.exit(main())
