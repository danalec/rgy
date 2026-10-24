#!/usr/bin/env python3
"""Render the Listary parity matrix (docs/listary-parity.toml) as Markdown.

The matrix is the implementation plan: one row per capability recovered
from the reverse engineering of Listary, one column per side, and for
every gap the work that closes it. It lives in docs/listary-parity.md,
auto-generated so it cannot rot. Regenerate with:

    python scripts/listary-parity.py > docs/listary-parity.md

Emits to stdout; writes nothing.

With --check it is a gate instead of a renderer, and it is what CI runs:

    python scripts/listary-parity.py --check

It fails when the committed docs/listary-parity.md is not what this file
renders, and it fails on a row that cannot be acted on:

  * a row with no status, phase, size or evidence;
  * a G (gap) or D (degraded) row with no `work` -- a gap without a plan
    is a gap nobody has scheduled;
  * a P (parity) row with no `rgy` -- a parity claim without a place in
    the code is a claim, not a fact;
  * a phase with no rows, or a row in a phase that does not exist;
  * an unknown that names a row that does not exist.

The gate does not fail because a gap is open. A gap is the plan. It fails
when a gap has no work attached to it.
"""
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PARITY = ROOT / "docs" / "listary-parity.toml"
RENDERED = ROOT / "docs" / "listary-parity.md"

STATUSES = ("P", "D", "G")
SIZES = ("S", "M", "L")

STATUS_MEANING = {
    "P": "parity",
    "D": "degraded",
    "G": "gap",
}


def _cell(value: str) -> str:
    """One table cell: no raw pipes, no newlines."""
    return str(value).replace("|", "\\|").replace("\n", " ").strip()


def render(data) -> str:
    """The Markdown the matrix renders to."""
    rows = data["capability"]
    phases = data.get("phases", {})
    meta = data.get("meta", {})

    out = []
    out.append("# rgy — Listary parity")
    out.append("")
    out.append("Auto-generated from [`docs/listary-parity.toml`](listary-parity.toml) by")
    out.append("[`scripts/listary-parity.py`](../scripts/listary-parity.py) — regenerate with")
    out.append("`python scripts/listary-parity.py > docs/listary-parity.md`.")
    out.append("")
    out.append("This is the implementation plan. Every capability Listary has, as recovered")
    out.append("by reverse engineering, and where rgy stands against it. A `G` row is a gap:")
    out.append("the `work` column says what closing it means in rgy's terms, and the phase")
    out.append("says when. A `D` row is where rgy has a weaker version, and the `work`")
    out.append("column says the difference. A `P` row is where rgy already matches, and the")
    out.append("`rgy` column names the code that does it — so the plan does not redo work")
    out.append("that is already done.")
    out.append("")
    out.append("**P** parity &middot; **D** degraded &middot; **G** gap")
    out.append("")
    out.append("| # | Capability | Status | Listary | rgy today | The work | Size |")
    out.append("|---|-----------|--------|---------|-----------|----------|------|")

    by_phase = {}
    for r in rows:
        by_phase.setdefault(r["phase"], []).append(r)

    for phase in sorted(by_phase):
        title = phases.get(str(phase), {}).get("title", f"Phase {phase}")
        why = phases.get(str(phase), {}).get("why", "")
        out.append(f"| | **{phase}. {title}** | | | | | |")
        if why:
            out.append(f"| | {why} | | | | | |")
        for r in by_phase[phase]:
            cells = [
                r["id"],
                r["name"],
                r["status"],
                r.get("listary", ""),
                r.get("rgy", ""),
                r.get("work", ""),
                r.get("size", ""),
            ]
            cells = [_cell(c) for c in cells]
            out.append("| " + " | ".join(cells) + " |")

    gaps = [r["id"] for r in rows if r["status"] == "G"]
    degraded = [r["id"] for r in rows if r["status"] == "D"]
    parity = [r["id"] for r in rows if r["status"] == "P"]
    out.append("")
    out.append(f"<!-- {len(rows)} capabilities: {len(gaps)} gaps (G), {len(degraded)} degraded (D), "
               f"{len(parity)} at parity (P) -->")

    out.append("")
    out.append("## The phases")
    out.append("")
    out.append("| Phase | What it is | Why here |")
    out.append("|-------|------------|---------|")
    for phase in sorted(by_phase):
        meta_p = phases.get(str(phase), {})
        out.append(f"| {phase} | {meta_p.get('title', '')} | {meta_p.get('why', '')} |")

    unknowns = data.get("unknown", [])
    if unknowns:
        out.append("")
        out.append("## What the evidence does not settle")
        out.append("")
        out.append("The rows above rest on a reverse engineering report, not on source. These")
        out.append("are that report's own residual unknowns, and the plan inherits them.")
        out.append("")
        out.append("| # | Unknown | Affects |")
        out.append("|---|---------|---------|")
        for u in unknowns:
            out.append(f"| {u['id']} | {_cell(u['text'])} | {_cell(u.get('affects', ''))} |")

    out.append("")
    out.append("## Source")
    out.append("")
    out.append(f"The report is `{meta.get('report', 'unknown')}` — reverse engineering of a")
    out.append(f"proprietary binary, not source. Its own verification: {meta.get('verification', 'unknown')}.")
    out.append("A row that rests on an inference says so; the unknowns above are the honest")
    out.append("limit of the evidence every row inherits.")
    return "\n".join(out) + "\n"


