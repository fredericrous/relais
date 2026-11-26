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
- Final-tree review (tree 92af73fd): approve-with-changes, one low finding: Phase 1 names `RecordedOn::of(...)` but the code has the free function `recorded_on` in `cli/trust.rs` (the trust module must not import shared_state: module-cycles gate). `deliberate: the plan body is bound by its panel sha; the deviation is stated here and in the homelab Resolution`. Candidates ruled fine.

## Code review profiles (tree 92af73fd, opus)

- backend: approve. One low note on the unchecked `parsed<Shown>` cast; `deliberate: the cast predates this change and the ternary falls back safely`.
- unix: approve, no findings.
- lang:typescript: approve. Same low note on the cast; `deliberate: as above`.
- lang:rust: approve-with-changes, one low: the `Shown::recorded_on` doc comment misdescribed `machine_settings`. Fixed.
- tui: approve-with-changes, one low: the label `selected in:` is unclear. `deliberate: the plan's tui reviewer proposed this wording, the SPEC and the Resolution say what the path is, and a rename would reopen the panel round`.

## After the rebase onto an-t32 (tree f27fe281, opus)

- implementation-review: approve-with-changes; Verification rows lacked observed results. Fixed: `Observed:` lines added under each row (docs only, the code tree is unchanged).
- backend: approve. Low: no test reaches the `shared.selection()` line under `external`; `deliberate: carried to the first private-build run (an-t13), as the plan states; a trust_command_with(state) split is a larger refactor than this ticket`.
- lang:rust, lang:typescript, unix: approve, no findings (typescript repeats the cast note, deliberate as before).
- tui: approve. Low: the new `selected in:` path is printed raw like the old `machine.toml:` line; `deliberate: same behaviour as the line it sits beside, which also prints it raw; an escape pass over both is a separate change`.

