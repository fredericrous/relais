---
status: done
branch: feat/trust-once-per-repo
repos: [relais]
adrs:
  - relais ADR-0007 (a repository is trusted once; a change to its commands warns)
---

# relais: trust a repository once, warn when its commands change

## Review panel

👉 **Decide:** none — approve if one grant per repository, with a once-per-change warning on command changes, is how relais trusts.
📍 relais · plan reviewed · next: one PR, release, then the task-types pilot resumes with no candidate grants. Panel: backend (opus-5-5).
**Changed by review:** a rollback section (v0.18.0 cannot read `[trust_repos]`); the warning hashes commands and integrations, not models; only `run` consumes the warning.
📄 Full reviews: [2026-10-13-trust-once-per-repo.reviews.md](2026-10-13-trust-once-per-repo.reviews.md)
**Verdicts:** round 1 approve-with-changes (6 applied); round 2 and the final binding pass approve-with-changes, no high; 4 carried into implementation (ledger copy, per-run trust status, no bool flag, a downgrade rehearsal).

## Context

The person, 2026-10-13, after a task-types replay stopped on `missing_trust_grant`: "we have too many trust stuff. one is already a lot. what do the industry do? would warning be enough". They chose **once per repository, warn on change**.

Today a grant is bound to the whole policy's content. `grant_key(authority_hash, RepoIdentity)` (`crates/relais/src/policy/mod.rs:605`) hashes `authority_value()`, which is the whole `RepoPolicy` minus `schema_version` (`mod.rs:504-536`), recipes included (`recipes_are_hashed_as_executable_authority`). So:

