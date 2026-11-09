# Full reviews: relais fewer and smaller status results

- backend (opus): round 1 approve-with-changes, 7 findings (1 high: `Why` duplicated `detail`/`stderrTail`); round 2 approve-with-changes, 4 low; delta on final body approve. Carried: failures take at most 30 of the 40 lines so 10 tail lines remain.
- lang:typescript (opus): round 1 approve-with-changes, 5 findings (2 high: failures must be kept in `appendOutput`; 40% bound unreachable); round 2 approve-with-changes (regex/test contradiction, 90% bound, Left open). Slice: Phases 2-5.
- tui (opus): round 1 approve-with-changes, 5 findings (2 high: stderr is untrusted text sent as a prompt; keep `Why` out of `facts`); round 2 all resolved, 1 low.
- unix (opus): round 1 approve-with-changes, 4 findings (1 high: filtering cannot see lines past the 40-line tail); round 2 one high (CRLF lines would be emptied) applied, plus `trailing` context counter and overflow rule.
- codex: not run (plan mode, read-only; the skill's `codex exec` bypasses the sandbox).
