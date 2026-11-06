# EGM drift status

The gate reads this file, not the free-form drift reports. After an
`egm-drift-report` pass, update `date`, `report` and `unresolved` here; the
dated reports in this directory are the audit trail behind it. An
`unresolved` count above zero fails the gate, so a finding cannot be merged
without a decision about the code. The status must name the newest dated
report, so clearing `unresolved` without a new report leaves the gate red.

The report named below is the **second, complete pass** of 2026-11-06: it
supersedes the first pass of the same date, which was a targeted
reconciliation and recorded its own coverage gap. All 51 tracked
contracts have now had a fresh-context semantic review, plus an
automated citation, count and structure audit. Phase 2 (`--generate` and
`--generated-check`) is implemented and in CI. No finding is unresolved.

date: 2026-11-06
report: .egm/2026-11-06_DRIFT_REPORT.md
unresolved: 0
