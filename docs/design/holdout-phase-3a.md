# Design: holdouts run and settle (ADR-0045, phase 3a)

Phase 2 (`holdout.rs`) resolves each holdout criterion at preflight and keeps only
`HoldoutRecord {name, sha, digest}` in the manifest. Phase 3a fetches, runs and
settles holdouts at verification. Out of scope here (phase 3b and 3c): the
overlay collision check, protected verification inputs, repair and escalation
feedback rules beyond the gap text, successor inheritance, and the repository-name
detail.

## Keep the resolved definitions

`runner/types.rs` `Preflight` gains `holdouts: Vec<crate::holdout::ResolvedHoldout>`.
It is never serialized into the manifest or any prompt. preflight.rs keeps
the `resolve_all` result instead of mapping it straight to records.

## Where things live

`holdout_root(artifacts_dir) = state_sibling(artifacts_dir, "holdouts")`, beside
`verify_root` (`runner/config.rs`). Per run: `<holdout_root>/<run>/`:
- `src/<sha>/`: the fetched store tree (a temp dir renamed into place once complete);
- `copy-<attempt suffix>/`: the second verification copy;
- `logs/`: the holdout command logs.

The kept report and log copies for the person go into the run's artifacts dir
under `holdouts/<name>/` (`report.xml`, `log.txt`). That dir is never named in a prompt.

## Fetch (`holdout::fetch`)

`fetch(resolved: &ResolvedHoldout, dest_root: &Path) -> Result<PathBuf, Unfetchable>`:
- `git init -q <tmp>`;
- `git -C <tmp> fetch --depth 1 <store> <sha>`. If that is refused (a server
  that does not allow fetching an unadvertised sha), fall back to
  `fetch --depth 1 <store> <ref>` and require `FETCH_HEAD == sha`;
- `git -C <tmp> checkout -q <sha>`;
- rename `<tmp>` to `<dest_root>/src/<sha>/`.

Every git call goes through `procs::run_with_timeout` (60 s each) with the
same env as `ls_remote`: `GIT_TERMINAL_PROMPT=0`,
`GIT_SSH_COMMAND="ssh -o BatchMode=yes"`, stdin null. Factor that env into
one helper shared with `ls_remote`.

`Unfetchable { store, sha, detail }` (git's last stderr line). Any failure,
or a sha mismatch, is `Unfetchable`.

## Run (in `verify_candidate`, after the profile checks and acceptance inputs)

Only when `preflight.holdouts` is non-empty. For each resolved holdout, in
contract order:
1. Fetch, once per run (reuse `src/<sha>/` when present).
2. Make the second copy, `verify::verification_worktree(candidate_sha, <copy dir>)`,
   AFTER the profile commands have finished. One copy per holdout run, released after.
3. Run the profile's setup steps in the copy (`setup_tree`), with logs in the
   holdout `logs/` dir, never the run's `logs_dir`. A setup failure makes the
   holdout `DidNotBuild`.
4. Copy `<src>/<decl.path>` (a file or a directory) to `<copy>/<decl.overlay>`,
   creating parent dirs.
5. Substitute `{config}` in `decl.command.argv` with the absolute path of
   `<src>/<decl.config>` (when `config` is set; a `{config}` with no config is
   `DidNotBuild`). Delete `<copy>/<decl.command.junit>` if it exists.
6. Run the command in the copy through the same bounded runner the profile uses
   (a one-command profile is fine), logging to the holdout `logs/` dir. Do NOT
   record it as `EvidenceKind::CheckLog` or as a junit-report row of the profile.
7. Read the report at `<copy>/<junit>` with `verify::read_junit` / `parse_junit`.

Outcome (`HoldoutOutcome`):
- `Passed { tests }`: exit 0, report `Reported`, at least one passed test, none failed or skipped;
- `Failed { failing: Vec<test id> }`: report `Reported` with a failed or skipped test,
  or a non-zero exit with a parseable report. The ids are failed plus skipped,
  or empty when the exit was non-zero and every test passed;
- `DidNotBuild`: report missing or unparseable, or a setup failure;
- the fetch error is not an outcome. `verify_candidate` returns it so the
  run ends `blocked (holdout_unfetchable)` (a new `BlockCode`, exit 3 like
  every block) with the candidate kept. The detail names store, sha and git's
  last stderr line. It is never a failed criterion.

## Settle

`verify::acceptance_gaps` takes `holdout_results: &BTreeMap<String, HoldoutOutcome>`
(the same shape as `gate_coverage`). For an `Evidence::Holdout { name }` criterion:
- `Passed`: met, `SettledVia::DeclaredEvidence`, and an evidence row
  (`attach_evidence`, tool `relais-holdout`, external id `<name>@<sha>`);
- `Failed`: gap `MissingEvidence::HoldoutFailed { name, failing }`, message
  ``acceptance criterion `<id>`: holdout `<name>` failed: <ids, comma-separated>``,
  or "exited non-zero" when the list is empty;
- `DidNotBuild`: gap `MissingEvidence::HoldoutDidNotBuild { name }`, message
  ``acceptance criterion `<id>`: holdout `<name>` did not build or wrote no report``;
- absent from the map (should not happen): the old `HoldoutNotRun`, kept as a fallback.

Gap messages carry the criterion id, the holdout name and test ids ONLY: never
the store, path, overlay, config, log lines or report text. A unit test asserts
this on each message.

## Receipt

`verify::Verified` and the receipt gain `holdouts: Vec<HoldoutRun>`, where
`HoldoutRun = { name, sha, digest, outcome: "passed"|"failed"|"did_not_build",
exit: Option<i32>, tests: Vec<{id, outcome}>, report: Option<path in artifacts>,
log: path in artifacts }`, written with `#[serde(default, skip_serializing_if = "Vec::is_empty")]`
so that existing receipts read back unchanged.

## Cleanup

After the receipt is written (and on every terminal state), remove
`<holdout_root>/<run>/` entirely. At run start, remove the `<holdout_root>/<run>/`
of every run the ledger has as ended. That is a small sweep, which
`relais resume --retire` also calls.

## Tests

A local bare repo as the store, built in the test. The holdout file is a shell
script; the command is `["sh", "<overlay>"]`, and the script writes a JUnit file
at the declared path from what it finds in the candidate tree. Cover:
- pass: criterion met, receipt `holdouts[0].outcome == "passed"`, run accepted;
- fail: one failing test id in the gap, and the gap text contains no line of the script;
- skipped with exit 0: not met;
- no report: `DidNotBuild`;
- a store deleted after preflight: `blocked (holdout_unfetchable)`, candidate ref kept;
- `<holdout_root>/<run>/` is absent after the run, and no holdout log is a `CheckLog` evidence row;
- `{config}` substitution reaches the command.
