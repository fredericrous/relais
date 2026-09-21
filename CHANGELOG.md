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

- **A worker that backgrounds something no longer holds the run open.**
  When a dispatch ended, its whole process group is now killed before its
  pipes are drained, on every path including a clean exit: a `npm run dev &`
  or an MCP server left behind inherits the worker's stdout, and reading
  that pipe used to block until THAT process ended — past the wall clock,
  with the write lease and the worktree still held. The result of the kill
  travels with the launch ("nothing left", "survivors killed", or the
  refusal), draining is bounded, and a file a descendant writes after the
  terminal result can no longer land in the tree at all. The same applies
  to verification commands, so a `cargo test` grandchild is never left
  running in a worktree about to be removed.
- **A prompt that did not reach the worker fails the launch.** The prompt
  was written by a detached thread whose result nobody read, so a harness
  that exited early read a truncated task and its answer was still
  recorded as a completed attempt. A partial write is now an interrupted
  attempt naming the write error — except when relais itself killed the
  process, where the cancellation or timeout stays the outcome.
- **An unreported model is a gap, not an agreement.** The adapter used to
  fill in the requested model when the harness named none, which made the
  substitution check structurally unable to fail. The effective model is
  now absent when the harness reports nothing, and a completed dispatch
  that cannot say which model ran blocks with `model_unverified` instead
  of being credited to the route it was supposed to test.
- **The worker gets an explicit environment.** Every launch starts from a
  cleared environment and receives an allowlist: the machine's own
  variables, the Anthropic, Bedrock, Vertex and Foundry credentials Claude
  Code documents, and proxy/CA settings. An ambient `GIT_DIR`,
  `GIT_WORK_TREE` or `GIT_INDEX_FILE` — inherited from a rebase or a hook
  shell — can no longer redirect the worker's commits into the user's own
  repository, and the context manifest records which variables (names
  only, never values) reached the worker.
- **`git`, `aval` and `amont` each have one home.** They are now ports the
  crate owns (`workspace::Git`, `context::DecisionResolver`,
  `verify::HookInventory`) with a single production implementation each
  and a fake for tests, so the `ls-tree` that fingerprints a read hint no
  longer bypasses the `GIT_*` scrubbing the rest of the crate does, and a
  run can be tested without any of the three installed.
- **Probes are bounded, cancellable and asked once.** `claude --version`,
  `claude --help`, `aval resolve` and `amont list` run under a timeout and
  the run's cancel flag instead of blocking forever, and the harness is
  probed once per process rather than twice per dispatch.
- **The baseline cache key names the toolchain that produces the verdict.**
  It used to hash relais, aval, amont and Claude Code — none of which
  decides whether `cargo test` passes — so after a `rustup update` a real
  regression came back as "it failed at the base too". The key now carries
  each program the profile runs, its resolved path and reported version,
  plus the machine's OS and architecture; a program that cannot be
  versioned refuses caching for that profile, with the reason in the
  receipt.
- **`.relais/` is recognised when `relais.toml` sits below the git root.**
  `git status` prints paths relative to the git root, so with the policy in
  `crates/relais` the contract the run was written for was reported as the
  user's uncommitted work and refused the run it described.
- **Protected configuration is protected at any depth.** `src/CLAUDE.md`,
  `src/.claude/settings.json` and a nested `AGENTS.md`, `.relais/`,
  `relais.toml`, `amont.conf`, `.adr.yaml` or `.gitignore` are refused
  inside an ordinary `src/**` scope, as the root ones already were —
  Claude Code loads the nested files just the same. `.forgejo/workflows/`,
  `.gitea/workflows/` and `.gitlab-ci.yml` join `.github/workflows/`.
- **Manifests and lockfiles count wherever they live.** A monorepo
  candidate editing `apps/web/package.json`, `services/api/go.mod` or a
  nested `pyproject.toml`, `Makefile`, `justfile` or lockfile now needs a
  decision from the user, instead of being accepted because the list was
  anchored to the repository root.
- **A verification-input pattern that will not compile blocks the run.**
  It used to be dropped silently, leaving the input it was written to
  protect unguarded, while a write scope with the same mistake was
  refused. The error names the pattern. A verification command declaring
  `timeout_seconds = 0` is likewise refused instead of quietly becoming a
  one-second check.
- **amont's inventory is read in the verification worktree.** It was read
  in the user's mutable checkout, so the receipt could bind a candidate to
  an inventory of a different tree; the stage is now an explicit
  `Local`/`Pushed` rather than a bare `true`/`false` at the call site.
- **The Claude binary is resolved to an absolute path.** A relative
  `RELAIS_CLAUDE_BIN` is refused, and a PATH hit is canonicalised, so the
  binary that was probed is the binary that runs — the launch's working
  directory is the worker's worktree, which could otherwise resolve a
  repository-committed `node_modules/.bin/claude`.
- **A read hint that names nothing is a preflight problem.** It used to
  produce no fingerprints and no word about it; the run now blocks with
  `read_hint_unresolvable` naming each hint, and a hint whose listing hits
  the fingerprint cap is flagged as truncated instead of having a sentence
  written into a fingerprint's path field.
- **A verification worktree that cannot be released says so.** The runner
  releases it and reports what git said, with `Drop` as the last resort
  rather than the only path; both git results used to be discarded.

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
