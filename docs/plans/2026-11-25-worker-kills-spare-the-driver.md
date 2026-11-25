---
status: active
branch: prog/follow-ups/f26-worker-kills-spare-the-driver
repos: [relais]
adrs: []
plan-id: p-01M88QJG8FPVZ9N9T3AGR5HG8X
---
# Keep a worker's process kills off its driver

## Review panel
📄 Full reviews: [reference](2026-11-25-worker-kills-spare-the-driver.reviews.md).
👉 **Decide:** none — approve if a physical relative `--task` plus a kill-by-recorded-pid worker rule keeps workers' kills off the driver.
📍 relais · 2 phases · 1 release · 1 step for you · next: Phase 1. Panel: backend, lang:rust, lang:typescript, tui, unix (opus-5-5), codex (gpt-6-astra).
**Changed by review:** relais computes the relative path (`native contract --relative`); the rule names `$!` and bans pid-by-pattern; a release phase.
**Verdicts:** r1 5 approve-with-changes → r2 3 approve, 2 approve-with-changes; codex approve-with-changes; 10 carried; words 808 → 947 (×1.17); remove 0, merge 0.

## Picture

```
 today                                   after this plan
 driver argv:                            driver argv:
   relais run --task                       relais run --task
     /…/relais-wt-f21/.relais/tasks/x.json  .relais/tasks/x.json
     --protocol                              --protocol
 worker: pkill -f relais-wt-f21          worker: the same pattern misses
   -> driver dies, run `interrupted`        the driver; its rules say kill
                                            by pid, never by pattern
```

The driver's argv no longer names the worktree. The worker's rules say
to stop processes by pid.

## Context

Follow-up ticket `docs/programmes/follow-ups/f26-worker-kills-spare-the-driver.md`.
On 2026-11-25 a worker of run `run-6610ef826b9a0-5ea8` ran
`pkill -f "relais-wt-f21"`. The pattern matched its own driver. f24 made
`cancel` of the ended run stop the orphaned worker. Nothing prevents a
repeat.

Facts checked in this session:
- The plugin starts the driver as `relais run --task <path> --protocol`, with
  `cwd` set to the tool's `cwd` (`claude-plugin/hooks/runs.ts:230`, `startChild`).
- A contract object is saved by `relais native contract`, run with the same
  `cwd`. That command prints the absolute path
  `<repo root>/.relais/tasks/<hash>.json`
  (`crates/relais/src/cli/native.rs:244`), and that path goes into argv.
  When the session works in a linked worktree, the path names that worktree.
- `relais run` uses the task path only in `load_contract` (`cli/run.rs:234`).
  That call is `std::fs::read_to_string`, so a relative path resolves
  against the driver's own cwd. The path is recorded nowhere else.
- `pkill -f` and `pgrep -f` match argv, not the cwd or the environment.
- A native worker is a Claude Code subagent. It is not a child of the
  driver: the driver itself spawns only short-lived checks.
- `relais dataset replay --task` takes a task id, not a path. Successor
  intents (`--intent <id>`) carry no path. `decide`'s `--successor-contract`
  goes to a short-lived `relais native decide` and is not the driver.
- The worker's rules are `worker_rules()` in
  `crates/relais/src/runner/prompt.rs:248`. A worker never reads the
  session-facing SKILL.md.

## Approach

Decision: **both**.
- The runner change removes the path that matched on 2026-11-25.
- The guidance covers what argv cannot: `pkill relais`, `killall relais`, a
  subdirectory `cwd`, and processes that are not relais's.

1. **A relative `--task`, computed by relais.** `relais native contract`
   gains `--relative`: it prints the saved path relative to its own
   physical cwd (`std::env::current_dir`), as `../` steps up to the
   repository root, then `.relais/tasks/<hash>.json`. The root is an
   ancestor of that cwd (`locate_repo_root` walks up), so the path holds
   no directory name above the root, and the kernel resolves `..`
   against the physical directory, so a symlinked or subdirectory `cwd`
   still loads. The plugin's run tool asks for `--relative` and passes
   that path to the driver, spawned with the same `cwd`. `decide` keeps
   the absolute path (no flag). A string path the model gives is passed
   unchanged. The plugin is embedded in the binary and versioned with it
   (`install/plugin.rs:8-11`), so the flag cannot be missing.
   The tool's reply names the path it passed.
2. **Worker rule.** `worker_rules()` gains a rule: record the pid of a
   process you start (`$!` after `&`) and stop only that pid with
   `kill <pid>`; never find or stop a process by a pattern (`pkill`,
   `killall`, `pgrep -f`, `ps | grep`), and never `kill 0` or `kill -1`.
   A pattern can match relais's own driver, and the run then ends
   `interrupted`.

No guard parses a `pkill` command: f24 already settled that.

## Phases

- [x] Phase 1. Add `--relative` to `relais native contract`
  (`crates/relais/src/cli/native.rs`, `crates/relais/src/cli/mod.rs`), with
  an integration test in `crates/relais/tests/it/portable_scenarios.rs` run from the root and from a subdirectory.
  Change `claude-plugin/hooks/runs.ts` (`contractPath` takes an option the
  run tool in `claude-plugin/hooks/register.ts` sets) and
  `claude-plugin/tests/spawn.test.ts` (through the run tool with a contract
  object, `native contract` stdout scripted via `script.runResult`).
  Change `crates/relais/src/runner/prompt.rs` (`worker_rules`), the rule
  wrapped to lines of at most 80 columns (`runner/tests/mod.rs:7742`). Add a
  `contains` assertion for the new rule to
  `the_prompt_carries_the_native_rules_and_no_headless_wording`
  (`runner/tests/mod.rs:7715`) and to
  `every_attempt_kind_carries_the_native_rules` (`:8191`).
