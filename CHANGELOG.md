# Changelog

What an upgrader gets, in sentences. Each [GitHub
release](https://github.com/fredericrous/relais/releases) carries the
mechanical pull-request list too, generated; this file is the part a human
wrote, and the release workflow refuses to tag a version whose section is
missing here.

## v0.2.0

A review of the whole crate against the fleet's decision records (2026-09-21)
found ninety defects; this release fixes them. Two are breaking: a trust
grant is now bound to the repository as well as the policy, so every
`[trust."…"]` block in `machine.toml` must be re-issued from `relais plan`,
and the coordinator wire protocol changed, so a running coordinator must be
stopped (`relais coordinator stop`) before the first command of this version.

### Architecture

- **The module graph has no cycles, and a check keeps it that way.**
  `policy`, `route`, `context` and `adapter` each reached into the next
  and back again, as did `ledger` and `runner`, so none of the six could
  be read or tested on its own. The lifecycle's `State` and `Reason` now
  live in a leaf `lifecycle` module the ledger, the report and the
  learning dataset can name without depending on the runner; the glob
  predicates over a contract's write scope live beside the contract; the
  default context budget belongs to `policy`, which hashes it.
  `make lint` and CI run `scripts/check-module-cycles.py`, which fails on
  any cycle and names the files that close it.
- **Policy decides, and touches nothing.** Deciding what a run is
  authorized to do was mixed in with scanning `$PATH`, running
  `<tool> --version`, walking the filesystem for a `relais.toml` and
  writing the init template. Those effects moved to two modules named for
  what they answer — `tooling` for what is installed on this machine,
  `repo` for the policy file on disk — and the same check asserts that
  `policy` names no filesystem, process or environment API.
- **The backend contract belongs to relais, not to its Claude adapter.**
  The `Backend` trait and everything crossing it (`LaunchSpec`,
  `LaunchResult`, `UsageReport`, `Capabilities`, `TurnCeiling`) are a
  `backend` module a second provider can implement without touching the
  adapter, and the generic process supervision sits in `procs` with the
  rest of the platform vocabulary.
- **A route, a launch and a check each report one outcome instead of a
  bag of flags.** Routing returns a route or a refusal carrying at least
  one blocker, so a blocked task can no longer be reported with nothing
  blocking it — and it now prints its reasons rather than only its codes,
  as does a route whose model policy does not name. A process reports
  `Exited(code)`, `TimedOut`, `Cancelled` or `Signalled`, so a check
  killed by the clock is no longer indistinguishable from one killed by a
  signal, and a dispatch's cost is either a reported figure (with whether
  it already covers subagents) or unknown — never an inclusive nothing.
- **`ScriptBackend` is gone.** A public, process-spawning backend that
  nothing referenced, documented against a `relais doctor --dry` flag
  that does not exist.

### Coordinator and admission

- **The coordinator wire protocol is a union, and a stale daemon is now
  detected instead of misunderstood.** Every answer used to be the same
  `ok` with a `known` flag, and "I have never heard of that dispatch"
  looked exactly like "done" to every caller: a worker that bound after
  the daemon re-elected ran on with no seat, no reservation and no PID on
  record. Each outcome is its own reply now, no caller has a catch-all
  arm left, and a `ping` carries the protocol version — so a daemon left
  running from an older install is named at the first call, with the
  `relais coordinator stop` that fixes it, rather than answering in a
  shape this version reads as success. **This is the breaking change in
  the release: stop any running coordinator before the first command of
  this version.**
- **A coordinator that cannot see what is already running refuses to
  start.** It adopted the ledger's live dispatches with
  `unwrap_or_default()`, so a busy database read as "nothing is running":
  every seat was granted a second time and every reservation was lost. It
  now reports the ledger error and starts nothing, leaving no endpoint
  and no lock behind for a client to find.
- **Heavy local commands and indexing share one limit again.** Both come
  out of `max_heavy_commands`, but they were counted separately, so each
  could fill the limit on its own and the machine ran twice the load
  configured — with the overshoot only half reported in
  `relais coordinator status`. Caps are enforced and reported per group
  now, and a `max_heavy_commands = 2` really is two.
