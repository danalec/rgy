#!/usr/bin/env python3
# check-egm-contracts.py -- validate the EGM contracts of THIS tree.
#
# The checks live in this one file so that the two platform wrappers,
# scripts/check-egm-contracts.sh and scripts/check-egm-contracts.ps1, cannot
# drift apart: both only locate a Python interpreter and hand it the root.
#
# The EGM.md contracts are tracked, so a fresh CI checkout has the same
# inputs as a working tree: there is one check, run identically by the local
# gate and by the CI matrix. There is no "CI-only" mode to drift from it.
#
# The configuration is .egmrc at the repository root. It is the single
# source of truth for the contract filename and for the verification
# command; a contract that names a different command is a failed check.
# The gate also enforces the config itself, rather than leaving it as a
# convention the bootstrap skill may or may not read.
#
# Besides checking the contracts that exist, the checker derives the set of
# directories that owe a contract from the source tree (the same prune the
# bootstrap uses) and fails when one is missing. A new crate or src/ dir
# must not be able to land without an EGM.md.
#
# Derived facts (which directories owe a contract, what each contract hashes
# to) are pinned in a generated CONTRACTS.index. Authored decisions (the
# drift status) stay human-written and are reviewed in the diff. The two are
# deliberately separate: a machine can verify the first, only a human can
# make the second.
#
# Phase 2 derives the structural skeleton of every contract from the tree:
# generated_children() names the immediate subdirectories that owe a
# contract, and generated_problems() holds each tracked contract's
# `](./<child>/EGM.md)` links and its Verification Run line against that
# skeleton, so a rename, a new crate, a deleted subcontract or a stale link
# cannot land silently. It is folded into validate() -- there is one check
# everywhere. --generate writes scratch skeletons under
# <artifact_dir>/generated/ and --generated-check runs only the skeleton gate.
#
# Usage:
#   python3 scripts/check-egm-contracts.py [--root DIR]
#   python3 scripts/check-egm-contracts.py --self-test
#   python3 scripts/check-egm-contracts.py --reindex
#   python3 scripts/check-egm-contracts.py --generate
#   python3 scripts/check-egm-contracts.py --generated-check
#   python3 scripts/check-egm-contracts.py --fix
#
# Exit status: 0 all contracts valid, 1 one or more failures, 2 bad config.

import argparse
import fnmatch
import hashlib
import os
import re
import subprocess
import sys
from datetime import date, timedelta
from pathlib import Path

# Directories that never hold a contract: VCS, build output, hidden agent
# scratch, and generated corpora. A leading dot covers .git, .egm, .dist,
# .worktrees, .cache and the rest without a second list to keep in step.
SKIP_DIRS = {
    "target", "node_modules", "__pycache__", "ripgrep-15", "venv", "dist",
    "build", "third_party", "golden",
}
SKIP_PREFIXES = (".",)

# The source suffixes the bootstrap counts when it prunes the tree. Anything
# else is data, a manifest, a lockfile or documentation, and neither owes a
# contract nor keeps a parent directory alive.
SOURCE_SUFFIXES = {
    ".c", ".cc", ".cpp", ".cxx", ".h", ".hh", ".hpp", ".rs", ".go", ".zig",
    ".lean", ".java", ".kt", ".scala", ".cs", ".fs", ".py", ".rb", ".php",
    ".lua", ".sh", ".bash", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs",
    ".swift", ".m", ".dart", ".sql", ".proto", ".graphql", ".gql", ".thrift",
    ".prisma", ".tf", ".hcl", ".cue",
}
TEST_DIR_NAMES = {"tests", "testing", "testdata"}
TEST_FILE = re.compile(r"(_test\.|Test\.|^test_|\.spec\.)")

REQUIRED_PEANUT_SECTIONS = ("Invariants", "Side Effects", "Verification")
BLUEPRINT_HEADINGS = re.compile(r"^## (?:Subdirectories|System Overview|Global Invariants)", re.M)
PEANUT_HEADING = re.compile(r"^## Interface\s*$", re.M)
RUN_LINE = re.compile(r"^- Run: `([^`]+)`", re.M)
LINK = re.compile(r"\]\(([^)]+)\)")
LINE_CITATION = re.compile(r"\.(?:rs|sh|ps1|py|rb|toml|yml|md):\d+")
PLACEHOLDER = re.compile(r"Details pending|documentation failed")
TABLE_PATH = re.compile(r"`([^`]+\.(?:sh|ps1))`")
CONTRACTS_INDEX = "CONTRACTS.index"


def read_config(path):
    cfg = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, value = line.split("=", 1)
        cfg[key.strip()] = value.strip()
    for key in ("target_filename", "artifact_dir", "verification", "scope",
                "source_extensions"):
        if not cfg.get(key):
            raise ValueError(f".egmrc is missing '{key}'")
    if cfg["target_filename"].lower() == "readme.md":
        raise ValueError(
            "target_filename must not be README.md: the root and crate "
            "READMEs are hand-written and tests/crate_readmes.rs holds them"
        )
    # Validate the scope here, not on first use: a malformed scope is a
    # config error (exit 2), not a crash from inside the index builder.
    scope_excludes(cfg)
    return cfg


def scope_excludes(cfg):
    """The out-of-scope directory prefixes from `.egmrc`'s `scope`.

    rgy carries upstream's rgx trees verbatim (see AGENTS.md §1). Contracts
    inside them would be a fork the next sync deletes, so neither the
    contract set nor the expected-directory set may see them.
    """
    raw = cfg["scope"].strip()
    if not raw.startswith("exclude:"):
        raise ValueError("scope must be 'exclude:<prefix>[,<prefix>...]'")
    return [p.strip().strip("/") for p in raw[len("exclude:"):].split(",") if p.strip()]


def excluded(rel, excludes):
    return any(
        rel == e or rel.startswith(e + "/") or fnmatch.fnmatch(rel, e)
        for e in excludes
    )


