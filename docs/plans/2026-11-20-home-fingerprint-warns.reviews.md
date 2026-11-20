# Reviews: home fingerprint

## Full reviews (reference)

- po: approve-with-changes. XDG git config and plugin files missing; trigger misses hand-run binaries; live check and measurement gaps. All accepted.
- lang:rust: approve-with-changes. After-point misses exits; sink is evidence table; Windows id escapes and prune; chmod/symlink tests. All accepted.
- backend: approve-with-changes. Same after-point; no event path; live check; assertion not rule; budget; known_hosts. All accepted.
- unix: approve-with-changes. FIFO hang; sorted cap; absolute git-common-dir; prune; live check. All accepted.
- tui: approve-with-changes. Where the warning shows; one text form; remedy; unreadable/truncated states. All accepted.
- architect: approve-with-changes. Assertion tier; run_to_outcome; EvidenceKind downgrade cost; deny_unknown_fields. All accepted.
- backend round 2: approve-with-changes, 4 carried into implementation: count each path once per hour across runs and decide only at 6+ events; label the picture with code names; concrete Phase 1 verification wording; resumed-run gap as a non-goal.


## Implementation review, round 1 (fix list)

Fix (accepted):
1. An unreadable path on BOTH sides is not a drift: print one separate line `not watched (unreadable): <path>`, never "changed". (tui#1, backend#4)
2. Output shape: `warning_line` is split. `relais run` prints to stderr a header `relais run: warning: home files changed while <run> was active:` and one indented `  <path> (<change>)` line per file; `status`/`explain` print the same body to stdout under the heading `home files changed while the run was active:` without the `relais run:` prefix. Lines fit 80 columns. The follow-up line says what to do: `check these files; if you did not change them, the run or its tests did: rerun them with HOME set to a temporary directory`. A directory truncated on both sides prints `not fully checked: <dir> (more than 500 files)`. (tui#2-5, unix#2)
3. `extra_paths`: expand a leading `~/` against the resolved home; reject any other relative path when machine.toml loads, naming `[fingerprint] extra_paths` and the entry. Test it. (unix#1)
4. Reader: add `relais::homeprint::drifts_of_run(&Ledger, &RunId) -> (Vec<Drift>, Vec<String>)` beside the writer; `home_drift_lines` and the test helper call it; an unreadable row or ledger error yields a visible line `home drift record <path> unreadable: <e>`. (architect#1, backend#2, rust#7)
5. Write the drift file with `record_immutable_artifact(EvidenceKind::HomeDrift, ..)`. (backend#3)
6. One stderr line when `enabled` but no home can be resolved. (backend#5)
7. Tests: a baseline check command that writes `$home/.gitconfig` and the candidate goes red (one drift row); a Superseded run (one drift row); `status_command` and `home_drift_lines` print the warning with the follow-up; the `~` expansion; the unreadable-both-sides line. (backend#1, impl#2)
8. `home_override` becomes `pub(crate)`. (rust#2)
9. Docs and style: module doc says `snapshot` is an action and `diff` the calculation (rust#1, architect#3); `Change::Truncated` comment accurate (rust#3); non-UTF-8 names filtered through `to_string_lossy` (rust#4); exhaustive `diff` match without `_` (rust#5); `impl Display for Change` replaces `label` (rust#6); `walk` returns a value instead of two `&mut` parameters (impl#3); same-line comments on `kill().ok()` and `wait().ok()` (impl#4); names `result`, `a`, `b` replaced (impl#5).

Deliberate (not changed):
- `(home changed)` mark in the list form of `relais status`: dropped. The three places that carry the warning (run stderr, `status <run>`, `explain`) cover it; plan item 5 is struck by this note. (impl#1)
- The engine resolving home and env itself (architect#2, `systems.construction-in-main`): kept; `runner/` already reads `HOST_SLOT_ENV` the same way and the injectable `HomeWatch` value is a follow-up if a second consumer appears.
- Streaming hash and a size cap on `extra_paths` files (unix#3), and a stored absolute path with display applied at print time (architect#4): carried into a follow-up; neither changes a default-set result.