- **A run cancelled while a dispatch waited in the queue no longer
  launches it.** A queued request that reached the front while its caller
  was polling was granted without re-checking cancellation. The seat and
  the reservation now come back the moment the caller asks.
- **Idle-exit and shutdown stop mid-flight work being dropped.** The
  daemon decided it was idle under its lock and unlinked its socket after
  releasing it, so a registration served in that window was accepted and
  lost; and the shutdown flag was only read after a connection had been
  accepted, which then got no answer at all ("EOF while parsing"). The
  decision and the flag are now one step, every connection already
  accepted is answered "shutting down" so the caller retries, and the
  socket and the lock go only once nothing is being served.
- **Cancelled workers are signalled off the admission lock, and only if
  their process is still alive.** Cancelling a run sent `SIGTERM` from
  inside the lock, without the liveness check lease reconciliation
  already did — so a recycled PID could be signalled and every other tab
  waited on the syscall. `relais coordinator status` now also lists each
  bound process and how old the check behind it is, which is the closest
  thing to proof that a PID is still the worker it was.
- **A signal the operating system refuses is no longer recorded as
  sent.** `terminate` and `kill` discarded their result, so a cancelled
  dispatch could be marked as asked to stop when nothing had been
  delivered, and a PID that had been recycled (`EPERM`) exhausted the
  escalation ladder and was reported as cancelled. A delivery that could
  succeed later is retried on the next reconcile; one that never can —
  the process is gone, or it is not ours — stops there and says which.
- **The endpoint is never world-connectable, not even for a syscall, and
  only this user may talk to it.** The socket was created with the
  process umask applied (0755 by default) and narrowed a moment later, in
  a window where a protocol carrying `shutdown` and `cancel_run` was
  reachable. It is now bound owner-only, its directory is owner-only and
  verified, and the uid on the other end of every connection is checked
  with the kernel before a byte of the request is read.
- **A flood of connections costs a bounded queue instead of a thread
  each.** The accept loop spawned an unjoined thread per connection and
  could spin at full speed on an accept error that would never pass
  (`EMFILE`); it now hands work to a fixed pool that is joined on exit,
  backs off and stands down when descriptors run out, and skips the
  failures a retry does fix.
- **A run blocked by admission says whether the coordinator was
  unreachable or simply said no.** Every failed admission call was
  reported as `admission_unavailable`, which sent operators looking for a
  dead daemon that was answering perfectly well. Admission errors are
  typed now, and a refusal is reported as `admission_refused`.
- **A worker whose bind the coordinator does not recognise ends the
  attempt instead of finishing unmanaged.** Its usage is settled and
  recorded first, and the run is blocked with the reason.
- **A withdrawn request no longer costs a run one of its agents.**
  Abandoning a request that had been admitted but never launched settled
  it as a finished dispatch, which spent an aggregate agent slot for the
  rest of the run and made the same dispatch ID unusable.

### Ledger, policy and contracts

- **BREAKING: a trust grant is bound to the repository, not just to the
  policy text.** A grant used to be keyed by the hash of `relais.toml`
  alone, so any other repository whose policy hashed the same — and the
  policy `relais init` writes is public and identical everywhere — ran
  its verification commands under a grant nobody had reviewed for it. The
  key is now a hash of the pair (authority hash, repository), where the
  repository is its canonical root path plus its `origin` remote URL when
  git reports one. Every existing `[trust."…"]` block stops matching and
  must be re-issued: `relais plan` prints the block to review and paste,
  including a new optional `repo = "…"` field that says which repository
  the grant is for, and `relais doctor` now has a `trust` line saying
  whether this repository is granted and what the other grants on the
  machine are for.
- **BREAKING: a trust grant must name its reviewer.** `reviewed_by` was
  optional and never read, so a grant nobody had signed counted as
  reviewed. It is required, and `granted_at` is parsed at the boundary
  (RFC3339, or a plain `YYYY-MM-DD`) rather than stored as whatever text
  was there; an invalid grant is a named error from `run`, `plan` and
  `doctor` instead of a valid-looking one.