def walk_dirs(root, excludes=()):
    """Yield (dirpath, dirnames, filenames) with the bootstrap's skips and
    the configured scope applied. A leading dot covers VCS and scratch."""
    for dirpath, dirnames, filenames in os.walk(root):
        rel = Path(dirpath).relative_to(root).as_posix()
        if rel == ".":
            rel = ""
        if rel and excluded(rel, excludes):
            dirnames[:] = []
            continue
        dirnames[:] = sorted(
            d for d in dirnames
            if d not in SKIP_DIRS
            and not d.startswith(SKIP_PREFIXES)
            and not excluded((rel + "/" + d).lstrip("/"), excludes)
        )
        yield dirpath, dirnames, filenames


def contracts(root, filename, excludes=()):
    for dirpath, _dirnames, filenames in walk_dirs(root, excludes):
        if filename in filenames:
            yield Path(dirpath) / filename


def expected_directories(root, filename, excludes=()):
    """Return the relative directories that owe a contract, by the bootstrap's prune."""
    info = {}
    for dirpath, _dirnames, filenames in walk_dirs(root, excludes):
        rel = Path(dirpath).relative_to(root).as_posix()
        if rel == ".":
            rel = ""
        src = [f for f in filenames if Path(f).suffix.lower() in SOURCE_SUFFIXES]
        nontest = [f for f in src if not TEST_FILE.search(f)]
        test_only = (
            Path(dirpath).name in TEST_DIR_NAMES or (bool(src) and not nontest)
        )
        parent = "" if rel == "" else Path(rel).parent.as_posix()
        if parent == ".":
            parent = ""
        info[rel] = {
            "parent": parent,
            "has_src": bool(src),
            "test_only": test_only,
            "children": [],
        }
    for rel, rec in info.items():
        if rel and rec["parent"] in info:
            info[rec["parent"]]["children"].append(rel)

    # A directory has code when it holds source itself or a child with code.
    # Propagate that upward until it stops moving; the root is always kept.
    has_code = {
        rel: (not rec["test_only"] and rec["has_src"]) for rel, rec in info.items()
    }
    changed = True
    while changed:
        changed = False
        for rel, rec in info.items():
            if (not rec["test_only"] and not has_code[rel]
                    and any(has_code.get(c, False) for c in rec["children"])):
                has_code[rel] = True
                changed = True

    keep = {"": True}
    for rel in sorted(info, key=lambda r: (r.count("/"), r)):
        if rel == "":
            continue
        keep[rel] = keep.get(info[rel]["parent"], False) and has_code.get(rel, False)
    return sorted(rel for rel in info if keep.get(rel, False))


def missing_contracts(root, cfg, excludes=()):
    filename = cfg["target_filename"]
    missing = []
    for rel in expected_directories(root, filename, excludes):
        path = root / rel / filename if rel else root / filename
        if not path.is_file():
            missing.append(rel or ".")
    return missing


def config_problems(root, cfg):
    """Enforce .egmrc against the tree, so the convention is not optional."""
    problems = []
    gi = root / ".gitignore"
    if gi.is_file():
        active = [ln.strip() for ln in gi.read_text(encoding="utf-8").splitlines()
                  if ln.strip() and not ln.strip().startswith("#")]
        art = cfg["artifact_dir"].strip("/")
        if not any(art in ln for ln in active):
            problems.append(
                f".gitignore does not ignore the artifact directory {cfg['artifact_dir']}"
            )
        # The contracts are tracked. An ignore rule for them would empty the
        # CI checkout and make every contract-derived check vacuous.
        for ln in active:
            if ln.startswith("!"):
                continue
            if fnmatch.fnmatch(cfg["target_filename"], ln.lstrip("/").rstrip("/")):
                problems.append(
                    f".gitignore ignores the tracked contract filename "
                    f"{cfg['target_filename']} ({ln})"
                )
                break
    ci = root / ".github" / "workflows" / "ci.yml"
    if ci.is_file():
        text = ci.read_text(encoding="utf-8")
        if cfg["verification"] not in text:
            problems.append(
                f"verification command is not the one CI runs ({cfg['verification']})"
            )
    # The prune policy is declared in .egmrc and implemented as
    # SOURCE_SUFFIXES. Assert they agree, so a change to the bootstrap's
    # prune list cannot silently change the derived set without a review.
    declared = {s.strip().lower() for s in cfg["source_extensions"].split(",")
                if s.strip()}
    if declared != SOURCE_SUFFIXES:
        missing = sorted(SOURCE_SUFFIXES - declared)
        extra = sorted(declared - SOURCE_SUFFIXES)
        problems.append(
            f".egmrc source_extensions does not match the checker's prune "
            f"policy (missing {missing}, extra {extra})"
        )
    return problems


def readme_problems(root, excludes=()):
    """A bootstrap run with the default README.md target leaves this shape."""
    problems = []
    for dirpath, _dirnames, filenames in walk_dirs(root, excludes):
        if "README.md" not in filenames:
            continue
        path = Path(dirpath) / "README.md"
        text = path.read_text(encoding="utf-8")
        if (PEANUT_HEADING.search(text)
                and re.search(r"^## Invariants", text, re.M)
                and re.search(r"^## Verification", text, re.M)):
            rel = path.relative_to(root).as_posix()
            problems.append(
                f"{rel}: README.md has the shape of an EGM contract "
                "(a bootstrap used the wrong target_filename?)"
            )
    return problems


PAIRS_REL = ".egm/PAIRS.md"


def platform_pairs(root, excludes=()):
    """The `.sh`/`.ps1` pairs the repository actually tracks.

    Reading `git ls-files` rather than walking the disk is what makes the
    local working tree and a fresh CI checkout agree: gitignored scratch
    such as `pgo.sh` exists here and not there, and a check that saw it in
    one place and not the other would be a check that can drift apart.
    A throwaway self-test tree has no git, so it falls back to the walk.
    """
    listed = None
    if (root / ".git").exists():
        try:
            listed = subprocess.run(
                ["git", "ls-files", "-z"], cwd=root,
                capture_output=True, timeout=30,
            )
        except (OSError, subprocess.SubprocessError):
            listed = None
    if listed is not None and listed.returncode == 0 and listed.stdout:
        names = set(listed.stdout.decode("utf-8", "replace").split("\0"))
        return sorted(
            (rel, rel[:-3] + ".ps1")
            for rel in names
            if rel.endswith(".sh") and rel[:-3] + ".ps1" in names
        )
    pairs = []
    for dirpath, _dirnames, filenames in walk_dirs(root, excludes):
        for name in filenames:
            if not name.endswith(".sh"):
                continue
            twin = Path(dirpath) / (name[:-3] + ".ps1")
            if not twin.is_file():
                continue
            pairs.append((
                (Path(dirpath) / name).relative_to(root).as_posix(),
                twin.relative_to(root).as_posix(),
            ))
    return sorted(pairs)


