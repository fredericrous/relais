# Full reviews: route settings writes through SharedState

## Round 1

- backend: approve-with-changes. Accepted: the declared Operator kind is only honoured by LocalState, an external primary derives it from the credential (high); the installer does not exit 3 (medium); verification entries were not observable checks (medium); the picture drew the primary as built here (low). Rejected: drop `tui` from the code review line, since greenbelay proposes it.
- lang:rust: approve-with-changes. Accepted: no test pins exit 3 for the writers, name them in Phase 2 (high); installer behaviour (medium); an external client runs the settings checks against a scratch primary only (medium).
- tui: approve-with-changes. Accepted: installer behaviour stated (high); verification covers the stderr sentence, the installer and the off forms (medium). Carried: `--json` under external prints nothing on stdout (applies to every routed operation).
- unix: approve-with-changes. Accepted: installer exception (medium); state the exit code of a failed write once (low).

## Round 2 (delta)

- backend: approve-with-changes; every finding resolved, one low gap carried into implementation: the conformance check cannot hold an external client to the credential rule, so a client runs `only_an_operator_writes_settings` while connected with a node credential.
- lang:rust: approve-with-changes; findings 1-3 resolved; one medium carried into implementation: the comparison with the `main` binary must ignore the `granted_at` and `at` timestamp lines.
- tui: approve; the installer prints the absent sentence once per write (two lines, new output, accurate); a shorter second line is a later option.
- unix: approve; no new findings.
