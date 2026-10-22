---
status: active
branch: feat/status-summary
repos: [relais, dotfiles]
adrs: [relais:ADR-0001, relais:ADR-0006, decisions:ADR-0042]
---
# Plan: cut model cost where the money is

## Review panel

👉 **Decide:** relais keeps choosing the session model and Opus only advises (proposed), or a fixed Sonnet session model. Reviewers pinned to Opus.
📍 relais, dotfiles · plan ready · next: test that the advisor works through the proxy. Panel: backend (opus-5-5), codex (gpt-6-astra). ⚠ The tool picked a backend-only panel because the plan file name was reused.
**Changed by review:** relais's router keeps the session model (ADR-0001); Haiku earns feature tasks through replays, not live trials (live trials refuse a Haiku change); both promotion gates required, person judges rejected replays.
📄 Full reviews: [2026-10-22-cut-model-cost.reviews.md](2026-10-22-cut-model-cost.reviews.md)
**Verdicts:** backend rework, then approve-with-changes ×4; codex rework, applied. 9 medium findings carried into implementation.

## Context

October, `relais report` (API-equivalent estimates):
- The orchestrating sessions (main Claude Code on Opus) cost ~$7,200; every relais worker and reviewer together ~$409. relais can move at most ~5% of the bill, so the main session's model is the big lever.
- Haiku ran 43 of 422 relais runs ($15). `relais.toml` uses Haiku only for research; every change goes to Sonnet, then Opus.
- The Haiku-on-feature trial is 7 of 7 qualifying (5 accepted, all without escalation per `trials.accepted_without_escalation`; 2 rejected then judged pass; 5 errored on infra), at ~$0.55 against ~$1.24 on Sonnet. The ADR-0006 gate (`crates/relais/src/learn/typed_gate.rs`) needs 15 with at least 14 accepted. Replays start only from runs relais accepted itself (`SPEC.md:902`); 15 such feature tasks have no trial yet.
- `mcp__relais__status` returned ~53k tokens. The tool already drops `events` and `output` (`claude-plugin/hooks/timeline.ts:580` `statusReply`), so the bulk is what it does keep, for every run held in the store.
- The relais reviewer (escalation tier, Opus) caught 4 in 51 runs: 0.078 per review, above ADR-0042's 0.05 retire bar. **It stays; no change.**
- Rescues (139 salvages in October), by cause: review findings 50, process crash 26, scope exceeded 23, reviewer unavailable 22. **Not in this plan.** They are the next plan, because they need a diagnosis first.

The person asked to also use Claude Code's `advisorModel`: the main session on Sonnet consults Opus when it judges it needs to (docs: code.claude.com/docs/en/advisor; v2.1.260+, installed 2.1.308).

## A. An Opus advisor behind the routed session (dotfiles)

1. **Probe the proxy first.** `ANTHROPIC_BASE_URL` points to the local CLIProxyAPI (`127.0.0.1:8317`). The advisor is a server-side tool. If the API refuses the pairing, Claude Code silently resends without the advisor, so a probe must show the advisor really ran.
   - Run `claude -p --model sonnet --advisor opus` and `claude -p --model haiku --advisor opus` (confirm the `--advisor` flag with `claude --help` first) on a prompt that asks it to consult the advisor before answering.
   - Read that session's transcript for an advisor tool-use block and Opus usage.
   - No block: stop A, report it, and leave the settings alone.
2. **Pin the reviewers before the main model changes.** 13 agents in `~/.local/share/chezmoi/dot_claude/agents/` have no `model:` and inherit the main model today (Opus): `plan-review-*`, `implementation-review`, `interface-review`. Each gets `model: opus`, so review quality does not silently drop to Sonnet. The pinned agents keep their models: Explore sonnet, relais-implementation sonnet, relais-review fable, relais-research haiku.
3. **Turn the advisor on; relais keeps owning the session model.** relais's session router (ADR-0001, mode `on` since 2026-10-08, 10% holdout) already picks the session's model per task (Haiku, Sonnet or Opus). Two writers must not own it, so `dot_claude/settings.json` gets only `"advisorModel": "opus"`: whenever the router puts the session on Sonnet or Haiku, Opus backs it. Lowering the router's tiers (its capability table) is a later, measured step.
4. **Land it.** PR on fredericrous/dotfiles, merge on green, `chezmoi apply`, then restart Claude Code.
5. **Measure after 7 days.** Compare against the 7 days before, from `relais report` `session_routing`:
   - cost per completed task on the routed arm;
   - the corrected count;
   - the salvage rate.
   - **Revert** (one line, `"model": "opus"`) if the corrected or salvaged share exceeds its October baseline by more than half, relative. Write the baseline numerator and denominator into the plan before step 3. Otherwise keep it.

## B. `status` returns a summary (relais plugin)

1. **Measure.** Write a test with a fixture of 20 runs, shaped like this session's (`relais native status --session …`). Measure the byte length of what `statusOf` (`claude-plugin/hooks/runs.ts:629`) returns. Record the figure.
2. **Without `run`, return one short object per run:** `run`, `phase`, `attempt`, `cost`, `outcome`, `waiting`, `stalled`, `foreign`.
   - Waiting or live runs come first and count toward the 10; the rest are the most recent. `more: N` counts those left out.
   - A `more: N` count tells the caller how many were left out.
   - `phases`, `decisions` and `stderr` stay in the reply only when `run` is given, exactly as today.
   - Update the tool description to match.