- [ ] Phase 2. Release relais (the `tag-release` skill) and install the
  released binary: the plugin is embedded by `build.rs` and cached by its
  version, which is relais's (`crates/relais/src/install/plugin.rs:8-11`).

### 🧑 Your steps

- `relais install --claude` (reinstall the relais plugin) — because the argv change ships in the plugin, and the plugin here is at 0.72.0.

## Verification

| Check | Input | Expected |
| --- | --- | --- |
| plugin tests | `claude plugin test` in `claude-plugin/` | all pass. New: the run tool with a contract object and cwd `/wt/relais-wt-x` calls `native contract --relative` and spawns `--task` with its stdout; no argv element contains `relais-wt-x`; `decide` still calls it without the flag. |
| relative flag | `cargo test -p relais --test it native_contract` (beside `native_contract_saves_a_contract_and_refuses_prose`, `tests/it/portable_scenarios.rs:1975`) | from the root it prints `.relais/tasks/<hash>.json`; from a subdirectory `../.relais/tasks/<hash>.json`, and `relais plan --task` loads both |
| new tests fail on main | the new test copied into a detached worktree of origin/main | the relative-path tests fail |
| prompt pins | `cargo test -p relais --lib runner::` | pass; the two new assertions fail with the rule line removed |
| piloted | in a scratch repo: `relais native contract` < a contract, then `relais plan --task .relais/tasks/<x>.json` (the same `load_contract`) | the relative path loads; after the reinstall, `pgrep -fl <worktree-name>` (the matcher `pkill -f` uses) lists no driver pid, while `pgrep -fl relais` still does |

### Observed (Phase 1)

| Check | Input | Actual |
| --- | --- | --- |
| plugin tests | `sh scripts/plugin-test.sh` | 602 pass, 0 fail; the new run-tool test and the `decide` no-flag assertion ran |
| new tests fail on main | spawn/successor tests and `runner/tests/mod.rs` copied onto a detached origin/main | the run-tool test fails (601 pass, 1 fail); both prompt pins fail (2 failed) Run before the child-process clause, whose two new assertions name text origin/main does not have. |
| prompt pins | `cargo test -p relais --lib runner::`, on the tree with the child-process clause | ok, 555 passed, including both pins with the `pgrep -P` and "direct children only" assertions |
| relative flag | `cargo test -p relais --test it native_contract` | 3 passed, including root, `deep/er`, and a symlink to it; `plan --task` gets past the contract load from each |
| piloted | built binary in a scratch repo with linked worktree `/tmp/…/relais-wt-pilot` | `--relative`: `.relais/tasks/60b983a40d7802da.json` from the root, `../.relais/tasks/…` from `sub`, both load; without it: `/private/tmp/…/relais-wt-pilot/.relais/tasks/…` (the shape that matched). The live-driver `pgrep -fl` check waits for the release and reinstall. |
| lint | `make lint` | rc 0 |
| child processes | under `zsh -c` and `bash -c`: `sh -c "sleep 30 & sleep 30" &`, then `kill $(pgrep -P <pid>) <pid>`; and a grandchild tree `sh -c "sh -c \"sleep 31; true\" & wait" &`, killed deepest first | no child left in either shell; the grandchild tree: parent, child and grandchild gone, checked by pid |
| `set -m` | `zsh -c 'set -m'`, `zsh -c 'setopt monitor'` | both refused: `can't change option`; macOS has no `setsid` |

Carried from review, done: symlink case, `/` joins, the pilot passes `--relative`, and `decide` keeps the absolute path, with a test. Kept deliberate: the reply already reads `… for: <path> (in <cwd>)`, so it names the cwd the path is relative to.

## Implementation review

- implementation-review: approve after round 1 (1 blocking, flag argument → `ContractPathForm` in the plugin (`runs.ts`), `ContractPath` in relais; 2 fixed), Delta, and rounds on the `pgrep -P` change; 49k+36k+37k+35k tokens.
- backend, lang:rust, lang:typescript, unix: all approve at the end. Fixed: the doc block placement, the silent fallback (now on stderr), exhaustive matches, the `None` tests, and children-first kills. All five re-approved after the rebase onto v0.85.0 (tree 9ed048bbb10b).
- deliberate: the plugin drops the fallback's stderr note → f30 (the fallback should not fire, since the root is found by walking up from the same cwd; not measured, f30).
- deliberate: a child forked after `pgrep -P` runs can be missed (unix low, accepted).
- `set -m` / `kill -- -<pid>` (unix round 1) replaced after this session measured `zsh -c` refusing it.

## Non-goals

- Guarding `pkill` commands in a hook.
- Moving contracts out of the repository.
- The replay `--recipe` path.

## Code review
- relais: backend, lang:rust, lang:typescript, unix (tui left out: no pane or terminal output changes)

## Decision log

- Both changes, not one: argv alone misses `pkill relais`; guidance alone
  depends on the worker obeying.
- The worker rule gained a clause for commands that start children:
  `kill $(pgrep -P <pid>) <pid>`, run again on each child, deepest first.
  `pgrep -P` matches a parent pid, not a pattern. The first wording, `set -m`
  then `kill -- -<pid>`, was dropped: `zsh -c`, the shell workers get on
  this machine, refuses `set -m`, and macOS has no `setsid`.
- The plugin drops the stderr note that `--relative` prints when it falls
  back to the absolute path. Filed as f30, not fixed here: the root is
  found by walking up from the same cwd, so the fallback should not fire
  (not measured).

<!-- panel: repos=relais adds= reviewers=backend,lang:rust,lang:typescript,tui,unix,codex body-sha=7735ec441a8d -->