def pairs_problems(root, excludes=()):
    """Hold the tracked platform-pair table against the tree.

    rgy's AGENTS.md is deliberately gitignored (§8), so the table lives in
    a tracked file instead: a checker that read a file absent from a fresh
    CI checkout would be a vacuous gate.
    """
    problems = []
    notes = root / PAIRS_REL
    if not notes.is_file():
        return [f"missing {PAIRS_REL} (tracked platform-pair table)"]
    listed = set()
    for line in notes.read_text(encoding="utf-8").splitlines():
        if not (line.startswith("|") and ".sh" in line and ".ps1" in line):
            continue
        paths = TABLE_PATH.findall(line)
        if not paths:
            continue
        for rel in paths:
            listed.add(rel)
            if not (root / rel).is_file():
                problems.append(f"{PAIRS_REL} lists a missing script ({rel})")
        if not any(p.endswith(".sh") for p in paths) or not any(p.endswith(".ps1") for p in paths):
            problems.append(f"{PAIRS_REL} row does not name both halves of a pair")
    for rel_sh, rel_ps in platform_pairs(root, excludes):
        if rel_sh not in listed or rel_ps not in listed:
            problems.append(f"unlisted .sh/.ps1 pair ({rel_sh}, {rel_ps})")
    return problems


CONTRACTS_INDEX_HEADER = (
    "# EGM contract index -- generated by check-egm-contracts.py --reindex\n"
    "# One row per directory that owes a contract: <relative-dir> <sha256>\n"
    "# Hashes are computed with CRLF normalised to LF, so a Windows checkout\n"
    "# and a Unix checkout produce the same digest.\n"
)


def contract_digest(path):
    """Hash a contract with line endings normalised, for cross-platform parity."""
    raw = path.read_bytes().replace(b"\r\n", b"\n")
    return hashlib.sha256(raw).hexdigest()


def within(base, rel):
    """True when rel resolves to a path inside base (no .. or absolute escape).

    Both the status manifest and the index name paths that the checker reads
    back. Resolve them first so a hand-edited row cannot reach outside the
    tree, including through a symlink.
    """
    try:
        (base / rel).resolve().relative_to(base.resolve())
    except (ValueError, OSError):
        return False
    return True


def contract_rows(root, cfg):
    """The derived facts: which directories owe a contract, and their hashes."""
    filename = cfg["target_filename"]
    rows = []
    for rel in expected_directories(root, filename, scope_excludes(cfg)):
        path = root / rel / filename if rel else root / filename
        if path.is_file():
            rows.append((rel or ".", contract_digest(path)))
    return sorted(rows)


def reindex(root, cfg):
    """Regenerate CONTRACTS.index from the tree. Run after any contract edit."""
    art = root / cfg["artifact_dir"].strip("/")
    art.mkdir(parents=True, exist_ok=True)
    rows = contract_rows(root, cfg)
    lines = [CONTRACTS_INDEX_HEADER.rstrip("\n")]
    lines.extend(f"{rel}\t{digest}" for rel, digest in rows)
    # write_bytes, not write_text: on Windows write_text translates \n to
    # \r\n, so the same tree would produce a different index on each host.
    # The index is tracked, and the two hosts must be able to agree on it.
    (art / CONTRACTS_INDEX).write_bytes(("\n".join(lines) + "\n").encode("utf-8"))
    return len(rows)


def index_problems(root, cfg):
    """Fail when the tracked index no longer describes the tree."""
    art = root / cfg["artifact_dir"].strip("/")
    path = art / CONTRACTS_INDEX
    label = f"{art.name}/{CONTRACTS_INDEX}"
    if not path.is_file():
        return [f"missing {label} (run --reindex)"]

    indexed = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split("\t")
        if len(parts) != 2 or not re.fullmatch(r"[0-9a-f]{64}", parts[1]):
            return [f"{label} has a malformed row ({line})"]
        rel = parts[0]
        if not within(root, rel):
            return [f"{label} has a row whose path escapes the repository ({rel})"]
        if rel in indexed:
            return [f"{label} has a duplicate row for {rel}"]
        indexed[rel] = parts[1]

    filename = cfg["target_filename"]
    expected = {rel or "." for rel in expected_directories(root, filename, scope_excludes(cfg))}
    problems = []
    for rel in sorted(expected - indexed.keys()):
        problems.append(f"{label} is missing a row for {rel}")
    for rel in sorted(indexed.keys() - expected):
        problems.append(f"{label} has a row for a directory that owes no contract ({rel})")
    for rel in sorted(expected & indexed.keys()):
        contract = root / filename if rel == "." else root / rel / filename
        if not contract.is_file():
            problems.append(f"{label}: {rel} has no {filename} on disk")
        elif contract_digest(contract) != indexed[rel]:
            problems.append(f"{label}: {rel} hash mismatch (contract edited; run --reindex)")
    return problems


DRIFT_STATUS = "DRIFT_STATUS.md"


