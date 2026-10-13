#!/usr/bin/env python3
"""Render the parity matrix (docs/formats/parity.toml) as a Markdown table.

The README carries this matrix, auto-generated so it cannot rot (plan 6.7).
Regenerate with:

    python scripts/parity-matrix.py > /tmp/parity.md

and paste the output under the README's "OS parity" heading. CI can diff the
committed README against a fresh run to fail when they diverge. Emits to
stdout; writes nothing.
"""
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PARITY = ROOT / "docs" / "formats" / "parity.toml"

LEGEND = (
    "**N** native &middot; **P** ported (shared code, platform hook) &middot; "
    "**D** degraded (documented limit) &middot; **G** gap (work required) &middot; "
    "**-** N/A"
)


def main() -> int:
    data = tomllib.loads(PARITY.read_text(encoding="utf-8"))
    rows = data["feature"]

    print("## OS parity")
    print()
    print(LEGEND)
    print()
    print("| # | Feature | Windows | Linux | macOS | Notes |")
    print("|---|---------|---------|-------|-------|-------|")

    section = None
    for r in rows:
        # A row's section is implied by its id prefix (1.x discovery, ...).
        sid = r["id"].split(".")[0]
        if sid != section:
            section = sid
            title = data.get("section", {}).get(sid, f"§6.{sid}")
            print(f"| | **{title}** | | | | |")
        cells = [r["id"], r["name"], r["windows"], r["linux"], r["macos"], r.get("notes", "")]
        cells = [c.replace("|", "\\|").replace("\n", " ") for c in cells]
        print("| " + " | ".join(cells) + " |")

    gaps = [r["id"] for r in rows if "G" in (r["windows"], r["linux"], r["macos"])]
    print()
    print(f"<!-- {len(rows)} features; {len(gaps)} carry at least one G (gap) cell: "
          + ", ".join(gaps) + " -->")
    return 0


if __name__ == "__main__":
    sys.exit(main())
