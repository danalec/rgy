#!/usr/bin/env python3
"""Render the parity matrix (docs/parity.toml) as a Markdown table.

The matrix lives in docs/parity.md, auto-generated so it cannot rot (plan
6.7). Regenerate with:

    python scripts/parity-matrix.py > docs/parity.md

The README links to the matrix rather than carrying it, so the page that
a reader lands on stays short. Emits to stdout; writes nothing.

With --check it is a gate instead of a renderer, and it is what CI runs:

    python scripts/parity-matrix.py --check

It fails when the committed docs/parity.md is not what this file renders
(the table has drifted from its source), and it fails when a row still
carries a G cell. A G cell is work required: the rule is that a feature
ships as N/P on every OS, or as D with a named reason, and a row that
still claims a gap is a row nobody has closed.
"""
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PARITY = ROOT / "docs" / "parity.toml"
RENDERED = ROOT / "docs" / "parity.md"

LEGEND = (
    "**N** native &middot; **P** ported (shared code, platform hook) &middot; "
    "**D** degraded (documented limit) &middot; **G** gap (work required) &middot; "
    "**-** N/A"
)

OS_COLUMNS = ("windows", "linux", "macos")


def render(data) -> str:
    """The Markdown the matrix renders to."""
    rows = data["feature"]
    verified = set(data.get("columns", {}).get("verified", []))

    out = []
    out.append("# rgy — OS parity")
    out.append("")
    out.append("Auto-generated from [`docs/parity.toml`](parity.toml) by")
    out.append("[`scripts/parity-matrix.py`](../scripts/parity-matrix.py) — regenerate with")
    out.append("`python scripts/parity-matrix.py > docs/parity.md`. A `G` cell is a gap")
    out.append("(work required) and fails CI until it is ported or documented as degraded.")
    out.append("")
    out.append(LEGEND)
    out.append("")
    out.append("| # | Feature | Windows | Linux | macOS | Notes |")
    out.append("|---|---------|---------|-------|-------|-------|")

    section = None
    for r in rows:
        # A row's section is implied by its id prefix (1.x discovery, ...).
        sid = r["id"].split(".")[0]
        if sid != section:
            section = sid
            title = data.get("section", {}).get(sid, f"§6.{sid}")
            out.append(f"| | **{title}** | | | | |")
        cells = [r["id"], r["name"], r["windows"], r["linux"], r["macos"], r.get("notes", "")]
        cells = [c.replace("|", "\\|").replace("\n", " ") for c in cells]
        out.append("| " + " | ".join(cells) + " |")

    gaps = [r["id"] for r in rows if "G" in (r["windows"], r["linux"], r["macos"])]
    out.append("")
    out.append(f"<!-- {len(rows)} features; {len(gaps)} carry at least one G (gap) cell: "
               + ", ".join(gaps) + " -->")
    out.append("")
    unverified = [c for c in OS_COLUMNS if c not in verified]
    if unverified:
        display = {"windows": "Windows", "linux": "Linux", "macos": "macOS"}
        names = ", ".join(display.get(c, c) for c in unverified)
        out.append(f"**The {names} column is unverified.** The code is transported and "
                   "compiles there, but it has never run on that OS: there is no "
                   "machine of that kind in the loop. The cell is a claim about the "
                   "design, not about a behaviour anyone has watched.")
    return "\n".join(out) + "\n"


def check(data, text) -> list:
    """What is wrong with the committed matrix, in the order it matters."""
    problems = []

    committed = RENDERED.read_text(encoding="utf-8") if RENDERED.exists() else ""
    if committed.strip() != text.strip():
        problems.append(
            "docs/parity.md is not what docs/parity.toml renders. Regenerate it:\n"
            "    python scripts/parity-matrix.py > docs/parity.md"
        )

    rows = data["feature"]
    gaps = [(r["id"], r["name"]) for r in rows if "G" in (r["windows"], r["linux"], r["macos"])]
    known = {(g["id"], g["os"]) for g in data.get("gap", [])}

    for r in rows:
        for os_name in OS_COLUMNS:
            if r[os_name] != "G":
                continue
            if (r["id"], os_name) not in known:
                problems.append(
                    f"row {r['id']} ({r['name']}) carries a G (gap) cell for "
                    f"{os_name} and is not on the known-gaps list. A new gap is a "
                    "failure: close it, document the limit, or add it to the list "
                    "in docs/parity.toml with the reason it is still open."
                )

    for gap in data.get("gap", []):
        row = next((r for r in rows if r["id"] == gap["id"]), None)
        if row is None:
            problems.append(
                f"the known-gaps list names row {gap['id']}, which the matrix does "
                "not have. Remove the entry."
            )
        elif row[gap["os"]] != "G":
            problems.append(
                f"row {gap['id']} ({row['name']}) is listed as a gap for "
                f"{gap['os']} but its cell is {row[gap['os']]}: it was closed. "
                "Remove the entry from the known-gaps list."
            )
    return problems


def main() -> int:
    as_gate = "--check" in sys.argv[1:]
    data = tomllib.loads(PARITY.read_text(encoding="utf-8"))
    text = render(data)

    if not as_gate:
        sys.stdout.write(text)
        return 0

    problems = check(data, text)
    for p in problems:
        print(p, file=sys.stderr)
    if problems:
        print(f"\nparity check FAILED: {len(problems)} problem(s)", file=sys.stderr)
        return 1
    gaps = [r["id"] for r in data["feature"] if "G" in (r["windows"], r["linux"], r["macos"])]
    print(f"parity check passed: the matrix is current, and the {len(gaps)} known gap(s) "
          f"({', '.join(gaps)}) are listed with their reasons")
    return 0


if __name__ == "__main__":
    sys.exit(main())
