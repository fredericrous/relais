# Full reviews: settings place under external

Panel ran from areas (the phase headings were not read as phases). Round 1 (opus):

- backend: approve-with-changes. Verification as input/expected; add lang:typescript; name `selected in:`; fold Phase 2; carry the external check. All applied.
- lang:rust: approve-with-changes. Add a local `trust show --json` binary test (applied); one wire spelling (applied).
- tui: approve-with-changes. Say what `machine_settings` means under external; add `selected in:` line (both applied).
- unix: approve-with-changes. Document in `docs/SPEC.md` (applied).
- architect: approve-with-changes. One wire spelling; correct the architect-left-out reason; module doc (applied).
- po: approve-with-changes. Verification names the external checks; public build non-goal; older plugin note (applied).

Round 2: backend approve, no findings. Codex: not run.

## Implementation review

- implementation-review (opus), round 1: approve-with-changes. Verification had no observed results (recorded in the homelab ticket Resolution), the binary test lacked the text check (added), an empty `impl RecordedOn {}` (removed). All applied.
- implementation-review (opus), delta on tree 2cbb9838: approve-with-changes on the Resolution gap only, which the homelab Resolution now answers; everything else resolved, no new findings. Candidates `names.reveal-intent` on `w` and `q` in consent.test.ts ruled fine.
- Rebase onto main 74d0b48c9: CHANGELOG.md conflict resolved under Unreleased; the code tree differs from the reviewed one only by main's v0.89.0 version bump (Cargo.toml, plugin.json), so no delta is due.