def drift_problems(root, cfg):
    """Read the tracked status manifest, not a free-form report or a filename.

    The dated reports are an audit trail; the manifest is the machine-readable
    decision about them. A human or agent updates it after a drift pass. The
    status must name the newest dated report, so clearing `unresolved` without
    a new report leaves the gate red.
    """
    art = root / cfg["artifact_dir"].strip("/")
    status = art / DRIFT_STATUS
    label = f"{art.name}/{DRIFT_STATUS}"
    if not status.is_file():
        return [f"missing {label} (drift status manifest)"]
    text = status.read_text(encoding="utf-8")

    stamp = re.search(r"^date:\s*(\d{4})-(\d{2})-(\d{2})\s*$", text, re.M)
    if not stamp:
        return [f"{label} has no ISO 'date: YYYY-MM-DD' line"]
    try:
        stamped_date = date(int(stamp.group(1)), int(stamp.group(2)),
                            int(stamp.group(3)))
    except ValueError:
        return [f"{label} has an invalid date"]
    stamped = stamp.group(0).split(":", 1)[1].strip()
    # A date decades out cannot describe a review that happened. Tolerate a
    # month of clock skew rather than depending on an exact wall clock; this
    # is a sanity bound, not a security control -- intent is still reviewed.
    if stamped_date > date.today() + timedelta(days=30):
        return [f"{label} is dated implausibly far in the future ({stamped})"]

    count = re.search(r"^unresolved:\s*(\d+)\s*$", text, re.M)
    if not count:
        return [f"{label} has no 'unresolved: N' line"]
    unresolved = int(count.group(1))

    # The decision names the durable note where the finding was resolved.
    # Machinery cannot verify intent, but it can require that the named
    # place exists, stays inside the repository, and that a clean status is
    # not left as "pending".
    decision = re.search(r"^decision:\s*(\S+)\s*$", text, re.M)
    if not decision:
        return [f"{label} has no 'decision: <path>' line"]
    d = decision.group(1)
    if unresolved == 0:
        if d == "pending":
            return [f"{label} has no recorded decision (unresolved is 0)"]
        if not within(root, d):
            return [f"{label} names a decision whose path escapes the repository ({d})"]
        if not (root / d).is_file():
            return [f"{label} names a decision file that does not exist ({d})"]

    report = re.search(r"^report:\s*(\S+)\s*$", text, re.M)
    if report:
        rel = report.group(1)
        # `report:` is root-relative (it already names .egm/), so resolve it
        # from root and require it to stay inside the repository and inside
        # the artifact directory.
        if not within(root, rel):
            return [f"{label} names a report whose path escapes the repository ({rel})"]
        try:
            (root / rel).resolve().relative_to(art.resolve())
        except (ValueError, OSError):
            return [f"{label} names a report outside {art.name}/ ({rel})"]
        if not (root / rel).is_file():
            return [f"{label} names a missing report ({rel})"]
        named = re.search(r"(\d{4}-\d{2}-\d{2})_DRIFT_REPORT\.md$", rel)
        if not named:
            return [f"{label} names a report that is not a dated drift report ({rel})"]
        if named.group(1) != stamped:
            return [f"{label} date {stamped} disagrees with the report name ({rel})"]
        newest = sorted(art.glob("*_DRIFT_REPORT.md"), key=lambda p: p.name)
        if newest and Path(rel).name != newest[-1].name:
            return [f"{label} names {rel}, but the newest dated report is "
                    f"{newest[-1].name}"]

    if unresolved:
        return [f"{label} reports {unresolved} unresolved drift findings"]
    return []


def check(path, root, cfg):
    problems = []
    rel = path.relative_to(root).as_posix()
    text = path.read_text(encoding="utf-8")
    filename = cfg["target_filename"]

    if PEANUT_HEADING.search(text):
        for section in REQUIRED_PEANUT_SECTIONS:
            if not re.search(r"^## " + re.escape(section) + r"\s*$", text, re.M):
                problems.append(f"missing ## {section}")
        runs = RUN_LINE.findall(text)
        if not runs:
            problems.append("missing - Run: line in ## Verification")
        for command in runs:
            if command != cfg["verification"]:
                problems.append(f"Run != verification ({command})")
    elif BLUEPRINT_HEADINGS.search(text):
        # A Blueprint (Hay) has no direct source, so it carries no
        # Interface / Invariants / Side Effects / Verification sections.
        pass
    else:
        problems.append("no Interface or Blueprint heading")

    for target in LINK.findall(text):
        target = target.strip()
        if target.startswith(("http://", "https://", "mailto:", "#")):
            continue
        target = target.split("#", 1)[0]
        if not target:
            continue
        if "README.md" in target:
            problems.append(f"links to README.md ({target})")
        elif (target.lower().endswith(".md")
              and not (target.endswith(filename) or target.endswith("SKILL.md"))):
            problems.append(f"links to a non-contract markdown file ({target})")
        if not (path.parent / target).exists():
            problems.append(f"broken link ({target})")

    fences = sum(1 for line in text.splitlines() if line.startswith("```"))
    if fences % 2:
        problems.append("unbalanced code fences")
    if PLACEHOLDER.search(text):
        problems.append("placeholder text")
    if LINE_CITATION.search(text):
        problems.append("line-number citation (use file:symbol)")

    return rel, problems


# --- Phase 2: the structural skeleton, derived from the source tree ---------

SECTION_HEADING = re.compile(r"^##[ \t]+(.+?)[ \t]*$", re.M)


def _sections(text):
    """Yield (title, body) for each level-2 `## ` heading, in order."""
    heads = list(SECTION_HEADING.finditer(text))
    for i, m in enumerate(heads):
        start = m.end()
        end = heads[i + 1].start() if i + 1 < len(heads) else len(text)
        yield m.group(1), text[start:end]


def section_body(text, name):
    """Body of the first level-2 section whose title starts with `name`."""
    for title, body in _sections(text):
        if title == name or title.startswith(name):
            return body
    return None


def _children_map(expected):
    """Map each directory rel to the sorted names of its immediate children
    that are themselves in `expected` (the expected_directories() set)."""
    out = {}
    for rel in expected:
        if not rel:
            continue
        parent, _, name = rel.rpartition("/")
        out.setdefault(parent, []).append(name)
    return {k: sorted(v) for k, v in out.items()}


def generated_children(root, cfg, rel, excludes):
    """Sorted IMMEDIATE child directory names of `rel` that themselves owe a
    contract (are in expected_directories). `rel` is "" for the root.

    These are the `](./<child>/<target>)` links a contract for `rel` must
    carry. The scope excludes are applied by expected_directories, so a child
    inside a vendored/excluded tree is never derived and is never required.
    """
    expected = expected_directories(root, cfg["target_filename"], excludes)
    return _children_map(expected).get(rel, [])


