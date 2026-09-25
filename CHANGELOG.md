# Changelog

What an upgrader gets, in sentences. Each [GitHub
release](https://github.com/fredericrous/relais/releases) carries the
mechanical pull-request list too, generated; this file is the part a human
wrote, and the release workflow refuses to tag a version whose section is
missing here.

## Unreleased

### Added

- **A queued spawn now waits for a seat instead of being refused at
  once.** Measured on Claude Code 2.1.282: a `PreToolUse` hook holds its
  tool call open for as long as it runs, so relais no longer has to
  refuse a spawn the moment it queues — the refusal that used to say "a
  hook cannot hold a tool call open" was false. `queue_wait_secs` under
  `[admission]` in machine.toml (default: 2 seconds) names how long a
  queued spawn polls for a freed seat before giving up; zero keeps
  today's immediate refusal. The polling re-asks the coordinator's
  existing admission protocol at `runner::ADMISSION_POLL`'s cadence — the
  same one `relais run` already uses — so no new coordinator request or
  wire message was needed. The `PreToolUse` handler timeout `relais
  install --claude --hooks` writes is now derived from the wait (plus
  the coordinator's own connect and round-trip timeouts, plus a margin)
  rather than left absent — an absent `timeout` field waits
  indefinitely, measured at 300 seconds with nothing suggesting a
  ceiling — and every other installed handler now carries an explicit,
  modest one too. `relais doctor` fails (not warns) when a recorded
  settings.json timeout no longer covers the configured wait, since the
  hook cannot read its own handler timeout and a stale one silently
  turns every capped spawn into an admission nobody decided. A hook
  killed mid-wait cannot strand the seat it was waiting on: an admitted
  but never-claimed dispatch is now reaped on its own short grace
  (`admission::UNCLAIMED_GRACE`, 5 seconds) rather than a lease's
  300-second one. The hook journal now records `waited_ms` and an
  `outcome` (`immediate`, `admitted_after_waiting`, or
  `refused_after_waiting`) for every firing.

- **`relais coordinator status` and `relais report` now derive their
  enforcement line from the coordinator's own counts instead of printing
  a fixed sentence nothing computed.** Every `DispatchRequest` now names
  a `DispatchSource` (`ManagedRun`, `HookAdmitted`, or `Observed`, the
  caller's own label, defaulted to `ManagedRun` for anything
  deserialized without one); `StatusSnapshot` carries live and admitted
  counts by source plus expired-but-unreaped hook leases; and
  `Enforcement` gains `Observed` for the honest case where nothing is
  capping a session at all. The rendered line names both halves of the
  hook-admitted path: what IS capped (agent count per session and per
  run) and what is NOT (depth — a hook payload joins no spawn to its
  parent — and spend — the run a hook registers carries no budget).
  `relais report`'s JSON carries the same summary under a new
  `enforcement` key (`REPORT_SCHEMA_VERSION` 5 → 6).

### Changed

- **BREAKING: the coordinator wire protocol is now 3
  (`PROTOCOL_VERSION` 2 → 3).** Two requests were added —
  `BindAgentLease` and `SettleByAgent` — and a coordinator left running
  from an older install does not have them. Run `relais coordinator
  stop` before upgrading (or after: the first command against a v2
  daemon names the skew and says the same thing), and the next command
  starts a coordinator that matches.

### Fixed

- **The per-session agent cap binds on the hook path.** A spawn's seat
  used to be given back on the Agent tool's `PostToolUse`, taken to be
  the agent's end. It is not: a real Claude Code 2.1.282 session showed
  an async spawn's `PostToolUse` arriving ~100ms after its `PreToolUse`
  with `duration_ms: 8` and `tool_response.status: "async_launched"`,
  while the agent ran 6–16 seconds more — so every seat came back at
  launch and `max_active_agents_per_session` admitted agents past the
  cap. The seat now follows the agent: admitted on `PreToolUse`, bound
  on `PostToolUse` to the agent its `tool_response.agentId` names (the
  only payload that names a tool call and its agent together), and
  given back on that agent's `SubagentStop`, found by session and agent
  id. A synchronous spawn, whose `SubagentStop` arrives before its
  `PostToolUse`, is handled too: the coordinator remembers the stop and
  the late bind ends the dispatch at once. An agent whose
  `SubagentStop` never arrives holds its seat until its lease lapses —
  a cap that over-counts for that long, never one that under-counts.
  `hook::event::ToolCallPhase::Post` now carries `launched_agent`, the
  only phase that can.

## v0.4.0

### Added

- **A hook payload now parses into a typed `HookEvent`.** `relais::hook::event::parse`
  turns the bytes a Claude Code hook receives on stdin into `SessionStart`,
  `SessionEnd`, `SubagentStart`, `SubagentStop`, an `AgentToolCall` (the
  Agent tool, or its legacy name `Task`, at any of the three tool-scoped
  phases), or `NotOurs` — the one case for a tool event on any other
  tool, and for a payload that is not JSON, is not an object, names no
  event, names an event this binary does not know, or exceeds a
  one-mebibyte cap. It never fails outward: a hook cannot refuse to
  answer. Fields the crate has no use for (`cwd`, `effort`,
  `permission_mode`, `transcript_path`, and more a future harness
  release will add) are deliberately not rejected, unlike this crate's
  usual `deny_unknown_fields` habit — every one of the nine payloads
  transcribed under `crates/relais/tests/fixtures/hooks` parses, a test
  that walks that directory rather than naming files so a payload a
  later probe run adds is covered automatically. The session, tool use,
  agent, agent type and prompt each get their own newtype (`SessionId`,
  `ToolUseId`, `AgentId`, `AgentType`, `PromptId`) so a function taking
  two of them cannot be called with them swapped. This package decides
  nothing, admits nothing and records nothing — it only establishes
  what a payload IS.

- **`relais doctor --probe-hooks` captures what Claude Code's hooks
  actually send, from a real session.** `relais hook --probe --record
  <dir>` is a record-only handler: it reads one hook payload on stdin
  and writes it verbatim, deciding nothing about its shape — a hook that
  exits non-zero or writes to stdout can block or alter the tool call
  that triggered it, so the handler cannot fail whatever it is handed
  (invalid JSON, an oversized payload, an event it was not wired for all
  still record and exit 0). `--probe-hooks` wires that handler into a
  throwaway settings file for the seven targets relais will use —
  `PreToolUse`/`PostToolUse`/`PostToolUseFailure` on the Agent tool,
  `SubagentStart`, `SubagentStop`, `SessionStart`, `SessionEnd` — runs
  one real `claude -p` session that forces a nested agent call, and
  writes a compatibility record naming the Claude Code version observed
  and which fields each target's payloads carried, reported as what was
  seen rather than what was expected. `relais doctor`'s normal report
  now flags that record as stale when the installed Claude Code no
  longer matches the version it names. The probe has its own `make
  probe-hooks` target — it needs a real Claude Code, costs money and
  touches the network, so it is never part of `make check`.

- **`relais install --claude --hooks` wires the live hook into
  `settings.json`, and `relais doctor` proves it actually refuses.**
  Until now `relais install --claude` wrote four owned files and never
  touched `settings.json` at all, so enforcement was one hand-edit away
  from existing and read, to anyone who had not made that edit, exactly
  like enforcement that was in force. `--hooks` (requires `--claude`,
  preview-first like every other install action) adds one relais
  handler on each of the seven targets the compatibility matrix names —
  the three tool-scoped events matched to the Agent tool and its legacy
  name `Task`, the four lifecycle events matched to nothing — recorded
  as the absolute path of the running relais binary, never a bare name
  PATH could fail to resolve. `settings.json` is a file a person
  maintains by hand, not a marked, hash-checked block like the agent and
  skill files, so every write round-trips the file through `serde_json`
  first (`preserve_order` stays off, so keys sort and
  `canonical_json_hash` stays stable) and refuses to write at all when
  the file cannot be re-rendered byte for byte — nothing is written, and
  the fragment to paste in by hand is printed instead. The single
  tolerated difference is the trailing newline `serde_json` does not emit
  and editors add; the file is written back with the tail it arrived
  with, and trailing blank lines or a `\r\n` tail are refused like
  anything else relais cannot reproduce. A handler someone else already
  has on one of these events is joined into rather than duplicated beside
  when it sits on the matcher relais installs — an entry on a different
  matcher gets its own, since widening somebody else's matcher would
  change which tool calls their handler sees. A re-run says the file is
  already current; uninstall removes only the leaf command relais itself
  added, never an entry object at that granularity, because relais cannot
  tell an entry it left empty apart from one that was already empty when
  it arrived; and a settings.json relais cannot READ is an error, never
  treated as an absent file to be replaced with one relais composed.
  `relais doctor`'s ordinary run now reports the live hook by exercising
  it: it spawns the exact command recorded in `settings.json` with a
  fixture `PreToolUse` payload on stdin, its config and state
  directories redirected to a scratch environment configured to refuse
  when the coordinator is unreachable, and distinguishes nothing
  recorded, a recorded hook that refused, a recorded hook that ran but
  did not refuse (the dead-guard case that looks installed and enforces
  nothing, reported with how it ended) and — kept apart from that
  verdict — an exercise that could not be staged or spawned at all, where
  what the hook would do is reported as unknown rather than guessed.
  Every settings.json write goes
  through the existing atomic stage-and-rename, so a settings.json that
  is a symlink is replaced rather than written through, and a plain
  `relais install --claude` (no `--hooks`) still reads and writes
  nothing under that name.

- **A hook now decides what to say about one event, purely.**
  `relais::hook::decide::decide` takes the typed event, the machine's own
  admission settings and whatever the coordinator answered, and resolves
  to one of exactly two things: `HookAnswer::Silent`, or
  `HookAnswer::Refuse { reason }` naming what was exceeded and what the
  person can do about it — never an affirmative answer, since a hook that
  said yes on a person's behalf would override their own permission
  settings. Only a spawn still in `PreToolUse` is ever refused; every
  other event, `PostToolUseFailure` included (it has never fired in five
  probe sessions), resolves silent. A coordinator that could not be
  reached is handled by the stance `HookAdmissionSettings::on_coordinator_unreachable`
  already names, rather than guessed at here, and `decide_or_silent`
  catches any panic and turns it into silence rather than leaving a tool
  call unanswered. The function reads no clock, touches no filesystem and
  makes no network call, so every reachable case is exercised by a
  table-driven test rather than sampled.

- **Labelling reads a task's recorded outcome, not just its run's
  terminal state.** A task whose latest recorded outcome is `reverted`
  or a confirmed regression is never a positive label, whatever its
  run's state said — labelling is a pure, exhaustive function over both.
  A training example now carries what the outcome said: whether the
  change stood, and a correction's magnitude when one was recorded. The
  sampling unit is the task, not the run, so a task retried to
  acceptance contributes one example, not one per attempt. A task
  accepted through a person's approval rather than verification alone
  is labelled honestly — the example records which route accepted it —
  and does not earn the positive label verified acceptance requires.
  `relais feedback` now records an outcome for an accepted run that
  carries no receipt, the case a person's approval creates for a run
  interrupted before verification ever wrote one; a receipt, whenever
  one does exist, is still checked against a `--candidate` that
  disagrees with it. The ledger's `outcomes.candidate_sha` gains a
  migration (v10) making that optionality explicit and tested. The
  label policy is versioned (`LABEL_POLICY_VERSION`), and the dataset
  and artifact schema versions move with it (dataset v3, artifact v4)
  so a dataset or artifact built under the old rule is distinguishable
  from one built under this one.

- **An evidence row says what it is and what it is about.** `kind` was
  free text; it is now a typed value covering every kind this ledger has
  ever stored (`check_log`, `context_manifest`, `worker_result`,
  `candidate_patch`, `review_result`, `receipt`, `setup_log`) plus two
  this package introduces (`external_attestation`, `human_sign_off`) —
  a stored name this binary does not know now reads as a corrupt row,
  never silently dropped or coerced. A row can also name the tool and
  external id it came from and the subject and acceptance criterion it
  is about; all four are optional, so every row relais already writes,
  and every row written before this package, reads unchanged. The
  evidence table gains these columns in their own migration (v11),
  proven against a ledger built at exactly the prior schema version.
  `relais evidence attach` records one such row — evidence a tool
  outside relais produced, against a run and, optionally, the criterion
  it answers — and decides nothing about it: no criterion is marked
  met, no run's state changes, no gap clears. It refuses a run it does
  not know and a criterion id the run's contract does not declare,
  naming the ids it does. `relais explain` now prints evidence through
  this typed row rather than the positional tuple it read before.

- **A declared criterion can name an amont gate as its evidence.** A new
  `Evidence::AmontGate { gate }` sits alongside the named check, test,
  review and sign-off a criterion could already declare, and settles the
  same way every gap already does: relais asks amont's own documented
  interface, `amont attest covered <gate>`, on the candidate's own tree
  — never by reading `refs/notes/amont-attest`, parsing an attestation
  format, or verifying a signature here. The gate and its attestation
  stay amont's to own. A gate amont reports covered is met and
  independent (it ran outside this run, and a valid signed attestation
  is amont's own verdict), and records an external-attestation evidence
  row against the run and the criterion it answers, naming amont as the
  tool and the candidate SHA as the subject. amont's interface is
  fail-open by design — it prints nothing and exits 0 whether an
  attestation is absent or one whose signature failed to verify — so a
  gate not reported covered is a gap through the same mechanism that
  already refuses gaps, never a second acceptance path and never a
  silent pass; its message says it cannot tell which of the two amont
  meant, and names `amont attest covered` as the command a person can
  run to see the same answer. amont being absent, too old for the
  subcommand, or failing any other way is likewise a gap naming the
  cause relais actually observed, never a silent pass and never a
  crash.

- **The reviewer is asked about stored facts, not only computed
  ones.** Every finding the review loop missed over v0.4.0's packages
  shared one shape: a second record of a fact, left disagreeing with the
  first — a ledger row written without its file, a receipt still naming a
  gap a person had answered, two hand-written copies of one query. The
  prompt now asks, for every fact a change records, where else that fact
  is already written and whether the change keeps them in step; and, for
  every fact it reads, whether the newest row matching a state is really
  the event that caused it, since a retired worktree appends a same-state
  transition after a run is already terminal.

- **A person's answer to `relais decide` now ends the run they
  answered.** `--answer approve` moves the run to `accepted`, and
  `reject`, `revise`, `decided` and `abandon` all move it to
  `cancelled` — a run a person answered is no longer reported as still
  awaiting one. The record says who judged it: an accepted run's
  transition carries `decision_approved` when a person approved it,
  distinct from the `checks_and_review_passed` the runner itself writes,
  so nothing downstream has to guess who vouched for a candidate.
  `relais decide --answer approve --criterion <id>` additionally records
  a human sign-off against one declared acceptance criterion and
  re-seals the run's receipt with it — refused when the contract
  declares no criterion with that id, naming the ids it does declare. A
  mandatory criterion whose evidence is a human sign-off still ends a
  run `needs_decision` without ever launching a reviewer for it; that
  answer, and nothing else, is what clears the gap it raised.
  `relais feedback` accepts a person-accepted run exactly like a
  relais-accepted one, since both are simply `accepted`. `relais report`
  now prints accepted tasks judged by relais and judged by a person as
  two separate counts, and its `cost per accepted task` line names the
  counts behind the denominator it used.

- **A contract can now declare which check, test, review or sign-off
  settles an acceptance criterion.** An acceptance entry is still a
  bare string, exactly as before, or a declared criterion carrying a
  statement, an optional id, whether it is mandatory, and its evidence:
  a named check from the verification profile (`CommandSpec` gains an
  optional `name`), a test recorded as pre-existing, human-added or
  model-added, an LLM review, or a human sign-off. A declared criterion
  naming a check no profile defines is refused at preflight, before any
  dispatch. A mandatory criterion whose evidence never produced anything
  to check becomes a gap in the existing verification report — refused
  by the mechanism that already refuses gaps — rather than a second
  acceptance path; a criterion settled only by a model-added test or an
  LLM review is still accepted once the checks pass, because a model
  writing the test that judges its own work is not independent evidence
  about it. A mandatory criterion asking for a human sign-off is always
  a gap for now: relais has no record of a person signing anything, and
  reading its answer off the passing checks would report a sign-off
  nobody gave. The receipt now records, per criterion, whether it was
  met and by which evidence, plus a summary of how independent the
  mandatory criteria's evidence was — for a decomposed run too, settled
  against the root contract its assembled revision was verified on. A
  contract written entirely in bare strings hashes exactly as it did
  before this change.

- **A usage event now says what produced it.** Every recorded dispatch —
  a worker attempt, the reviewer, the planner — carries its phase
  (`initial`, `repair`, `escalation`, `review`, `planning`,
  `integration`), the dispatch's own elapsed time, the model and effort
  the route asked for beside the one the harness actually reported
  running, and the harness identity. `relais report` prints a run's cost
  broken down by phase, each figure labelled with its completeness, so a
  reviewer that outspent its worker shows up as such rather than
  disappearing into one undifferentiated total.

- **A contract limit it does not write no longer narrows anything.**
  `limits.attempts` and `limits.wall_seconds` defaulted to 3 and 1200,
  and the effective authority is the intersection of repository and
  contract — so a repository that raised its wall clock to 2700 s kept
  watching workers killed at exactly 1200, twice on the machine that
  raised it, both times read as a model running long. Both fields are
  optional now: absent means the repository's, and what a contract does
  write still narrows and still cannot broaden. **A contract that omits
  `limits` hashes differently than it did**, because the block no longer
  materializes into its canonical form; one that writes them is
  unchanged.

- **`relais decide` answers a run that is waiting for a person.** A run
  landing on `needs_review`, `needs_decision` or `interrupted` now opens
  a decision record the moment it does — the same transaction as the
  transition that raised it, so a run can never be waiting without one.
  `relais decide <run> --answer <approve|reject|revise|decided|abandon>
  --actor <who> [--note <text>] [--successor <run>]` records who decided
  what and when, unblocking `relais report`'s "open decisions" heading
  and giving `relais explain` the resolution and how long it waited.
  `approve` is refused, naming the gaps, while the run's verification
  still carries them — waiving a gap is a policy or contract change, not
  a CLI flag.

- **`relais report --by <dimension>` compares like task classes instead
  of blending every task into one number.** `--by task-class|tier|model
  |policy|repository` groups the window's tasks into cohorts; each
  cohort line carries its task and standing counts, cost per accepted
  change, acceptance rate over tasks that reached a terminal state (a
  task still in flight is reported separately rather than counted as a
  failure), escalation rate, review-correction rate printed with its own
  denominator (tasks whose reviewer actually ran, not every task),
  median duration, regressions and pending feedback. A task with no
  known value for the dimension lands in a named `unknown` bucket rather
  than being dropped. Omitting `--by` leaves the report exactly as it
  was; the JSON gains one `cohorts` key beside the existing ones.

- **Machine settings now carry what a hook-admitted agent needs that no
  other field states.** `MachineSettings.admission` is a new, optional
  `[admission]` table: `binding_lease_secs` (how long a binding may be
  held before it lapses, default 120), `dispatch_reserve_micros` (what
  one dispatch reserves against its session's money, default zero —
  observation, not refusal), and `on_coordinator_unreachable`, a named
  `carry_on`/`refuse` choice rather than a boolean, defaulting to
  `carry_on` so a restarting coordinator does not turn into every agent
  being refused. The agent and depth caps already on `ConcurrencyLimits`
  are not redeclared. A `machine.toml` written before this table existed
  still parses unchanged. `ids::derive_dispatch_id(session, tool_use)`
  and `ids::derive_run_id(session)` are new pure functions — the same
  inputs always derive the same identity, reading no clock, counter or
  environment — so a hook that fires twice for one tool call derives one
  `DispatchId`, not two.

- **A dispatch can now be bound to a hook-admitted agent, which has no
  process id to check.** `AdmissionState::bind_agent_lease` binds a
  dispatch to an `agent_id` on a lease held for `agent_lease_ttl`
  (`set_agent_lease_ttl`, default `DEFAULT_AGENT_LEASE_TTL` = 120s,
  mirroring `binding_lease_secs`) rather than a pid the process table can
  check, coexisting with the existing pid-checked `bind` rather than
  replacing it — the hook path never reports a pid, so the old `bind`
  simply cannot serve it. Reconciliation reports a lapsed lease as its
  own `ReconcileReport::expired`, distinct from `dropped` (a process
  found dead) and `unbindable` (a pid-bind that never arrived): the
  first says a hook stopped reporting, the second says a process ended,
  and conflating them would lose that distinction. Every binding this
  way carries a `Provenance` — `Known`, `Inferred { basis }` or
  `Unknown` — because nothing in a single hook payload joins the tool
  call that admitted an agent to the agent that then ran (the spawn
  carries no `agent_id`, the start carries no `tool_use_id`). The new
  `relais::hook::pairing::pair` makes that join from arrival order: a
  spawn with exactly one `SubagentStart` waiting behind it pairs as
  `Known` (`tests/fixtures/hooks-concurrent` recorded five overlapping
  agents whose spawn was, every time, immediately followed by its own
  start), and a start arriving with more than one spawn still waiting
  pairs the oldest as `Inferred`, naming the ambiguity as its basis — a
  case no recorded session has produced, so its test is built from an
  invented sequence rather than a fixture. `StatusSnapshot.leased_agents`
  reports every such binding alongside `bound_processes`, so a reader of
  a snapshot can tell which kind a dispatch's binding is.

- **`relais hook`, run with no flags, now answers for real.** It reads
  one payload on stdin, asks the coordinator about a spawn (only a
  `PreToolUse` on the Agent tool — every other event is answered with
  no call at all), applies the existing pure `hook::decide::decide`,
  prints a refusal to stdout when there is one and nothing at all when
  there is not, and on a refusal withdraws the request it asked about
  so a refused spawn is never left holding a reservation. The command
  exits 0 on every path — a payload that is not JSON, an unreachable
  coordinator, a missing or invalid machine.toml, a panic anywhere
  inside — because a non-zero exit from a hook fails the tool call it
  was watching, and a relais that cannot answer must look exactly like
  one that had nothing to say. Every firing is journalled as one JSON
  line to `hook_journal.jsonl` under the state directory, created
  owner-only (mode 0600) since a payload names a person's transcript
  path and working directory. The machine's `binding_lease_secs` now
  governs the lease a hook-admitted agent is actually held on:
  `coordinator::Coordinator::start` reads it from machine.toml and
  calls `set_agent_lease_ttl` before serving, so it no longer merely
  mirrors `DEFAULT_AGENT_LEASE_TTL` by hand.

- **The first spawn of a session now registers its own run, so the
  caps `relais hook` enforces are reachable at all.** Every session
  `relais run` had not already started found no run on record at its
  first spawn, and `request_admission` refuses an unregistered run
  outright — so the hook answered `UnknownRun` for every ordinary tab,
  regardless of any cap, and no cap had ever been observed refusing a
  spawn on the hook path. `hook::respond::ask_coordinator` now derives
  that session's own run id exactly as `admit` did, registers it with
  no budget, agent-cap or depth override, and retries the admission
  once — at most once per firing, since a hook cannot hold a tool call
  open. An unreachable coordinator is never registered against: that
  failure stays `CoordinatorAnswer::None`, resolved by
  `on_coordinator_unreachable` as before. Registering cannot resurrect
  or launder a run: the id is always the one the hook derives from the
  session in hand, never one a caller supplied.

  For that cap to count agents rather than spawns, the seat has to come
  back when the call ends, and it now does: a `PostToolUse` or
  `PostToolUseFailure` on the Agent tool carries the same `tool_use_id`
  its spawn did, so the dispatch id derives identically and the end
  settles and releases exactly what the start took — no pairing, nothing
  remembered between firings. Settled with `None` rather than a figure,
  because a hook payload carries no usage: that books the reservation as
  a lower bound and marks the run uncertain, where zero would claim the
  agent was free. Without this the seat would lapse only with the unbound
  dispatch's five-minute grace, so `max_active_agents_per_session` would
  cap spawns per rolling five minutes while the refusal advised retrying
  "when a running agent finishes" — advice that would free nothing.

  Three limits of this path are stated rather than implied away, in SPEC
  §23 and the README's known limits. **Depth is not enforced**: nothing
  in one payload joins a spawn to the dispatch it descends from, so every
  hook-admitted spawn is requested at depth 0. **No money limit is
  enforced**: the registered run carries no budget and the coordinator
  refuses on budget only for a run that has one, so
  `dispatch_reserve_micros` above zero changes what a spawn reserves
  against an unbounded total and refuses nothing. **A cancellation is not
  durable**: `relais cancel` on a session's derived run holds until the
  next reconcile reaps it — 15 seconds — after which the next spawn finds
  no record, registers the run again and is admitted, because the
  cancellation lived in coordinator memory and nothing on disk tells a
  reaped-cancelled run from a session never seen.

### Fixed

- **A run whose attempt started and was killed before reporting any
  usage no longer reads as `$0 (actual)`.** `run_cost_completeness`
  folded an empty set of usage rows to `Actual`, so a run interrupted
  mid-dispatch — no usage event ever arrived — looked identical to a
  run that genuinely spent nothing. The ledger now tells the two apart
  by the run's own attempts: no dispatched attempt at all is a true
  zero; a dispatched attempt with no usage anywhere is `unknown`; a
  run that reported some attempts and started a silent one is an
  `incomplete lower bound`, not unknown, since what was recorded is
  still real. The rule rolls up through a work-package tree the same
  way cost already does. `docs/SPEC.md` §11 needed no wording change —
  it already said missing usage is unknown, never zero.

- **A journal write can no longer interleave with a concurrent one.**
  `append_journal` rendered its record with `writeln!`, which goes
  through `Write::write_fmt` and issues one write per formatted
  fragment — every brace, key, colon and value of the record, dozens of
  syscalls for one line. `O_APPEND` makes each individual write atomic
  in where it lands, never a group of them, so two hook firings at the
  same instant interleaved their fragments and left lines in
  `hook_journal.jsonl` that were not JSON. Measured in a real session,
  not inferred: the two lines that failed to parse were exactly the two
  spawns whose timestamps collided. The record is now rendered into one
  buffer, newline included, and written with a single `write_all`;
  folding the newline into the `writeln!` would have fixed nothing.

## v0.3.0

### Added

- **Every run belongs to a task.** A task is the stable identity a
  dispatch, a re-run and a `--revise` all share — the ledger's task spine.
  A pre-spine run gets one `task-legacy-<root run id>` task backfilled
  onto it, so nothing on disk loses its cost attribution.
- **`--revise` lets a run correct a task's contract** instead of starting
  a disconnected one: the new run inherits the task's identity, and its
  cost joins the same cohort as the attempts before it.
- **Feedback is now typed and dataset-readable.** `relais feedback`
  records an outcome — accepted unchanged, corrected, reverted or
  confirmed regression — attributed to the task and the exact candidate,
  so `relais report` can tell a still-standing acceptance from one later
  withdrawn.
- **`relais report` adds the per-task figure.** Alongside the existing
  run-based numbers, the report now carries one line per task, an
  accepted-task and standing-task count, cost per accepted task, cost per
  standing change, a pending-feedback count and a backfilled-task count —
  the primary metric denominated in tasks, so a task retried to
  acceptance is not counted as several cheaper successes. The JSON
  payload gains a `schema_version` field.

## v0.2.4

### Fixed

- **The reviewer reads the patch, because the patch is in its prompt.**
  A reviewer is launched with an empty tool allowlist — it reports, it
  does not act — and the prompt told it to read a file. The first time
  a review mattered it spent its whole wall clock being refused and
  answered nothing, so a run whose checks had passed ended
  `needs_review` with the attempt already paid for. The diff now travels
  in the prompt, fenced as data like every other thing the runner did
  not write, bounded at 192 KiB with the truncation stated in the text
  the reviewer reads.
- **A review has its own wall clock.** It used to inherit whatever the
  worker left of the run's, with a one-second floor: a worker that used
  its twenty minutes left the reviewer a second to answer in. A review
  is part of acceptance, so it gets five minutes of its own.

## v0.2.3

### Fixed

- **A decomposed run's package worktrees are swept too.** They hang off
  the root run's artifacts, under `runs/<root>/packages/worktrees/`, a
  layout `resume --retire` and the doctor count walked past — 1.9 GB on
  the first sweep. The scan now walks the runs tree by shape, so a
  worktree at any depth is found, retired and counted.

## v0.2.2

### Fixed

- **A leftover worktree is retired from its own `.git` link, not from
  the directory the run was launched in.** The first `resume --retire`
  on a real machine kept four legacy worktrees with "git could not be
  launched": each had been launched from a task worktree removed weeks
  earlier, and retirement ran git there. Every repository-level command
  now runs inside the leftover itself, whose link names the repository's
  common dir whatever became of the launch path; `retire` no longer takes
  a repository directory at all.

## v0.2.1

The first days of running 0.2.0 from git worktrees, on a laptop, through
the reviewer. One change to how a grant is keyed: a trust grant is now
bound to the REPOSITORY — its `origin` URL, or its git common directory
— rather than to one checkout's path, so every `[trust."…"]` block
0.2.0 wrote changes key and must be re-issued from `relais plan` (0.2.0
hashed the checkout path into every grant, the origin-bound ones too).
`relais plan` prints the block, and prints which identity it is keyed on.

### Fixed

- **A run's status is a projection of its history.** `runs.status` was
  a column every writer had to remember to set, and one that drifted
  from the transition chain; it is now the `to_state` of the run's last
  transition, selected by one SQL expression every reader shares, with
  the column kept only as the fallback for a run that predates the
  chain. Every worker dispatch — initial, repair, escalation, a
  decomposed package's own — records a `running` transition
  (`worker_dispatched`), so a run reads `prepared -> running ->
  verifying -> accepted` instead of skipping the state it spends most
  of its life in.
- **A trust grant is bound to the repository, not the checkout.** The
  grant key hashed the canonical path of the directory `relais.toml`
  was found in, so a run started from a git worktree blocked on a grant
  the user had already issued from the live checkout, and each worktree
  asked for its own. The identity is now the `origin` remote URL when
  git reports one, else the canonical git common directory every
  worktree shares. `relais plan` prints `repository: <url>` or
  `repository: <common-dir> (no origin)` and `relais doctor`'s `trust`
  line names it the same way. Every grant 0.2.0 issued must be
  re-issued from `relais plan`; the serialized shape is pinned by a
  test so the next such change is a release note and not a surprise.
- **A run retains a named candidate, not a directory.** SPEC §8 listed
  a "retained worktree" among what a run delivers, and the runner kept
  the worktree of every run that did not accept: twelve runs left
  4.5 GB of `target/` and `node_modules/` under the state directory and
  a permanent `git worktree list` entry each. Every terminal state but
  `interrupted` now retires the worktree — the task one and a
  decomposed run's integration one alike: whatever the tree holds that
  no candidate of the run has named is snapshotted as
  `refs/relais/candidates/<run>/final` and exported to
  `candidate-final.patch` first, and only then is the directory removed,
  ignored build output included. The `worktree_retired` transition
  records the ref, the patch and the bytes reclaimed; a retirement that
  fails is `worktree_not_released` and never changes the run's outcome.
  An interrupted run keeps its worktree, because its tree may still be
  being written. `relais resume --retire` (or `--all --retire`) retires
  the worktrees older releases left behind, in both the
  `worktrees/<run>/…` and the legacy `runs/<run>/worktree` layout, once
  a run's dispatches are provably dead, and `relais resume <run>
  --retire` does so for one run, including one it has just reconciled.
  `relais doctor` counts what is still retained, with its size, and
  names the sweep.
- **A reviewer's findings under a Markdown heading are findings.** A
  review that listed three findings under `**Findings**` and closed with
  a "verified" section was read as having no verdict, and the run parked
  in `needs_review` with its findings unread. A findings block in any
  markup and any case is findings; `FINDINGS: none` on the last line is
  the one clean verdict; only an answer that declares nothing is
  unavailable. The reviewer is now told to end with exactly one of the
  two, and that it may only run read-only commands — the same run's
  reviewer had reported a denied `make check` as a finding.

## v0.2.0

A review of the whole crate against the fleet's decision records (2026-09-21)
found ninety defects; this release fixes them. Three are breaking: a trust
grant is now bound to the repository as well as the policy, so every
`[trust."…"]` block in `machine.toml` must be re-issued from `relais plan`;
the coordinator wire protocol changed, so a running coordinator must be
stopped (`relais coordinator stop`) before the first command of this
version; and the exit codes moved, so anything branching on relais's exit
status needs updating against [the table in
README.md](README.md#exit-codes) — `needs_decision` 2 → 8, `needs_review`
2 → 9, `budget_exhausted` 4 → 5, `interrupted` 4 → 6, `cancelled` 4 → 7,
unknown run 2 → 10, and "nothing to train on yet" 2 → 14.

### Added

- **A verification profile can declare the setup its commands need.**
  A verification worktree is a checkout of one revision and nothing
  else, so in a repository whose dependencies live in the tree (npm,
  pnpm, yarn, bun, uv, poetry, bundler, composer) the profile's commands
  found no `node_modules` or virtualenv there and could not run at all.
  `[[verification.profiles.<name>.setup]]` names the install step; it
  runs first, in every worktree the commands run in — the base, the
  task worktree, each candidate's copy — and stops at its first failure.
  It is executable authority like the commands: hashed into the trust
  grant, so declaring it asks for a new grant from `relais plan`, and
  never inferred — `relais doctor` and `relais plan` warn when a
  lockfile is at the root and a profile declares no setup, and relais
  never runs an installer that policy does not name. The setup's
  programs join the toolchain identity a cached baseline is keyed on, a
  cached baseline skips the base's setup but not the task worktree's,
  and every setup log is evidence in the ledger, never a passed check.
  A setup that does not succeed in a worktree the run owns ends the run
  `blocked (verification_setup_failed)` before a worker is launched; at
  a candidate it is a verification gap and the run ends
  `needs_decision`.
- **A baseline that cannot run is blocked before a worker is
  dispatched.** The first run on an npm repository spent two worker
  attempts against `sh: react-router: command not found` — every check
  exited 127 at the base and at each candidate alike — and ended
  `failed: same failure on an unchanged candidate`. A check that ends
  127 (the shell's "command not found"; a program relais itself cannot
  find is recorded the same way, in the log and in the outcome, instead
  of interrupting the run) is now an *unrunnable* check, not a failing
  one. At the base that is the absence of a verdict: the run ends
  `blocked (baseline_unrunnable)` with no worker launched, the base's
  logs kept as evidence, nothing written to the baseline cache, and the
  remedy named — the setup block to declare when a lockfile is present
  and none is, or, when a setup is declared and succeeded, the setup to
  look at rather than another install step. At a candidate, 127 stays
  the candidate's own failure, repaired like any other.

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

- **A decomposed run no longer spends its ceiling twice.** The reviewer of
  an assembled candidate was handed a spend of zero, so a run whose work
  packages had already used the whole `per_run` ceiling still bought a
  review on top of it — a 100-micro ceiling could spend 200. The review is
  now judged against what the run has actually spent, read from the ledger,
  and a run past its ceiling ends `needs_review` with the reviewer never
  dispatched.
- **A repair worker the harness refused says which tools to grant.**
  "The worker produced nothing" was measured against the BASE revision, so
  from the second attempt on the previous attempt's changes made a refused
  worker look productive: the run reported a recurring failure and bought a
  stronger model, instead of `blocked` with the missing permission named.
  It is now measured against the previous attempt's candidate.
- **A write lease the coordinator will not take back no longer strands an
  acceptable candidate.** Leases have no expiry, so one dropped socket call
  left the run's own finished dispatch on record as the worktree's writer
  and verification waited for it until the wall clock ran out — reporting a
  passing candidate as `interrupted`. The release is retried once, a
  failure is recorded as a `write_lease_not_released` transition, and
  verification never waits on a lease this run holds itself.
- **A coordinator that stops answering heartbeats ends the run instead of
  being ignored.** Cancellation reaches a worker only on the heartbeat, so
  a coordinator restart quietly made `relais cancel` a no-op for as long as
  the worker ran. Three consecutive unanswered heartbeats now end the run
  `interrupted` with reason `coordinator_unreachable`, evidence preserved.
- **The reviewer of an assembled candidate is sent to a patch that
  exists.** Its prompt named `candidate-latest.patch`, which only the
  single-worker path writes; a decomposed run exports
  `candidate-integrated.patch`. The patch to read is now part of the review
  request rather than a path assumed at the far end.
- **`verifying` appears in a run's history.** The state was assigned
  directly to the runner's own field, so the ledger never recorded entering
  it and the next row claimed to come from a state no row named.
- **An accepted decomposed run releases its integration worktree.** One
  permanent entry in `git worktree list` was left behind per accepted run.
  It is released under the same three conditions a task worktree is — the
  patch is exported, a ref names the revision, and the tree still holds
  exactly it — and kept, on the record, otherwise. A decomposed run that
  ended without integrating anything releases it too.
- **A receipt names every model the run paid for.** The planner's model was
  folded into `models_used` only when the assembled candidate happened to
  need a review, so a receipt could bill a model it did not name.
- **A review verdict is the reviewer's last word.** "findings: none"
  anywhere in the last five lines counted as a pass, so a reviewer quoting
  the phrase inside a finding was read as clean; and an answer with no
  verdict at all was reported as findings by default. The verdict is now
  the last non-empty line, and an answer that carries none ends the run
  `needs_review` saying so, never `accepted`.
- **An integration that cannot be read is an error, not an empty
  revision.** `git rev-parse HEAD` went unchecked after a fast-forward, so
  a failure produced `Ok("")` that travelled on as the integrated revision.
- **A work plan's own limits are bounded.** `packages x
  attempts_per_package` — both contract-supplied, and model output under
  `"decomposition": "propose"` — was an unchecked multiplication. The
  product saturates and each factor is now validated where the plan is.
- **A run that cannot record its evidence does not accept.** The review
  artifact was written best-effort and then hashed with the failure
  discarded, and check-log evidence rows were dropped silently. Writing an
  artifact and recording the evidence row that points at it are one
  fallible operation; a failure ends the run `needs_review` or
  `interrupted`, never `accepted` with evidence nobody can find.

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
  training records (14), no tier with enough coverage (15), a solver that
  diverged (16), and "no dataset has been built yet" is 14 too rather than
  the invalid-invocation 2 it used to share with a malformed command line
  — instead of a single stringly error. The solver's
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

- **Exit codes are one documented table, and the ones a caller must tell
  apart are different numbers.** `2` used to mean "a human must decide",
  "a human must review", "your contract is invalid" and "no such run" all
  at once; `4` covered a failed task, a spent budget and an interrupted
  run; `1` meant seven unrelated things and was documented nowhere. There
  is now a single exhaustive function over a `CliOutcome` enum, printed in
  `crates/relais/src/main.rs` and in README.md: `1` is relais's own
  machinery failing and says nothing about the task, `2` is an invalid
  invocation, `5` a spent ceiling, `6` an interrupted run, `8`/`9` the two
  kinds of "a human has to look", `10` an unknown run, `11` a resume
  refused because a worker may still be running and `12` a resume that
  reconciled one, and `13` a `--write` that could not carry out part of
  its plan. **Scripts that branch on relais's exit status need updating:**
  `needs_decision` moved from 2 to 8, `needs_review` from 2 to 9,
  `budget_exhausted` from 4 to 5, `interrupted` from 4 to 6 and
  `cancelled` from 4 to 7.
- **`install --claude --write` tells you when it could not apply
  something.** A file that changed between the preview and the write was
  skipped in silence, and the command printed `applied 0 change(s)` and
  exited 0 — the one case where a person needed to look. It now names
  every action it could not carry out and exits 13. The scope and the
  preview/apply decision are a value the module owns and tests, including
  a user-level install against an injected home directory.
- **Every owned file relais writes is replaced atomically.** An install or
  an update used to truncate the file and write over it, so a full disk or
  a kill in the middle destroyed the user's own text around the block and
  left a file relais would then refuse to touch for ever. Writes now go to
  a temporary sibling, are fsynced and renamed into place, so a failed
  write leaves the previous file exactly as it was. Uninstall re-reads and
  re-judges a block before removing it, instead of trusting a preview
  taken minutes earlier, and a dangling symlink is reported as somebody
  else's file rather than written through.
- **The installers refuse a download they cannot verify.** A missing
  `SHA256SUMS`, or a machine with no sha256 tool, printed a warning and
  installed the binary anyway — under a comment saying checksums are not
  optional. Both `install.sh` and `install.ps1` now stop, and
  `RELAIS_SKIP_CHECKSUM=1` is the explicit way to accept an unverified
  binary. `install.ps1` also fails when the archive held no `relais.exe`,
  instead of printing the "here is what to run next" epilogue having
  installed nothing.
- **`coordinator status --json` is a JSON document in every state.** With
  no daemon running it printed an English sentence on stdout, exited 0 and
  discarded the client's error, so a caller could not tell "nothing is
  running" from a parse failure. The output is now
  `{"coordinator":"absent","socket":…,"cause":…}`, `"serving"` with the
  snapshot, or `"version_skew"` with both protocol versions.
- **`relais doctor` no longer reports an unreadable ledger as a pass.** A
  ledger that opened but could not say which schema it carried printed
  `✓ ok` and exited 0. It is a `✗` now, with the error text. Finding
  levels are an enum rather than a string beside a `bool` that disagreed
  with it, so the mark a line prints and the exit code always agree — the
  JSON report drops the redundant `ok` field and keeps `level`.
- **`relais plan` blocks when it cannot read the working tree.** A failing
  `git status` was treated as a clean tree and the plan went ahead; `run`
  has blocked on exactly that since v0.1.3.
- **Resume's reconciliation is a tested function.** Eighty lines of policy
  in `main.rs` became `resume::reconcile`, pure and injected with the
  liveness probe, with a test per row: a live worker refuses, a recorded
  PID that is gone is provably dead, a dispatch with no PID recorded is
  unknown unless the coordinator still holds a seat for the run, and
  "unknown" is never folded together with "dead" or retried.
- **A deleted working directory is a diagnosed exit, not a panic.**
  Tearing down a task worktree under a running relais made
  `current_dir()` panic with a backtrace; it is a typed error now, as the
  unset-`HOME` case already was — and `paths::home_dir()` no longer exits
  the process from inside the library, so `doctor` can report it.
- **`make check` proves all four gates.** There is an `audit` target
  (`cargo audit`, installing the tool if it is missing, or skipped with
  `AUDIT_SKIP_OK=1`), `make msrv` now FAILS when the pinned toolchain is
  absent instead of exiting 0 on a skip (`MSRV_SKIP_OK=1` opts out), and
  CI's msrv job reads `rust-version` from `Cargo.toml` rather than
  restating it. CI's audit job runs `make audit`.
- **No release publishes untested code.** CI never runs on a tag, and the
  release workflow had no job that ran the suite — so a tag on an
  unmerged or amended commit built and published binaries nothing had
  tested. `publish` now needs a `test` job that runs `cargo test` on the
  tagged commit.
- **relais reads the fleet decision corpus it asks workers to consult.**
  `[integrations] aval = "required"` was satisfied by the binary being on
  PATH and nothing else: there was no `.adr.yaml`, no vendored pack and no
  `[[architecture.mapping]]`, so every run in this repository resolved
  zero decisions. The corpus is vendored at `.adr/packs/decisions.pack`,
  `relais.toml` maps `crates/**` to the code paradigm and the two code
  canons and the workflows to the CI and release decisions, and an
  `amont.conf` gate runs `aval check` at commit.

### Tests and documentation

- **`relais doctor` tells a harness that would not answer from one that is
  not installed.** A capability probe that failed — the binary gone
  mid-probe, a timeout, a non-zero exit, an empty answer — was reported as
  `claude --version did not answer`, the same line a missing CLI produced,
  and a launch blocked on it said no more. The probe now carries its
  failure, so `doctor` and a blocked launch both name which of the four it
  was and which flag was being asked about.
- **A check inventory that could not be read says why.** `amont list`
  failing to run, exiting non-zero, or printing something that is not an
  `amont-list-v1` envelope all became the same silent "no inventory"; the
  gate failed closed either way, but the gap line read
  `<check>: inventory unavailable` with nothing to act on. Each gap now
  carries the reason, and an inventory that could not be read is reported
  even when the profile names no check of its own — a run must never read
  "no inventory" as "nothing to enforce".
- **`relais plan` and the coordinator say when they fall back.** A learned
  registry that would not open silently disabled learned routing, an
  unreadable ledger let the coordinator start adopting no live dispatch,
  and `plan` printed no harness identity without saying whether the
  harness was absent or merely unresponsive. All three now print one line
  naming the cause before falling back.
- **A corrupt dispatch-intent row is a corrupt row.** `first_dispatch_intent`
  read a column that is not JSON as "there is no intent", so
  `relais dataset build` excluded the run for "no dispatch intent naming a
  model" and the real fault was never named.
- **Release scenarios that need no worker run on Windows too.** The
  integration suite was `#![cfg(unix)]` in one piece because the fake
  `claude` it drives is a `sh` script — so `init`, `install`/`uninstall`,
  `doctor`, a `plan` blocked on a missing trust grant, `coordinator
  status` with no daemon and the exit-code table were proved on one of the
  two platforms the release builds for, which is how the Windows PATH
  lookup shipped broken. Those six scenarios are now
  `crates/relais/tests/portable_scenarios.rs`, with no shell and no fake
  worker anywhere; the seven that drive a worker stay unix-only.
- **The architecture gate checks three properties, not two.**
  `scripts/check-module-cycles.py` already refused a module cycle and an
  impure `policy`; it now also refuses ambient state (`std::fs`,
  `std::process`, `std::env`) in `contract`, `route`, `money`, `ids`,
  `lifecycle` and `resume`, and refuses `libc::` or `windows_sys::`
  anywhere but `procs.rs` — tests included, since a test that spells an
  errno itself is a second copy of the table. `ipc`'s own `umask` guard
  and `EMFILE` table moved into `procs` behind `narrow_umask`,
  `peer_uid(&UnixStream)` and `out_of_descriptors`. `make lint` and CI run
  it, as before.
- **Property tests for the invariants examples cannot cover.**
  `rng::shuffle` is a permutation for any seed and length and is a
  function of the seed alone; `contract::scope::globs_overlap` is
  symmetric and reflexive and `**` overlaps everything; `MicroUsd`
  addition and subtraction saturate and never wrap and a remaining budget
  is never negative; `ids::canonical_json_hash` is invariant under key
  reordering at any nesting depth and separates different documents;
  every `lifecycle::State` and `Reason` round-trips through `as_str` and
  `parse` while an unknown spelling is refused; and `route::eligible_tiers`
  never offers a tier below the floor its kind implies.
- **The test suite no longer waits on a guess.** The signal-escalation
  test slept 200 ms twice and hoped the child's `TERM` trap was installed
  in between; the child now writes a readiness file and the test polls,
  bounded, for it and for each state after the signal. The candidate
  identity test slept 1.1 s to cross a clock second; it asserts the fixed
  author and committer stamp instead, which is the thing that actually
  makes identity independent of time. One `#[cfg(test)] test_support`
  module owns the temp-directory rule — unique per process AND per call,
  pre-cleaned — for the seventeen places that each wrote their own.
- **The session-id test cannot be weakened by the developer's shell.**
  `session_id` read `RELAIS_SESSION_ID` and `CLAUDE_SESSION_ID` from the
  ambient environment, so the test asserted the fallback only when
  neither happened to be set. The lookup is injected the way
  `paths::resolve_home` is, and every branch is asserted unconditionally.
- **`relais plan` names a blocker once.** `Blocked::explain` lists every
  blocker with its code on stdout, and `plan` then repeated the whole
  list on stderr — so a plan blocked by one thing reported it twice.
- **The review this release came out of is in the repository.**
  `docs/REVIEW-2026-09-21.md` carries every finding against `f231b7f`,
  with a status column naming the pull request that fixed each one or the
  reason it was kept, and a scorecard re-measured after v0.2.0.

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
