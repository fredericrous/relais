# Reviews: relais zero-setup onboarding

## Full reviews (reference)

### Round 1

- **plan-review-backend: rework** (52k tokens, 60 s). Findings, all applied:
  - [blocking] An uncommitted `relais.toml` triggers DirtyBase.
  - [blocking] A fresh machine never reaches `missing_trust_grant` (pre-run `CliError`, `Done` needs a run).
  - [high] The model would word the consent question; the plugin must rewrite it and grant the key `show` returned.
  - [high] The Bash argv guard is bypassable.
  - [medium] No lock on `machine.toml`.
  - [conditional] A native dialog means `adds=ui`.
  - [medium] Verification checks were not observable.
  - [low] No ledger event for a grant.
- **plan-review-language (rust): approve-with-changes** (46k tokens, 53 s). Findings, all applied:
  - [high] `no_policy` and a missing `machine.toml` fail before preflight.
  - [high] "Model-proof" overclaimed, since Write/Edit/`sh -c` reach `machine.toml`.
  - [medium] `flock`, parent fsync, 0600.
  - [medium] Detection must not run `make -n`.
  - [low] `toml_edit 0.22` is already transitive.
  - [low] Fixtures go in tempdirs.
- **plan-review-tui: approve-with-changes** (41k tokens, 39 s). Findings, all applied:
  - [high] `endUnfinished` queues nothing, so the model waits forever.
  - [high] A missing `machine.toml` gives a read error.
  - [high] No `declined` outcome.
  - [medium] Nobody owns the "nothing detected" question.
  - [medium] The init/`load_machine` hints still say to paste the block.
  - [medium] `stale_grant_key` gives no next step.
  - [low] Text output layout, 80 columns, `NO_COLOR`.
  - [low] `runs.ts` path.
- **plan-review-unix: approve-with-changes** (39k tokens, 37 s). Findings, all applied:
  - [high] Read-modify-rename race.
  - [medium] Atomic write loses the file mode.
  - [medium] Exit codes: 18 stale key, 4 nothing detected, 2 on overwrite.
  - [medium] The Bash guard is a nudge, not a control.
  - [low] Config precedence stated in `trust show`.
  - [low] stdout/stderr split; `grant` never reads stdin.

### Round 2

- **plan-review-backend: approve-with-changes** (36k tokens, 35 s). All round-1 findings resolved. New findings, all applied:
  - [high] Onboard commit uses `git commit --only`, refuses on detached HEAD/rebase/merge/hook reject, and reports the SHA.
  - [medium] `endUnfinished` queues only without `done` (`doneSeen`).
  - [low] `run: null` text.
  - [low] An expected value for the bypass line.
- **plan-review-backend, final bind: approve** (24k tokens, 15 s). Two lows carried to implementation:
  - the `env` bypass line should state one expected value (1 grant plus a decision-log entry);
  - on `onboard_commit_failed`, keep `relais.toml` and tell the person to commit it, or remove it — choose one.
