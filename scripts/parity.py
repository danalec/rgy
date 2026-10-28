#!/usr/bin/env python3
"""Check the parity document: one file, two tables, one gate.

`docs/parity.md` is written by hand. It answers two questions: what each
operating system gets, and what Listary does that rgy does not. This
script reads it and fails when the document says something nobody can act
on.

    python scripts/parity.py --check

It fails when:

  * a row of the per-OS matrix has a cell that is not one of N, P, D, G
    or -, or the wrong number of columns;
  * a cell is a G (gap) and the known-gaps list does not name it, with the
    operating system it is a gap on -- a gap that is not on the list is a
    gap that appeared quietly;
  * the known-gaps list names a row, or an operating system, that is not a
    G cell any more -- a closed gap has to leave the list, so the list
    cannot become a graveyard;
  * a capability row has a status that is not P, D or G, or the wrong
    number of columns;
  * a G or D capability row has no work attached -- a gap without a plan is
    a gap nobody has scheduled;
  * a P capability row has no place in the code -- a parity claim without
    one is a claim, not a fact;
  * a phase has no rows, a row is in no phase, or the phases are not
    contiguous from 1 -- a phase nobody wrote is a phase nobody scheduled.

The gate does not fail because a gap is open. A gap is the work. It fails
when a gap has nothing attached to it.
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DOCUMENT = ROOT / "docs" / "parity.md"

OS_STATES = {"N", "P", "D", "G", "-"}
CAPABILITY_STATES = {"P", "D", "G"}
SIZES = {"S", "M", "L"}
OSES = ("windows", "linux", "macos")

SEPARATOR = re.compile(r"^:?-{2,}:?$")
SECTION_ROW = re.compile(r"^\*\*(\d+)\.\s")
KNOWN_GAP = re.compile(r"^-\s+\*\*([^*]+)\*\*\s+\(([^)]+)\)\s*(?:--|—|-)?\s*(.*)$")


def split_row(line: str) -> list[str]:
    """The cells of a pipe row. A `\\|` is a pipe in the text, not a column."""
    inner = line.strip()
    if inner.startswith("|"):
        inner = inner[1:]
    if inner.endswith("|"):
        inner = inner[:-1]
    cells, current, index = [], [], 0
    while index < len(inner):
        char = inner[index]
        if char == "\\" and index + 1 < len(inner) and inner[index + 1] == "|":
            current.append("|")
            index += 2
            continue
        if char == "|":
            cells.append("".join(current).strip())
            current = []
            index += 1
            continue
        current.append(char)
        index += 1
    cells.append("".join(current).strip())
    return cells


def is_separator(line: str) -> bool:
    cells = split_row(line)
    return bool(cells) and all(SEPARATOR.match(cell) for cell in cells)


def tables(text: str):
    """Every pipe table in the document, as (header, rows)."""
    lines = text.splitlines()
    index = 0
    while index < len(lines):
        line = lines[index]
        if (line.startswith("|") and index + 1 < len(lines)
                and lines[index + 1].startswith("|") and is_separator(lines[index + 1])):
            header = split_row(line)
            rows, cursor = [], index + 2
            while cursor < len(lines) and lines[cursor].startswith("|"):
                rows.append(split_row(lines[cursor]))
                cursor += 1
            yield header, rows
            index = cursor
        else:
            index += 1


def section_body(text: str, title: str) -> list[str]:
    """The lines under a `## title` heading, up to the next heading."""
    lines = text.splitlines()
    start = None
    for index, line in enumerate(lines):
        if line.strip() == f"## {title}":
            start = index + 1
            break
    if start is None:
        return []
    body = []
    for line in lines[start:]:
        if line.startswith("## "):
            break
        body.append(line)
    return body


def check_matrix(rows: list[list[str]], problems: list[str]) -> dict:
    """The per-OS matrix. Returns the G cells it carries."""
    gaps = {}
    for row in rows:
        if len(row) != 6:
            problems.append(
                f"the matrix row {row[0]!r} has {len(row)} columns, not 6 "
                "(# | Feature | Windows | Linux | macOS | Notes)"
            )
            continue
        identifier = row[0]
        if not identifier or not re.match(r"^\d+\.\d+$", identifier):
            continue  # a section row, not a feature
        for offset, os_name in enumerate(OSES, start=2):
            state = row[offset]
            if state not in OS_STATES:
                problems.append(
                    f"row {identifier} ({row[1]}) has {state!r} in the {os_name} column; "
                    "a cell is N, P, D, G or -"
                )
            elif state == "G":
                gaps[(identifier, os_name)] = True
    return gaps


def check_known_gaps(body: list[str], gaps: dict, problems: list[str]) -> None:
    """The known-gaps list and the G cells have to agree, both ways."""
    listed = set()
    for line in body:
        match = KNOWN_GAP.match(line.strip())
        if not match:
            continue
        identifier, os_name = match.group(1).strip(), match.group(2).strip().lower()
        if not match.group(3).strip():
            problems.append(
                f"the known gap {identifier} ({os_name}) has no reason; a gap "
                "without a reason is a gap nobody can decide about"
            )
        listed.add((identifier, os_name))

    for identifier, os_name in sorted(gaps):
        if (identifier, os_name) not in listed:
            problems.append(
                f"row {identifier} carries a G (gap) cell for {os_name} and is not on "
                "the known-gaps list. A new gap is a failure: close it, document the "
                "limit, or add it to the list with the reason it is still open."
            )
    for identifier, os_name in sorted(listed - set(gaps)):
        problems.append(
            f"the known-gaps list names row {identifier} for {os_name}, which is not a "
            "G cell any more: it was closed, and the line has to go, so the list "
            "cannot become a graveyard."
        )


def check_capabilities(rows: list[list[str]], problems: list[str]) -> None:
    """The capabilities table, and the phases the rows sit in."""
    phase = None
    phases: dict[int, int] = {}
    for row in rows:
        if len(row) != 7:
            problems.append(
                f"the capability row {row[0]!r} has {len(row)} columns, not 7 "
                "(# | Capability | Status | Listary | rgy today | The work | Size)"
            )
            continue
        identifier = row[0]
        if not identifier:
            # A section row carries the phase; a prose row carries the reason.
            section = SECTION_ROW.match(row[1])
            if section:
                phase = int(section.group(1))
                phases.setdefault(phase, 0)
            continue
        if not re.match(r"^[A-Z]\.\d+$", identifier):
            continue  # not a capability row
        status = row[2]
        if status not in CAPABILITY_STATES:
            problems.append(
                f"the capability {identifier} ({row[1]}) has status {status!r}; it is "
                "P, D or G"
            )
            continue
        work, today, size = row[5], row[4], row[6]
        if status in ("G", "D") and not work:
            problems.append(
                f"the capability {identifier} ({row[1]}) is {status} and has no work "
                "attached: a gap without a plan is a gap nobody has scheduled."
            )
        if status == "P" and not today:
            problems.append(
                f"the capability {identifier} ({row[1]}) claims parity and names no "
                "place in the code: a parity claim without one is a claim, not a fact."
            )
        if size and size not in SIZES:
            problems.append(
                f"the capability {identifier} ({row[1]}) has size {size!r}; it is S, M or L"
            )
        if phase is None:
            problems.append(
                f"the capability {identifier} ({row[1]}) is in no phase; a row in no "
                "phase is a row nobody scheduled."
            )
        else:
            phases[phase] = phases.get(phase, 0) + 1

    for number in sorted(phases):
        if phases[number] == 0:
            problems.append(
                f"phase {number} has no rows: a phase with no rows is a phase nobody wrote."
            )
    if phases and sorted(phases) != list(range(1, max(phases) + 1)):
        problems.append(
            f"the phases are {sorted(phases)}, not contiguous from 1: a phase that is "
            "not in the order is a phase nobody can schedule around."
        )


def main() -> int:
    if "--check" not in sys.argv[1:]:
        print("usage: python scripts/parity.py --check", file=sys.stderr)
        return 2
    if not DOCUMENT.exists():
        print(f"parity check FAILED: {DOCUMENT} is missing", file=sys.stderr)
        return 1

    text = DOCUMENT.read_text(encoding="utf-8")
    problems: list[str] = []

    matrix = capabilities = phases = None
    for header, rows in tables(text):
        if len(header) >= 6 and header[1] == "Feature" and header[2] == "Windows":
            matrix = rows
        elif len(header) >= 7 and header[1] == "Capability" and header[2] == "Status":
            capabilities = rows
        elif header and header[0] == "Phase":
            phases = rows

    if matrix is None:
        problems.append("the per-OS matrix is not in the document")
    if capabilities is None:
        problems.append("the capabilities table is not in the document")
    if phases is None:
        problems.append("the phases table is not in the document")

    gaps = {}
    if matrix is not None:
        gaps = check_matrix(matrix, problems)
        check_known_gaps(section_body(text, "The known gaps"), gaps, problems)
    if capabilities is not None:
        check_capabilities(capabilities, problems)

    if problems:
        print(f"parity check FAILED: {len(problems)} problem(s)", file=sys.stderr)
        for problem in problems:
            print(f"  - {problem}", file=sys.stderr)
        return 1

    features = sum(1 for row in (matrix or []) if len(row) == 6 and re.match(r"^\d+\.\d+$", row[0]))
    caps = [row for row in (capabilities or []) if len(row) == 7 and re.match(r"^[A-Z]\.\d+$", row[0])]
    at_parity = sum(1 for row in caps if row[2] == "P")
    listary_degraded = sum(1 for row in caps if row[2] == "D")
    listary_gaps = sum(1 for row in caps if row[2] == "G")
    print(
        f"parity check passed: {features} features, {len(gaps)} gap(s) "
        f"({', '.join(sorted(f'{i} {o}' for i, o in gaps)) or 'none'}); "
        f"{len(caps)} capabilities, {listary_gaps} gap(s), {listary_degraded} degraded, "
        f"{at_parity} at parity; every gap has a reason and every gap has work"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