- **The deny floor is a floor.** The permissions documentation promised
  that a worker cannot commit, merge, push or publish whatever else is
  configured. `disallowed_tools = []` in `machine.toml` quietly replaced
  that list with nothing. The shipped denials are now merged into
  whatever the machine adds, so naming `disallowed_tools` can only widen
  the deny list, never shorten it.
- **The authority hash covers the whole policy by construction.** It
  enumerated its fields by hand, so a new `relais.toml` control was
  executable authority outside the hash and a grant reviewed before it
  survived. It is now taken over the serialized policy minus an explicit
  exclusion list (the schema version), with a test asserting exactly
  that.
- **A migration is all-or-nothing, and two first runs no longer race.**
  Each schema step ran as loose statements with no transaction: a crash
  in the middle of the v3 rebuild left a half-built table that made every
  later `relais` command fail with "table already exists", and two
  processes opening a fresh ledger could both decide a step was
  unapplied. Each step is now one immediate transaction covering its DDL
  and the row that records it, re-checked inside the lock.
- **Two relais processes can open the ledger at the same moment.**
  `PRAGMA journal_mode = WAL` takes a lock SQLite refuses without
  consulting the busy handler, and it ran before the busy timeout was
  even set, so one of two simultaneous openers died with "database is
  locked" before reading a row — the coordinator starting alongside a
  run is exactly that case. The timeout is set first and the mode change
  is retried against the mode actually in force.
- **A transition and the run status it implies are one write.** They were
  two; a process killed between them left a run whose history said
  `accepted` while its status still said `verifying`, and `relais resume`
  then overwrote the accepted run as `interrupted`.
- **A declared write scope is validated where the contract is read.** The
  globs were kept as plain strings and compiled only after a worker had
  run and been paid for, so a mistyped pattern surfaced as a git error at
  the end of an attempt. A scope is now compiled at parse time, and an
  absolute pattern, a `..` segment or a glob that will not compile is a
  contract error naming the pattern. A contract also carries its kind and
  its scope as one value, so an `inspect` with a write scope and a
  `change` without one cannot be built at all.
- **Spending ceilings are money, and the arithmetic saturates.** They
  were raw integers, unvalidated, and the per-package budget was computed
  as `ceiling - spent`, which near the extremes wraps into a number that
  reads as an unlimited budget. Ceilings are parsed into checked amounts,
  a negative one is refused by name, and every remaining-budget
  subtraction saturates at zero.
- **Ledger rows leave the adapter as values.** The stored contract came
  back as raw JSON with an unparseable one silently becoming an empty
  objective, a run's packages came back as untyped status strings, and
  live dispatches as three-element tuples with a bare integer pid. They
  are now a parsed `TaskContract` and tier, a typed child-run record, and
  a dispatch record with real identifiers — and a row this version cannot
  read is reported as corrupt rather than quietly dropped. `relais
  dataset build` now says which runs it could not read instead of leaving
  them out in silence.
- **Identifiers take their inputs.** Run and dispatch ids read the wall
  clock, the process id and a process-global counter from wherever they
  were called, and a clock before 1970 aborted the process. They are
  minted from a source built once at start-up, and a clock that far wrong
  is a reported error. The id format is unchanged.
- **A malformed `machine.toml` stops the coordinator instead of becoming
  the defaults.** `relais coordinator daemon` read the file best-effort
  and fell back to default concurrency limits — which are wider than any
  file would state — when it would not parse. An absent file still means
  defaults; an unreadable or invalid one is now an error.
- **A stored state or reason this version does not know says so.**
  `State::parse` and `Reason::parse` return an error naming what was read
  and what was found, rather than an empty option a caller can mistake
  for "nothing recorded".
- **An inclusive cost total covers its whole subtree.** The
  double-counting guard looked one generation up, so a grandchild of a
  provider total that already included it was added again. It now walks
  the ancestor chain, in `relais report` and in the daily ceiling alike.
- **A replaced contract revision is recorded as replaced.** The
  `superseded_by` column existed and nothing ever wrote it; a new
  revision now closes the ones it supersedes in the same transaction that
  records it. `changed_controls` also counts a changed decomposition as a
  scope change — a work plan partitions the declared scope, so replacing
  it changes what may be written and by whom.