def check(data, text) -> list:
    """What is wrong with the committed matrix, in the order it matters."""
    problems = []

    committed = RENDERED.read_text(encoding="utf-8") if RENDERED.exists() else ""
    if committed.strip() != text.strip():
        problems.append(
            "docs/listary-parity.md is not what docs/listary-parity.toml renders. Regenerate it:\n"
            "    python scripts/listary-parity.py > docs/listary-parity.md"
        )

    rows = data["capability"]
    phases = data.get("phases", {})
    seen = set()

    for r in rows:
        rid = r.get("id", "<no id>")
        if rid in seen:
            problems.append(f"row {rid} appears twice in the matrix.")
        seen.add(rid)

        status = r.get("status")
        if status not in STATUSES:
            problems.append(
                f"row {rid} ({r.get('name', '')}) has status {status!r}; it must be one of "
                + ", ".join(STATUSES) + "."
            )

        if "phase" not in r:
            problems.append(f"row {rid} ({r.get('name', '')}) has no phase: it is not scheduled.")
        elif str(r["phase"]) not in phases:
            problems.append(
                f"row {rid} ({r.get('name', '')}) is in phase {r['phase']}, which the phases "
                "table does not define."
            )

        if r.get("size") not in SIZES:
            problems.append(
                f"row {rid} ({r.get('name', '')}) has size {r.get('size')!r}; it must be one of "
                + ", ".join(SIZES) + "."
            )

        if not r.get("evidence"):
            problems.append(
                f"row {rid} ({r.get('name', '')}) cites no evidence: a claim about Listary "
                "without the section that established it is a claim, not a finding."
            )

        if status in ("G", "D") and not r.get("work"):
            problems.append(
                f"row {rid} ({r.get('name', '')}) is a {STATUS_MEANING.get(status, status)} "
                "with no work attached. A gap without a plan is a gap nobody has scheduled: "
                "say what closing it means."
            )

        if status == "P" and not r.get("rgy"):
            problems.append(
                f"row {rid} ({r.get('name', '')}) claims parity but names no place in rgy. "
                "A parity claim without a place in the code is a claim, not a fact."
            )

    # A phase with no rows is a phase nobody wrote.
    for phase in sorted(phases, key=lambda p: int(p)):
        if not any(str(r.get("phase")) == str(phase) for r in rows):
            problems.append(f"phase {phase} ({phases[phase].get('title', '')}) has no rows.")

    # Every phase a row names must be a phase that exists, and the phases
    # have to be contiguous from 1 -- a gap in the numbering is a phase
    # that was dropped without its rows going with it.
    used = sorted({int(r["phase"]) for r in rows if "phase" in r})
    if used:
        expected = list(range(1, max(used) + 1))
        missing = [p for p in expected if p not in used]
        if missing:
            problems.append(
                "phase(s) " + ", ".join(str(p) for p in missing) + " have no rows: the phases "
                "have to be contiguous from 1, because the numbering is the order."
            )

    for u in data.get("unknown", []):
        affects = u.get("affects", "")
        for token in [t.strip() for t in affects.replace(",", " ").split()]:
            if token.startswith("S.") or token.startswith("P.") or token.startswith("Q.") \
                    or token.startswith("I.") or token.startswith("C.") or token.startswith("H.") \
                    or token.startswith("X."):
                if token not in seen:
                    problems.append(
                        f"unknown {u.get('id', '<no id>')} names row {token}, which the matrix "
                        "does not have."
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
        print(f"\nlistary-parity check FAILED: {len(problems)} problem(s)", file=sys.stderr)
        return 1
    rows = data["capability"]
    gaps = [r["id"] for r in rows if r["status"] == "G"]
    degraded = [r["id"] for r in rows if r["status"] == "D"]
    parity = [r["id"] for r in rows if r["status"] == "P"]
    print(f"listary-parity check passed: {len(rows)} capabilities, {len(gaps)} gap(s) "
          f"({', '.join(gaps)}), {len(degraded)} degraded, {len(parity)} at parity; every gap "
          "has work and a phase")
    return 0


if __name__ == "__main__":
    sys.exit(main())