def generated_problems(root, cfg):
    """Hold every tracked contract against the tree-derived skeleton.

    The set of `](./<child>/<target>)` links a contract declares is compared
    with the immediate children that owe a contract. Such links conventionally
    sit under `## Subdirectories & Boundaries`, but a Peanut may roll a child
    up in `## Interface` (scripts/EGM.md does), so the whole contract is
    scanned: any `./<child>/<target>` link is a declared child wherever it
    appears. Exactly one path segment is an immediate child; a grandchild link
    such as ./scripts/lib/EGM.md is ignored. Blueprints and leaf Peanuts with
    no derived children need no links at all, so links are only required when
    derived children exist. The Verification Run line is checked as a second,
    structural gate, with a message distinct from check()'s "Run != ...".
    """
    excludes = scope_excludes(cfg)
    filename = cfg["target_filename"]
    cmap = _children_map(expected_directories(root, filename, excludes))
    child_link = re.compile(
        r"\]\(\./([^/)\s]+)/" + re.escape(filename) + r"(?:#[^)]*)?\)"
    )
    problems = []
    for path in sorted(contracts(root, filename, excludes)):
        file_rel = path.relative_to(root).as_posix()
        dir_rel = path.parent.relative_to(root).as_posix()
        if dir_rel == ".":
            dir_rel = ""
        text = path.read_text(encoding="utf-8")

        derived = set(cmap.get(dir_rel, []))
        linked = set(child_link.findall(text))
        for child in sorted(derived - linked):
            problems.append(
                f"{file_rel}: missing child link ./{child}/{filename} "
                f"(derived from the source tree)"
            )
        for child in sorted(linked - derived):
            problems.append(
                f"{file_rel}: stale child link ./{child}/{filename} "
                f"(the directory owes no contract)"
            )

        verification = section_body(text, "Verification")
        if verification is not None:
            for command in RUN_LINE.findall(verification):
                if command != cfg["verification"]:
                    problems.append(
                        f"{file_rel}: Verification Run line disagrees with "
                        f".egmrc verification ({command})"
                    )
    return problems


def skeleton(rel, cfg, children):
    """A deterministic scratch contract: title, one derived child link per
    child, and the exact Verification Run line from .egmrc."""
    filename = cfg["target_filename"]
    lines = [f"# {rel or 'ROOT'}", "", "## Subdirectories & Boundaries", ""]
    for child in children:
        lines.append(f"- {child}: [{child}](./{child}/{filename})")
    lines += ["", "## Verification", "",
              f"- Run: `{cfg['verification']}`", ""]
    return "\n".join(lines)


def generate(root, cfg):
    """Write a skeleton for every directory that owes a contract into
    <artifact_dir>/generated/. That scratch lives under a hidden, pruned,
    gitignored directory, so contracts()/expected_directories() never see it.
    The root (rel "") is placed in a ROOT/ subdirectory."""
    excludes = scope_excludes(cfg)
    filename = cfg["target_filename"]
    gen = root / cfg["artifact_dir"].strip("/") / "generated"
    expected = expected_directories(root, filename, excludes)
    cmap = _children_map(expected)
    for rel in expected:
        dest = gen / (rel or "ROOT") / filename
        dest.parent.mkdir(parents=True, exist_ok=True)
        # LF on every host: the generated set must be byte-comparable
        # between Windows and Linux, not just semantically equivalent.
        dest.write_bytes(skeleton(rel, cfg, cmap.get(rel, [])).encode("utf-8"))
    return len(expected)


def generated_check(root, cfg):
    """Run only generated_problems; print the EGM GENERATED verdict."""
    problems = generated_problems(root, cfg)
    checked = sum(1 for _ in contracts(root, cfg["target_filename"],
                                       scope_excludes(cfg)))
    for problem in problems:
        print(f"  FAIL generated: {problem}")
    print(f"EGM GENERATED: checked={checked} failed={len(problems)}")
    return 1 if problems else 0


def validate(root, cfg):
    """Return (checked, problems); problems are fully formatted 'rel: issue'.

    There is one check for every tree: contracts are tracked, so a CI
    checkout and a working tree have the same inputs and the same result.
    """
    problems = []
    checked = 0
    excludes = scope_excludes(cfg)
    for path in sorted(contracts(root, cfg["target_filename"], excludes)):
        checked += 1
        rel, issues = check(path, root, cfg)
        problems.extend(f"{rel}: {issue}" for issue in issues)
    for rel in missing_contracts(root, cfg, excludes):
        problems.append(f"{rel}: missing contract {cfg['target_filename']}")
    problems.extend(f"config: {p}" for p in config_problems(root, cfg))
    problems.extend(f"readme: {p}" for p in readme_problems(root, excludes))
    problems.extend(f"pairs: {p}" for p in pairs_problems(root, excludes))
    problems.extend(f"drift: {p}" for p in drift_problems(root, cfg))
    problems.extend(f"index: {p}" for p in index_problems(root, cfg))
    problems.extend(f"generated: {p}" for p in generated_problems(root, cfg))
    return checked, problems


BLUEPRINT = """# root

## System Overview

The root of a test tree.

## Subdirectories & Boundaries

- pkg: [pkg](./pkg/EGM.md) — the package.
"""


def peanut(body="", run="cargo test --workspace --no-fail-fast --locked"):
    text = """# pkg

## Overview

A package.

## Interface

- `pub fn f()` — a function.

## Invariants

- One.

## Side Effects

- None.

## Verification

"""
    if run is not None:
        text += f"- Run: `{run}`\n"
    return text + body


PRUNE_VALUE = ",".join(sorted(SOURCE_SUFFIXES))
CONFIG = ("target_filename=EGM.md\n"
          "artifact_dir=.egm\n"
          "verification=cargo test --workspace --no-fail-fast --locked\n"
          "scope=exclude:crates/rgx-*,crates/rar-codec\n"
          f"source_extensions={PRUNE_VALUE}\n")
PAIR_TABLE = """# notes

## The platform pairs

| script | what it does |
|---|---|
| `scripts/fake.sh`, `scripts/fake.ps1` | a pair |
"""
PAIRS_EMPTY = """# The platform pairs

Tracked pair table. No pairs are listed in this test tree.

| script | what it does |
|---|---|
"""
DRIFT_STATUS_CLEAN = ("# EGM drift status\n\n"
                      "date: 2026-01-02\n"
                      "report: .egm/2026-01-02_DRIFT_REPORT.md\n"
                      "unresolved: 0\n"
                      "decision: .egm/PAIRS.md\n")
