# EGM drift status

The gate reads this file, not the free-form drift reports. After an
`egm-drift-report` pass, update `date`, `report` and `unresolved` here; the
dated reports in this directory are the audit trail behind it. An
`unresolved` count above zero fails the gate, so a finding cannot be merged
without a decision about the code. The status must name the newest dated
report, so clearing `unresolved` without a new report leaves the gate red.

date: 2026-11-06
report: .egm/2026-11-06_DRIFT_REPORT.md
unresolved: 0