3. **Tests in `claude-plugin/tests/status.test.ts`:**
   - the summary shape;
   - the cap and `more`;
   - the per-run detail unchanged;
   - a byte budget of at most 6 KB for the 20-run fixture.
4. **Release.** A relais patch release (the plugin version rides along), then `brew upgrade`. The person reinstalls the plugin.

## C. Haiku earns feature tasks through replays (ADR-0006 as it stands)

Live trials cannot run this candidate: `runner/live_trial.rs:112` admits with `CandidateAdmission::Promote`, which refuses a research-tier change (`route/candidate.rs:656`). Replays (`CandidateAdmission::Replay`) are the door ADR-0006 names, and the pool is not dry: 15 feature tasks have a relais-accepted run with a receipt and no trial yet (14 in other repositories, 1 in relais), plus the 5 errored trials, which can be replayed again.

1. **Write the candidate** at `~/.config/relais/candidates/change-feature-haiku.toml`: the existing arm's recipe (`change-feature` revision 2, `tier = "research"`, `task_type = ["feature"]`), reused so the 7 Success trials already recorded keep counting. `relais recipe diff` must admit it. If a recipe-level `models` override for escalation to `sonnet` is admitted, it becomes a new arm and the count restarts, so keep the existing arm and note escalation stays Opus.
2. **Replay**, one task at a time: `relais dataset replay` on each of the 15 sources, then the 5 errored ones. A rejected replay is judged by the person with `relais dataset judge-trial` under the D9 rule (check each finding against the source's code at its commit; `fail` only for a defect the source lacks). I report the findings and do not judge: the typed gate counts only a person's pass (`typed_gate.rs:27`, `:92`). Hard budget $20: after each replay I read the running total from `relais report` (the replay CLI has no cap flag) and stop before a replay would take it past $20. The $0.55 figure covers Haiku successes only; each Opus escalation's cost is recorded as it lands. A source refused for a changed verification profile (`SPEC.md:902`) is skipped and counted.
3. **Two gates, both required:** the typed gate (first 15 qualifying tasks, at least 14 Success: accepted without escalation, or judged pass; `typed_gate.rs:97-106`), and the paired comparison, which needs at least 20 paired tasks (`learn/comparison.rs:45`). Check both with `relais recipe evaluate <candidate>`.
4. **Promote** only when both pass: `relais recipe promote <candidate> --write` through a relais PR, merged on green. Feature tasks then start on Haiku; the 10% Sonnet draw and the typed rollback (`typed_rollback.rs`) keep watching. If the repository's grant does not cover the new policy, `relais plan` says `missing_trust_grant` and the person grants it; I never do.
5. **If a gate fails**, report the numbers and leave routing as it is.

## Decisions for the person

1. **Who owns the main model.** relais's router keeps choosing per task, and the advisor backs every non-Opus choice (proposed: ADR-0001 stays true and the holdout still measures). The alternative is a static `"model": "sonnet"`, which needs ADR-0001 amended and the router's session routing turned off.
2. **Reviewer agents pinned to Opus (proposed).** Not Sonnet: the panel's catch rate was measured on Opus. Subagents inherit the Opus advisor, so state whether pinned reviewers run without it; measure advisor consults per session first.
3. **Replay budget:** $20 hard cap for up to 20 replays (~$0.55 each when Haiku succeeds; a failed one escalates to Opus and costs more).

## Verification

- **A:**
  - The probe transcript shows an advisor block. Record the line it was found on.
  - `grep -L '^model:' dot_claude/agents/*.md` → empty.
  - After `chezmoi apply`, a new session runs the model `relais native router-state` gives it, and `/advisor` shows `opus`.
  - The 7-day comparison is written into memory with its numbers.
- **B:**
  - `make plugin` exits 0. Input: 20-run fixture, 12 waiting or live. Expected: 10 entries and `more: 10`, reply at most 6144 bytes. Actual: recorded.
  - The byte figure before and after.
  - The live `mcp__relais__status` call in a session with more than 10 runs returns at most 6 KB.
  - `cargo test` unaffected; the pre-commit gate is green.
- **C:**
  - `relais recipe diff` admits the candidate. Before any replay, `relais recipe evaluate <candidate>` counts the 7 existing Success trials under the candidate's arm (expected 7 qualifying, 7 Success; actual recorded). Arm ids are content hashes (`learn/comparison.rs:598-604`), so a changed byte starts the count at 0.
  - Each replay leaves a settled `trials` row on the existing arm; the count of replays, Success, judged and errored is recorded with total spend (at most $20).
  - `relais recipe evaluate` prints both gates; promotion happens only when both pass.
- **All:** the merged PRs and the release are verified as `tag-release` steps 4–6.

<!-- panel: repos=dotfiles,relais reviewers=backend body-sha=385fca95a47f -->