DRIFT_REPORT_CLEAN = "# report\n\nNo drift detected\n"
GITIGNORE_GOOD = ("# scratch\n/.egm/*\n"
                  "!/.egm/*_DRIFT_REPORT.md\n"
                  "!/.egm/DRIFT_STATUS.md\n"
                  "!/.egm/CONTRACTS.index\n"
                  "!/.egm/PAIRS.md\n")


def self_test():
    """Prove every rule actually fires, on throwaway trees."""
    import tempfile

    results = []

    def case(name, files, expected, config=CONFIG, index=True, forbidden=()):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            (root / ".egmrc").write_text(config, encoding="utf-8")
            for rel, text in files.items():
                path = root / rel
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(text, encoding="utf-8")
            try:
                cfg = read_config(root / ".egmrc")
            except ValueError as exc:
                blob = f"config error: {exc}"
            else:
                if index and f".egm/{CONTRACTS_INDEX}" not in files:
                    reindex(root, cfg)
                _, problems = validate(root, cfg)
                blob = "\n".join(problems)
            ok = (all(e in blob for e in expected)
                  and not any(f in blob for f in forbidden))
            results.append((name, ok, blob))

    base = {
        "EGM.md": BLUEPRINT,
        "pkg/EGM.md": peanut(),
        "pkg/lib.rs": "pub fn f() {}\n",
        ".egm/DRIFT_STATUS.md": DRIFT_STATUS_CLEAN,
        ".egm/2026-01-02_DRIFT_REPORT.md": DRIFT_REPORT_CLEAN,
        ".egm/PAIRS.md": PAIRS_EMPTY,
    }

    def without(files, *keys):
        out = dict(files)
        for key in keys:
            out.pop(key, None)
        return out

    # A valid index for the base tree, used to build deliberate corruptions.
    with tempfile.TemporaryDirectory() as td:
        scratch = Path(td)
        (scratch / ".egmrc").write_text(CONFIG, encoding="utf-8")
        for rel, text in base.items():
            p = scratch / rel
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(text, encoding="utf-8")
        reindex(scratch, read_config(scratch / ".egmrc"))
        VALID_INDEX = (scratch / ".egm" / CONTRACTS_INDEX).read_text(encoding="utf-8")

    STALE_INDEX = re.sub(r"[0-9a-f]{64}", "0" * 64, VALID_INDEX, count=1)
    MISSING_ROW_INDEX = "\n".join(
        line for line in VALID_INDEX.splitlines() if not line.startswith("pkg\t")
    ) + "\n"
    EXTRA_ROW_INDEX = VALID_INDEX + "ghost\t" + "0" * 64 + "\n"
    FIRST_ROW = next(line for line in VALID_INDEX.splitlines() if "\t" in line)
    DUPLICATE_INDEX = VALID_INDEX + FIRST_ROW + "\n"
    ESCAPING_INDEX = VALID_INDEX + "../evil\t" + "0" * 64 + "\n"

    case("valid tree", base, [])
    case("missing section", dict(base, **{"pkg/EGM.md": peanut().replace("## Side Effects\n\n- None.\n\n", "")}),
         ["missing ## Side Effects"])
    case("empty verification", dict(base, **{"pkg/EGM.md": peanut(run=None)}),
         ["missing - Run: line"])
    case("wrong verification", dict(base, **{"pkg/EGM.md": peanut(run="cargo test --workspace")}),
         ["Run != verification"])
    case("broken link", dict(base, **{"EGM.md": BLUEPRINT.replace("./pkg/EGM.md", "./nope/EGM.md")}),
         ["broken link"])
    case("README link", dict(base, **{"EGM.md": BLUEPRINT.replace("./pkg/EGM.md", "./README.md"),
                                     "README.md": "# readme\n"}),
         ["links to README.md"])
    case("other markdown link", dict(base, **{"EGM.md": BLUEPRINT.replace("./pkg/EGM.md", "./status.md"),
                                              "status.md": "# status\n"}),
         ["non-contract markdown"])
    case("unbalanced fence", dict(base, **{"pkg/EGM.md": peanut() + "\n```rust\n"}),
         ["unbalanced code fences"])
    case("placeholder", dict(base, **{"pkg/EGM.md": peanut() + "\nDetails pending\n"}),
         ["placeholder text"])
    case("line citation", dict(base, **{"pkg/EGM.md": peanut() + "\nSee src/lib.rs:1234\n"}),
         ["line-number citation"])
    case("missing contract", dict(base, **{"orphan/lib.rs": "pub fn g() {}\n"}),
         ["missing contract EGM.md"])
    case("excluded vendored tree owes no contract",
         dict(base, **{"crates/rgx-core/lib.rs": "pub fn f() {}\n"}), [])
    case("contract inside excluded vendored tree is not checked",
         dict(base, **{"crates/rgx-core/EGM.md":
                       peanut().replace("## Side Effects\n\n- None.\n\n", "")}), [])
    case("README.md as target", base, ["config error", "must not be README.md"],
         config="target_filename=README.md\nartifact_dir=.egm\n"
                "verification=cargo test --workspace --no-fail-fast --locked\n"
                "scope=exclude:crates/rgx-*,crates/rar-codec\n"
                f"source_extensions={PRUNE_VALUE}\n")
    case("missing artifact_dir", base, ["config error", "artifact_dir"],
         config="target_filename=EGM.md\n"
                "verification=cargo test --workspace --no-fail-fast --locked\n"
                "scope=exclude:crates/rgx-*,crates/rar-codec\n"
                f"source_extensions={PRUNE_VALUE}\n")
    case("missing scope", base, ["config error", "scope"],
         config="target_filename=EGM.md\nartifact_dir=.egm\n"
                "verification=cargo test --workspace --no-fail-fast --locked\n"
                f"source_extensions={PRUNE_VALUE}\n")
    case("malformed scope", base, ["config error", "scope must be"],
         config="target_filename=EGM.md\nartifact_dir=.egm\n"
                "verification=cargo test --workspace --no-fail-fast --locked\n"
                "scope=nonsense\n"
                f"source_extensions={PRUNE_VALUE}\n")
    case("config drift from CI", dict(base, **{".github/workflows/ci.yml": "run: cargo test --workspace\n"}),
         ["verification command is not the one CI runs"])
    case("README shaped like a contract", dict(base, **{"README.md": peanut()}),
         ["README.md has the shape of an EGM contract"])
    case("pair table names a missing script", dict(base, **{".egm/PAIRS.md": PAIR_TABLE}),
         ["lists a missing script"])
    case("unlisted pair", dict(base, **{".egm/PAIRS.md": "# notes\n\n## 1.1\n\nnothing\n",
                                        "foo.sh": "#!/bin/sh\n", "foo.ps1": "# ps\n"}),
         ["unlisted .sh/.ps1 pair"])
    case("missing tracked pair table", without(base, ".egm/PAIRS.md"),
         ["missing .egm/PAIRS.md"])
    case("unresolved drift status",
         dict(base, **{".egm/DRIFT_STATUS.md":
                       DRIFT_STATUS_CLEAN.replace("unresolved: 0", "unresolved: 2")}),
         ["reports 2 unresolved drift findings"])
    case("missing drift status", without(base, ".egm/DRIFT_STATUS.md"),
         ["missing .egm/DRIFT_STATUS.md"])
    case("drift status without a count",
         dict(base, **{".egm/DRIFT_STATUS.md": "# status\n\ndate: 2026-01-02\n"}),
         ["has no 'unresolved: N' line"])
    case("drift status names a missing report",
         dict(base, **{".egm/DRIFT_STATUS.md":
                       DRIFT_STATUS_CLEAN.replace("2026-01-02_DRIFT_REPORT.md", "nope.md")}),
         ["names a missing report"])
    case("drift status date disagrees with the report name",
         dict(base, **{".egm/DRIFT_STATUS.md":
                       DRIFT_STATUS_CLEAN.replace("date: 2026-01-02", "date: 2026-01-03")}),
         ["disagrees with the report name"])
    case("drift status names an undated report",
         dict(base, **{".egm/DRIFT_STATUS.md":
                       DRIFT_STATUS_CLEAN.replace(".egm/2026-01-02_DRIFT_REPORT.md", ".egm/notes.md"),
                       ".egm/notes.md": "# notes\n"}),
         ["not a dated drift report"])
    case("drift status with an invalid date",
         dict(base, **{".egm/DRIFT_STATUS.md":
                       DRIFT_STATUS_CLEAN.replace("2026-01-02", "2026-13-45")}),
         ["invalid date"])
    case("gitignore ignores tracked contracts",
         dict(base, **{".gitignore": GITIGNORE_GOOD + "EGM.md\n"}),
         ["ignores the tracked contract filename"])
    case("artifact directory not ignored",
         dict(base, **{".gitignore": "# nothing\n"}),
         ["does not ignore the artifact directory"])
    case("status names a stale drift report",
         dict(base, **{".egm/2026-01-03_DRIFT_REPORT.md": DRIFT_REPORT_CLEAN}),
         ["newest dated report"])
    case("missing contract index", base,
         [f"missing .egm/{CONTRACTS_INDEX}"], index=False)
    case("stale contract index",
         dict(base, **{f".egm/{CONTRACTS_INDEX}": STALE_INDEX}),
         ["hash mismatch"])
    case("index missing a contract row",
         dict(base, **{f".egm/{CONTRACTS_INDEX}": MISSING_ROW_INDEX}),
         ["missing a row for pkg"])
    case("index has a row for a dead directory",
         dict(base, **{f".egm/{CONTRACTS_INDEX}": EXTRA_ROW_INDEX}),
         ["owes no contract"])
    case("malformed index row",
         dict(base, **{f".egm/{CONTRACTS_INDEX}": "# header\nnot-a-row\n"}),
         ["malformed row"])

    # --- Phase 2: the tree-derived structural skeleton --------------------
    # (a) a tracked contract missing a required child link.
    NO_LINK_BLUEPRINT = BLUEPRINT.replace("[pkg](./pkg/EGM.md)", "pkg")
    case("generated: missing required child link",
         dict(base, **{"EGM.md": NO_LINK_BLUEPRINT}),
         ["missing child link"])
    # (b) a stale child link to a directory that owes no contract. ghost's
    # EGM.md exists so the only new failure is the generated stale link.
    case("generated: stale child link to a dir that owes no contract",
         dict(base, **{
             "EGM.md": BLUEPRINT + "- ghost: [ghost](./ghost/EGM.md)\n",
             "ghost/EGM.md": peanut(),
         }),
         ["stale child link"])
    # (c) a child inside a scope-excluded vendored tree is NOT required: it
    # must not create a derived child (so no generated: problem at all).
    case("generated: excluded vendored child is not required",
         dict(base, **{"crates/rgx-core/pkg/lib.rs": "pub fn f() {}\n"}),
         [], forbidden=["generated:", "missing contract"])
    # (d) the Verification Run line is a distinct generated: gate.
    case("generated: Verification Run line disagreement",
         dict(base, **{"pkg/EGM.md": peanut(run="cargo test --workspace")}),
         ["generated:", "Verification Run line disagrees"])

    # --- Ported hardening: the audit gaps and the four boundaries ----------

    case("hash mismatch names the --reindex fix",
         dict(base, **{f".egm/{CONTRACTS_INDEX}": STALE_INDEX}),
         ["hash mismatch", "run --reindex"])
    case("index row path escapes the repository",
         dict(base, **{f".egm/{CONTRACTS_INDEX}": ESCAPING_INDEX}),
         ["escapes the repository"])
    case("duplicate index row",
         dict(base, **{f".egm/{CONTRACTS_INDEX}": DUPLICATE_INDEX}),
         ["duplicate row"])
    case("status names a report whose path escapes the repository",
         dict(base, **{".egm/DRIFT_STATUS.md":
                       DRIFT_STATUS_CLEAN.replace(".egm/2026-01-02_DRIFT_REPORT.md",
                                                  "../2026-01-02_DRIFT_REPORT.md")}),
         ["escapes the repository"])
    case("status names a report outside the artifact directory",
         dict(base, **{".egm/DRIFT_STATUS.md":
                       DRIFT_STATUS_CLEAN.replace(".egm/2026-01-02_DRIFT_REPORT.md",
                                                  "2026-01-02_DRIFT_REPORT.md")}),
         ["outside .egm"])
    case("prune policy declared in .egmrc disagrees with the checker",
         base, ["does not match the checker's prune policy"],
         config=CONFIG.replace(".zig", ""))
    case("config is missing source_extensions",
         base, ["config error", "source_extensions"],
         config=("target_filename=EGM.md\n"
                 "artifact_dir=.egm\n"
                 "verification=cargo test --workspace --no-fail-fast --locked\n"
                 "scope=exclude:crates/rgx-*,crates/rar-codec\n"))
    case("status has no decision",
         dict(base, **{".egm/DRIFT_STATUS.md":
                       DRIFT_STATUS_CLEAN.replace("decision: .egm/PAIRS.md\n", "")}),
         ["no 'decision"])
    case("status names a decision file that does not exist",
         dict(base, **{".egm/DRIFT_STATUS.md":
                       DRIFT_STATUS_CLEAN.replace("decision: .egm/PAIRS.md",
                                                  "decision: missing.md")}),
         ["does not exist"])
    case("status names a decision whose path escapes the repository",
         dict(base, **{".egm/DRIFT_STATUS.md":
                       DRIFT_STATUS_CLEAN.replace("decision: .egm/PAIRS.md",
                                                  "decision: ../outside.md")}),
         ["escapes the repository"])
    case("clean status still says pending",
         dict(base, **{".egm/DRIFT_STATUS.md":
                       DRIFT_STATUS_CLEAN.replace("decision: .egm/PAIRS.md",
                                                  "decision: pending")}),
         ["no recorded decision"])
    case("status dated implausibly far in the future",
         dict(base, **{".egm/DRIFT_STATUS.md":
                       DRIFT_STATUS_CLEAN.replace("date: 2026-01-02",
                                                  "date: 2099-01-01")}),
         ["implausibly far in the future"])

    # --fix heals a stale index and the tree then validates, in one step.
    import contextlib
    import io
    with tempfile.TemporaryDirectory() as td:
        scratch = Path(td)
        (scratch / ".egmrc").write_text(CONFIG, encoding="utf-8")
        stale = dict(base, **{f".egm/{CONTRACTS_INDEX}": STALE_INDEX})
        for rel, text in stale.items():
            p = scratch / rel
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(text, encoding="utf-8")
        cfg = read_config(scratch / ".egmrc")
        _, before = validate(scratch, cfg)
        with contextlib.redirect_stdout(io.StringIO()):
            rc = main(["--root", str(scratch), "--fix"])
        _, after = validate(scratch, cfg)
        results.append(("--fix regenerates a stale index and the tree validates",
                        bool(before) and rc == 0 and not after,
                        "\n".join(before + after)))

    # A lockfile must be byte-stable: regenerating it twice changes nothing.
    with tempfile.TemporaryDirectory() as td:
        scratch = Path(td)
        (scratch / ".egmrc").write_text(CONFIG, encoding="utf-8")
        for rel, text in base.items():
            p = scratch / rel
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(text, encoding="utf-8")
        cfg = read_config(scratch / ".egmrc")
        reindex(scratch, cfg)
        first = (scratch / ".egm" / CONTRACTS_INDEX).read_bytes()
        reindex(scratch, cfg)
        second = (scratch / ".egm" / CONTRACTS_INDEX).read_bytes()
        results.append(("reindex is idempotent and byte-stable",
                        first == second, ""))

    failures = [name for name, ok, _ in results if not ok]
    for name, ok, blob in results:
        print(f"  {'PASS' if ok else 'FAIL'} {name}")
        if not ok:
            print(f"       got: {blob[:400]}")
    print(f"EGM CONTRACTS SELF-TEST: cases={len(results)} failed={len(failures)}")
    return 1 if failures else 0


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    default_root = Path(__file__).resolve().parent.parent
    parser.add_argument("--root", default=str(default_root), help="repository root")
    parser.add_argument("--self-test", action="store_true",
                        help="build throwaway trees and prove every rule fires")
    parser.add_argument("--reindex", action="store_true",
                        help="regenerate CONTRACTS.index from the tree; run this "
                             "after any contract edit or directory rename")
    parser.add_argument("--generate", action="store_true",
                         help="write deterministic skeleton contracts into "
                              "<artifact_dir>/generated/ (gitignored scratch)")
    parser.add_argument("--generated-check", action="store_true",
                         help="run only the tree-derived skeleton check")
    parser.add_argument("--fix", action="store_true",
                         help="regenerate CONTRACTS.index when it is stale or "
                              "missing, then re-validate (explicit opt-in; "
                              "CI never uses it)")
    args = parser.parse_args(argv)

    if args.self_test:
        return self_test()

    root = Path(args.root).resolve()
    try:
        cfg = read_config(root / ".egmrc")
    except (OSError, ValueError) as exc:
        print(f"EGM CONTRACTS: config error: {exc}", file=sys.stderr)
        return 2

    if args.reindex:
        n = reindex(root, cfg)
        print(f"EGM CONTRACTS: reindexed {n} contracts into {cfg['artifact_dir']}/{CONTRACTS_INDEX}")
        return 0

    if args.generate:
        n = generate(root, cfg)
        print(f"EGM CONTRACTS: generated {n} skeleton contracts into "
              f"{cfg['artifact_dir']}/generated/")
        return 0

    if args.generated_check:
        return generated_check(root, cfg)

    if args.fix:
        art = root / cfg["artifact_dir"].strip("/")
        index = art / CONTRACTS_INDEX
        before = index.read_bytes() if index.is_file() else None
        n = reindex(root, cfg)
        after = index.read_bytes()
        if before != after:
            print(f"EGM CONTRACTS: --fix regenerated {n} contracts into "
                  f"{cfg['artifact_dir']}/{CONTRACTS_INDEX}")
        else:
            print(f"EGM CONTRACTS: --fix found the index current ({n} contracts)")

    checked, problems = validate(root, cfg)
    for problem in problems:
        print(f"  FAIL {problem}")
    print(f"EGM CONTRACTS: checked={checked} failed={len(problems)}")
    if checked == 0:
        print("  FAIL no contracts found", file=sys.stderr)
        return 1
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
