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

### UI delta (2026-10-07, after S0 picked `$.ui.ask`; the person chose to run it)

- **backend: approve-with-changes** (32k tokens, 30 s). Findings, all applied:
  - [blocking] The first-label yes rule made `Not now` a yes.
  - [high] Typed text was both a decline and the answer.
  - [medium] "Timeout" contradiction.
  - [medium] Rejection paths.
  - [medium] Escape repository-controlled text.
  - [medium] UI verification rows.
  - [low] Mark S0 done.
- **game-ux: approve-with-changes** (28k tokens, 27 s). Findings, all applied:
  - Same yes-rule finding.
  - [high] ~9-minute gap between the questions → both questions first.
  - [medium] Confirmation after Allow.
  - [medium] Label/text mismatch.
  - [medium] Chat about this.
  - [low] How to resume.
  - [low] "1 of 2".
- **ui-design: approve-with-changes** (29k tokens, 39 s). Findings, all applied:
  - Same yes-rule finding.
  - [high] Q2 must list models and integrations.
  - [medium] Q1 phrased as a question, with "Skipped".
  - [medium] Q2 puts the risk first.
  - [medium] Discuss vs decline.
  - [medium] No re-ask in the session.
  - [low] Toast style.
  - [low] 80 columns.
- **ux-research: approve-with-changes** (37k tokens, 48 s). Findings, all applied:
  - Same yes-rule finding, plus a shell-style split that refuses metacharacters.
  - [medium] Re-ask habituation → `+`/`~` marks (Alice in Warningland).
  - [medium] Safe option first (NN/g).
  - [low] `Allow N commands`.
  - Kept as is:
    - [low] two questions are not backed by research → measure command recall;
    - [low] jargon → "Skipped … (not a plain command)".
- **react: approve-with-changes** (43k tokens, 39 s). Findings, all applied:
  - [high] `Fx.ui.ask` wiring.
  - [high] Fail closed (`try/catch` + `.catch`).
  - [medium] Per-repository `store.consent`.
  - [medium] Answer rules.
  - [low] Timeout.
  - [low] Status overwritten.
  - [medium] Scripted `askAnswer`.
- **backend, re-bind: approve-with-changes** (33k tokens, 31 s). Applied:
  - `no_ui` cannot be detected → folded into `dismissed`.
  - The commit inside the handler, against the mods limits → `fx.process.run`, 10 min.
  - Re-ask expected value.
  - Missing rows.
- **backend, final bind: approve** (25k tokens, 18 s). Two lows: a stale `index.lock` after a timeout (carried to implementation), and the verdicts line (fixed).
