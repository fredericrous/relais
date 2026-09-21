# Changelog

What an upgrader gets, in sentences. Each [GitHub
release](https://github.com/fredericrous/relais/releases) carries the
mechanical pull-request list too, generated; this file is the part a human
wrote, and the release workflow refuses to tag a version whose section is
missing here.

## v0.1.6

### Fixed

- **`relais.toml` is found from anywhere inside the repository.** `plan`,
  `run`, `doctor`, `init` and project-level `install` read the policy of
  the nearest ancestor holding one, stopping at the repository root (the
  nearest `.git`, a directory or a worktree's file). Before, only the
  exact cwd was tried, so a Claude Code session started in a parent
  folder, or a shell sitting in `crates/`, was told to `relais init`
  a policy the repository already had. A nested repository never
  inherits the policy of the one containing it; a directory outside any
  repository is refused by name. `init` writes at the repository root,
  never in a subdirectory where a policy would govern nothing.
- **The `/relais` skill says where to stand.** Its first step is now the
  repository (or task worktree) root as cwd for every command, so a
  session run from a workspace folder that holds many repositories
  routes to the right one instead of failing on the folder itself.
  `relais install --claude --write` updates the installed skill.

## v0.1.5

### Fixed

- **The installed skill and agents are readable by Claude Code.** The
  ownership marker was written as an HTML comment above the YAML
  frontmatter, and Claude Code reads frontmatter only when `---` is the
  first line — so the whole comment became the skill's description and
  the agents had no name. The marker now sits under the opener as a YAML
  comment (`# relais:begin <sha>`); the end marker stays an HTML comment.
  `relais install --claude --write` migrates a file installed with the
  old layout as an ordinary update, and uninstall removes a wholly-owned
  file instead of leaving a `---` stub.
- **The `/relais` skill's own contract no longer refuses the run it
  describes.** The skill writes `.relais/task.json` in the repository,
  and `plan`/`run` refused the tree as dirty because of it. `.relais/` is
  relais's own scratch: never part of a candidate (the worktree is created
  from the base SHA) and never counted as uncommitted work.

## v0.1.4

### Added

- **Write leases reach the runner** (SPEC §23). Every candidate-writing
  dispatch takes the exclusive write lease on its worktree after
  admission and before its process exists, and gives it back when the
  process has ended; the reviewer and the planner, which only read, take
  none. A worktree somebody else is writing refuses the launch outright,
  naming the holder. Before a candidate is snapshotted, and before the
  scheduler verifies an integrated candidate, the runner waits for the
  relevant worktrees' holders to be gone — a wait that happened is a
  transition on the run, and a holder still there at the run's deadline
  ends the run `interrupted` with nothing snapshotted from a tree in
  motion. The three operations travel the coordinator socket, so the
  guarantee holds across tabs; `relais coordinator status` lists the
  leases in force.

## v0.1.3

The rest of the audit: issues #6–#10 closed, five pull requests
(#15–#19), each merged after green checks on Linux, macOS and Windows.

### Changed

- **The acceptance boundary is tighter** (#19). Task worktrees live
  under `<state>/worktrees/<run>/`, not beside the run's receipt, so a
  worker with Bash cannot write `../receipt.json`; verification trees
  are under `<state>/verify/` and release themselves. The tool deny
  floor also covers `git -C`/`-c`/`--git-dir`, `sh|bash|zsh|dash -c`,
  `env git` and `eval` — a floor, not a sandbox. A candidate that edits
  the verification profile's own inputs (manifests, lockfiles, the
  commands' programs, listed `inputs`) ends `needs_decision`; edits to
  the test tree still go to review. Un-waived amont bypasses and
  downgrades are verification gaps (`amont_waivers` on a profile waives
  named checks); a profile with no `amont_checks` takes the inventory's
  in-force blocking checks. A failing `git status` blocks instead of
  proceeding. The reviewer is the strongest tier that did not write the
  candidate. Every git command relais spawns drops `GIT_DIR`,
  `GIT_WORK_TREE`, `GIT_INDEX_FILE` and friends; candidate commits are
  held by `refs/relais/candidates/<run>/<n>` so `git gc` cannot drop
  them; an accepted run's worktree is released once the patch and the
  ref exist, and kept — with both SHAs on the record — if its tree
  moved after the snapshot.
- **The coordinator survives its own failures** (#18). Election holds
  an OS lock (`flock`, `LockFile`) for the daemon's lifetime, so a
  recycled PID after a SIGKILL cannot wedge every `relais run`. A lease
  with nothing bound for three grace periods is reclaimed; ledger rows
  with no PID are the runner's to reconcile, not seats. A cancelled
  dispatch is acknowledged by the runner and freed at once; reconcile
  signals once, escalates to a hard kill after a grace period, then
  stops. EPERM on liveness means alive. `Bind` refuses a dead PID. A
  finished dispatch id is remembered (4096 of them) and refused on
  repeat; over-admission from resumed parents is capped. Twelve threads
  now race the real socket in a test, and write leases exist in the
  state machine (runner wiring is the follow-up).
- **The learner tells the truth about what it knows** (#15). Artifacts
  whose weights disagree with the feature schema's dimension are
  rejected. A task family straddling the temporal cutoff goes to the
  later split, never to train. Calibration bins are measured on held-out
  data. The cost model abstains to its cohort mean outside the range it
  was fitted on and has no $485 ceiling; the cohort is the contract's
  real kind. The seed seeds a stochastic warm-up. A `**` contract is no
  longer "covered" by a `docs/**` recipe.
- **Install upgrades, ledger errors, prompts as data** (#16). An
  installed block is "unchanged" when it hashes to what its marker
  recorded, so a new template upgrades it and uninstall removes it; an
  update splices the block in place; a reordered marker pair is
  reported, not a panic. Stored states and receipts that do not parse
  are errors, a ledger written by a newer relais is refused by name, and
  no CLI command aborts on a ledger error. Every externally-sourced
  string in a prompt sits in a labelled, fenced data block; a planner
  package objective must be one line.
- **No dead configuration** (#17). `per_day_micros` is enforced across
  runs on the UTC day. `[trials]` says it is not implemented when
  enabled. `[context] budget_bytes` is repository policy, hashed, and
  counts the whole package the worker receives. `doctor` respects
  `[integrations]` modes, names the directories in effect, flags
  environment overrides, and reports whether the harness has a turn
  ceiling (recorded in the manifest). `HOME` unset is an error, not a
  panic. The reviewer is spend-gated. Three new release scenarios: a
  wall-clock-killed worker resumes without a second launch; a worker
  editing `relais.toml` ends `needs_decision`; a file written after the
  result is not in the candidate.

## v0.1.2

### Fixed

- **Unknown usage is unknown, not zero.** A dispatch whose harness reported
  no cost was stored as `0` and summed into a figure that read as money,
  with only a completeness label to say otherwise. The ledger's
  `cost_micros` is nullable since schema v3 (the table is rebuilt on first
  open, rows preserved, and every row whose completeness already said
  `unknown` gets the NULL its zero stood for); unknown usage is left out of
  every sum; and every cost line — `run`, `status`, `explain`, `report` —
  reads `unknown (no usage was reported)` or `at least $x (unknown: …)`
  instead of a number. A session with subagents is recorded `inclusive`,
  and the ledger's dedup of inclusive parents is documented as ready for a
  managed nested dispatch that does not exist yet.

## v0.1.1

### Added

- **Windows.** The coordinator's endpoint on Windows is a loopback TCP
  port and a 32-byte nonce written to the same file a Unix socket would
  occupy; a connection that does not present the nonce is dropped before
  a byte of its request is read, and the file's ACL under the user's
  profile is the permission restriction SPEC §23 asks for. Process
  liveness, termination and parentage go through process handles; a
  worker tree is killed with `taskkill /T`. The release ships an
  `x86_64-pc-windows-msvc` zip again and `install/install.ps1` is back.
  Election, the stale-endpoint probe and cleanup are unchanged: they
  see one path and one listener on every platform.

## v0.1.0

The first release: the companion described in `docs/SPEC.md`, usable on a
repository today, with the audit that made it so.

### Added

- **The runner, the machine and the coordinator.** A task contract is
  frozen and hashed, routed by risk floors and a conservative baseline
  (the learned router abstains until it has evidence), executed in an
  owned worktree by a `claude -p` worker, verified against an immutable
  candidate commit, and receipted. The §9 lifecycle is one pure function
  with a test per row. Dispatch goes through a per-user coordinator that
  reserves seats and budget before any process exists.
- **`relais install --claude`**, preview-first, for the `/relais` skill and
  the namespaced agent definitions.
- **The learning loop**: dataset build, train, evaluate, promote — owned,
  local, and pinned per run.
- **Prebuilt binaries** for Linux (glibc and musl, x86_64 and aarch64)
  and macOS (Intel and Apple silicon), with checksums, and
  `install/install.sh` to fetch them verified. (No Windows in this
  release; 0.1.1 adds it.)

### Fixed, before anyone upgraded

The audit in `docs/AUDIT-2026-09-20.md`, run against Claude Code 2.1.278,
found and this release closes: the adapter probing flags the CLI does not
have (`--budget`, `--max-turns`), permission denials read as a worker's
choice, a candidate identity that changed every second, scope judged on the
live tree instead of the snapshot, one protected prefix unlocking all of
them, and a coordinator that idle-exited under a run that was merely
verifying. What remains open is issues #5–#10.

### Not in this release

No crates.io or npm package: `relais` is a working name whose availability
has not been checked. No homebrew formula until the tap is seeded.