- every edit to `relais.toml` needs a fresh grant (direnv's model and its most common complaint);
- every learner or replay candidate is a new policy with its own hash. `dataset replay` runs `execute` on `candidate.policy()` (`main.rs:5877`), and live trials recheck the candidate's key (`runner/live_trial.rs:100-120`). The task-types screening would need about six grants, and every later candidate one more, although a candidate only changes routing: its verification is required to be byte-identical to the incumbent's (`main.rs:5664`).

Other tools trust a location once: VS Code Workspace Trust (the folder), `git safe.directory` (the path), mise (the config path; content hashing only in paranoid mode). The repository's contents are written by its committers, so a merged malicious `relais.toml` is already outside what a hash grant stops: the person reviews their own changes again and again without that protecting anything. The SPEC's own limit stays: a session in bypass-permissions mode is outside the threat model (`docs/SPEC.md:1033`).

**Intended outcome:** one grant per repository, ever. relais never blocks a trusted repository on its own policy changes or on a candidate. When the commands it would run change (verification setup and commands, execution, integrations), it says so once, plainly, and runs.

## The model

- **What is trusted.** A repository, by `policy::repo_key(&RepoIdentity)` (`mod.rs:617`; origin URL, else the git common dir, so worktrees share it; a different repository never shares a grant, as today's P2 requires).
- **The grant.** A new machine.toml table, `[trust_repos."<repo_key>"]`, with the same fields as `TrustGrant` (`granted_at`, `reviewed_by`, `repo` label, `note`; `mod.rs:1038`, validated the same way). Written only by `relais trust grant`, through the existing locked, atomic `trust::grant` path (`src/trust.rs:311`).
- **The check.** `effective_authority` (`mod.rs:1632`, the only grant check `run` and replay pass through) holds when the repository has a repo grant, **or** a legacy `[trust."<key>"]` grant matches the policy exactly (expand step: every existing grant keeps working). Otherwise `missing_trust_grant` as today, so an untrusted, newly cloned repository is still blocked until the person grants it once.
- **Candidates.** Live-trial admission (`live_trial.rs:100-120`) checks the repository's grant, not the candidate's key; a candidate whose commands differ from the incumbent's is still refused by `validate_candidate` (unchanged). Replay preflight passes on the repository's grant.
- **The warning.** relais keeps, per repository, the command declaration it last ran under: the `steps` and `integrations` of `trust::declaration()` (`trust.rs:68-124`; `models` excluded, so a routing candidate never warns; execution limits are not covered) hashed, stored in the ledger next to `trust_grants`. At preflight, when the current declaration differs from the last one seen, relais prints once on stderr `relais: commands changed in relais.toml since <date>: <one line per added / removed / changed step>`, records a `commands_changed` event on the run (`relais status`, `relais report`), and updates the stored declaration (an upsert keyed by `repo_key`; two concurrent runs may both warn, neither fails), so the warning appears once per change, not on every run. Only `run` preflight updates it; `plan`, `doctor` and `trust show` report a pending change without consuming it, and a replay neither says nor records one (changed in implementation: a replay that reported it printed and recorded it again on every replay run). `trust grant` records the starting declaration. A recipe, tier, model-routing or context change is not a command change and prints nothing. The plugin relays the warning in the run's outcome message.
- **`relais trust grant`** with no `--key` grants the current repository (`--reviewed-by` still required, `--note` optional, `--json`). `--key` keeps writing a legacy hash grant and prints that it is deprecated in favour of the repository grant (CLI interface is public, `cli.compat.interface-is-public`; removed only in a later major). `trust show` reports `repository: granted <date> by <who>` or `not granted`, then the declaration.
- **Plugin.** `mcp__relais__trust` and `mcp__relais__onboard` (`claude-plugin/hooks/consent.ts:237-262`) ask once per repository ("Allow relais to run this repository's commands?", showing them) and call `relais trust grant --source plugin` without a key; a repository already granted returns `ready` without asking. `runs.ts:59`'s `missing_trust_grant` text stays. The machine.toml and `relais trust grant` guards stay (`guards.ts:~85`): only the person grants, now once.
- **What does not change.** Nothing grants on its own; no run, recipe, learner or worker writes machine.toml; the bypass-mode limit; `deny_unknown_fields`; escaping of displayed commands.

## Changes

- `crates/relais/src/policy/mod.rs`: `MachineSettings.trust_repos`, `RepoTrust` (or reuse `TrustGrant`), `effective_authority` accepts the repo grant, `TrustStatus { Repository, LegacyExact, Missing }` returned instead of a bool so `plan`/`doctor` can say which.
- `crates/relais/src/trust.rs`: `grant_repository`, `show` reports the repository grant, `declaration_hash`.
- `crates/relais/src/ledger/mod.rs`: next ledger step, `repo_declarations (repo_key, declaration_hash, declaration_json, seen_at)`; `commands_changed` run event.
- `crates/relais/src/runner/mod.rs` preflight: the warning, once.
- `crates/relais/src/runner/live_trial.rs`: admission by repository grant.
- `crates/relais/src/main.rs`: `trust grant` without `--key`, `--key` deprecation line, `plan`/`doctor` text; `doctor` prints how many legacy `[trust]` grants were matched in the last 30 days.
- `claude-plugin/hooks/consent.ts`, `runs.ts` (relay the warning), tests in the existing `consent.test.ts` (no new test file).
- `docs/SPEC.md` §5 rewritten (repository grant, legacy grants, the warning, threat model stated as above), §25 live trials; `.adr/records/0007-*.md` (ADR-0007; HEADS.md updated; 0005 confirmed taken or reserved); CHANGELOG.

## Verification

- `make check` exits 0.
- Tests: a repo grant admits any policy of that repository, including a recipe-only candidate (replay preflight and live-trial admission); a repo grant never admits another repository; a legacy exact grant still admits its policy; no grant blocks with `missing_trust_grant`; a changed verification command runs, prints the warning once and records `commands_changed`, the next run prints nothing; a recipe-only change prints nothing; `trust grant` with no key writes `[trust_repos]` at 0600 under the lock; `--key` still works and prints the deprecation line; `the_grant_key_shape_is_pinned` and the legacy-hash tests still pass.
- Plugin tests: the trust tool asks once, then returns `ready` without asking for a granted repository; onboard grants the repository.
- Piloted with the branch binary on scratch `RELAIS_STATE_DIR`/`RELAIS_CONFIG_DIR` in a scratch repository: `relais plan` blocked → `trust grant --reviewed-by` → `plan` trusted; edit a recipe → `plan` trusted, silent; edit `verification` → `plan` shows the pending change twice, `run` warns once, second `run` silent; edit a model tier → `run` silent; a second scratch repository still blocked.
- After release: the person grants the relais repository once through `mcp__relais__trust`; the task-types pilot replay (`task-0e615d0a4320e337`, `change-feature` rev 2) passes preflight with no further grant.

## Delivery

One relais PR, then a release through `tag-release` (the plugin's trust tool needs the reinstall). Then the task-types live phase resumes (pilot, screening) with no per-candidate grants.

## Rollback

- v0.18.0 cannot read a machine.toml that has `[trust_repos]` (`MachineSettings` is `deny_unknown_fields`, `policy/mod.rs:689`), and may refuse the newer ledger schema. Rollback: remove `[trust_repos]`, re-grant with `relais trust grant --key`, restore the ledger from the copy taken before the migration step. The pilot runs this once with v0.18.0.

## Not in scope

- Removing legacy hash grants (the contract step of `migration.expand-migrate-contract`): a later major, once `relais doctor` reports none in use.
- Trust for repositories outside the person's machine (shared or CI settings).

## Decision log

- **D1** (person, 2026-10-13) A repository is trusted once; a change to its commands warns and runs. Chosen over re-prompting on command changes and over keeping per-hash grants.
- **D2** The warning is once per change, stored per repository in the ledger, so it stays readable rather than becoming noise on every run.
- **D3** Legacy hash grants keep working (expand step), so no existing grant breaks on upgrade.
- **D4** A grant keyed by origin URL also covers any other clone with the same origin on this machine (a fork branch in a scratch directory); this follows from D1 and ADR-0007 says so. ADR-0005 and ADR-0006 are reserved by the orchestrator-decides and task-types plans, so this is ADR-0007.

## Record

Done in one PR: commits be49d1c (salvaged from relais run run-65db401540a09-173a3, interrupted at wall time), 8634332 (review fixes), de65b51. Observed before the push, branch binary on scratch `RELAIS_STATE_DIR`/`RELAIS_CONFIG_DIR`:

| check | expected | actual |
|---|---|---|
| `make check` on 8634332 | exit 0 | exit 0 at load ~15: fmt, clippy, module cycles, cargo test, MSRV, audit, plugin 282/282 |
| scratch repo, no grant | blocked | `plan` rc=3, `trust grant: MISSING` |
| `trust grant --reviewed-by pilot` | `[trust_repos]` | rc=0, `[trust_repos.8e87c32c…]` written; `plan` "trust grant: repository …" |
| recipe edit, then model-tier edit | trusted, silent | rc=0, 0 warning lines each |
| verification edit, `plan` twice | pending shown, not consumed | both `plan`s show `commands changed` |
| a second scratch repository | still blocked | rc=3, `MISSING` |
| `doctor`, `trust show` | grant reported, legacy count | "1 repository grant(s), 0 legacy"; "0 run(s) in the last 30 days were admitted by a legacy [trust] grant"; `repository: granted … by pilot`, pending `~ changed check sh -c true [default]` |
| downgrade: v0.18.0 on the new machine.toml | refuses | rc=2, parse error at `[trust_repos.…]` |
| rollback: remove `[trust_repos]`, restore a ledger copy | v0.18.0 works, asks for `--key` | rc=3, legacy `--key` grant asked; the new binary then upgrades a ledger with runs to v30 and writes `ledger.sqlite.bak-v30` |

Not piloted, covered by tests only: `run` warns once then is silent (scenario `a_repository_is_trusted_once_and_a_command_change_warns_once`); a model-tier change never warns (`trust::tests::a_recipe_or_model_tier_change_is_not_a_command_change`); a replay neither says nor records a pending change (`runner::tests::a_replay_neither_says_nor_records_a_pending_command_change`, a bug found in review: replays repeated the warning). Next: the release, then the person grants the relais repository once and the task-types pilot replay runs with no further grant.

## Implementation review

- **approve** after two Deltas (round 1 approve-with-changes, 89k tokens, 154 s; Deltas 97k/46 s and 100k/98 s).
- Fixed: ledger errors no longer swallowed (doctor, pending change); no text says a candidate needs its own grant; a live-trial test under a repository grant alone; `GrantScope` enum; the v30 backup comment; replays no longer repeat the warning.
- Carried: `commandsChangedText` in the plugin has no test; check a real outcome message after release.

<!-- panel: repos=relais reviewers=backend body-sha=dd97337efb51 -->