### Runner

### Adapter, verification and workspace

### Learning and routing

- **A run that is still running is no longer a training failure.** The
  dataset labelled every state it did not name explicitly as "the tier
  failed", so runs that were merely prepared, running, verifying,
  repairing or escalating — and runs whose budget ran out before the
  reasoning was ever tested — taught the learner that their tier does not
  work. Only an accepted or failed run is a reasoning outcome now;
  everything else is excluded with the reason printed by
  `relais dataset build`, and every lifecycle state is answered for by
  name, so a new one cannot fall into the negative class by default.
- **`relais dataset build` fails loudly instead of building nothing.** A
  ledger that could not be read — a busy database, a permissions problem
  — produced an empty dataset, exit 0 and the advice to "collect outcomes
  first". Each read now says which run and which query failed, and the
  command exits non-zero. A run with no recorded contract, state,
  transition or dispatch intent is still an exclusion, because absence is
  not failure: what changed is that the two are told apart. A run whose
  dispatch never named a model is excluded rather than credited to an
  empty profile.
- **The evaluator measures the route the router would actually take.** It
  used to choose among every trained tier with no floor, no risk rules
  and no check that policy configures a model there, and to compare the
  result against a "baseline" that pooled research and implementation
  records regardless of each task's own floor. Eligibility and selection
  are now the router's own functions, applied per record to the routing
  floor that record was dispatched under — which the dataset records, so
  datasets must be rebuilt (`relais dataset build`) before training.
- **Promotion needs enough evidence to be evidence.** One supported test
  record could pass the quality gate and the abstention rate was computed
  and never used. Promotion now also requires a minimum number of
  held-out records observed at the tier the artifact selects
  (`routing.min_supported_test_records`, 20 by default) and an abstention
  rate at or below `routing.max_abstention_rate` (0.5), both settable in
  `machine.toml`. An artifact whose solver did not converge, or whose
  calibration temperature came out anti-predictive, fails the gates and
  says so instead of being quietly floored to a weak positive. The report
  prints both fit reports, the temperature and the supported-record
  count, and names every gate that did not hold.
- **`relais promote` verifies the evidence rather than believing it.** The
  evaluation report now carries the artifact id, the dataset fingerprint
  and its schema version, and promotion checks all three and RECOMPUTES
  the verdict from the report's own numbers — a stored `gates_passed`
  edited to `true` no longer promotes anything. "Evaluated" is a type only
  the registry can build, so nothing can promote an id and a hopeful blob.
- **A model swap starts with no evidence.** Coverage was keyed by tier, so
  changing the model, effort or harness behind a tier inherited the old
  profile's acceptance record. Artifacts now carry the profile identities
  training observed per tier, and inference abstains for a tier whose
  current identity is not among them, naming it. Artifacts trained before
  this release are refused by version; retrain.
- **An unreadable artifact says so.** A registry pointer that could not be
  read was treated as "nothing is promoted": inference reported a
  schema-incompatible artifact as an absent one, and promotion overwrote
  the rollback pointer, so one `rollback` went two artifacts back. Only a
  missing pointer is an absence now, the rollback pointer is written
  through the same atomic temp-and-rename as the active one, and
  inference's abstention carries the reason.
- **`relais train` exits with a code that says which way it failed** — no
  training records (3), no tier with enough coverage (4), a solver that
  diverged (5) — instead of a single stringly error. The solver's
  convergence test no longer chases a shrinking loss, so a fit that has
  stopped making material progress is recognized as converged rather than
  burning its whole iteration budget.
- **A risk rule with no paths no longer governs every task.** An empty
  `paths` list was compared against the declared scope as the empty
  pattern, which a `**` scope matches — so a malformed rule raised the
  floor of exactly the broadest tasks. Policy validation already refuses
  such a rule; routing no longer honours one either.
- **`relais report` counts states, not strings.** A run's status is parsed
  into the lifecycle state it names (an unknown one is a corrupt-row
  error, not a silently uncounted run), and the runs awaiting a person —
  `needs_review`, `needs_decision`, `interrupted` — are now what the field
  documents.

### CLI, doctor, install and release

### Tests and documentation

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
