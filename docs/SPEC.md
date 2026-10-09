# Relais — coding-agent execution companion

Status: Complete target specification · revised 17 September 2026

One product release, including learned routing, context reuse, bounded decomposition and outcome feedback. Cold-start and calibrated operation are runtime states of the same product, not separate versions.

Relais is a working name; package and trademark availability have not been checked. Commands, schemas and integrations below are proposed interfaces, not existing features.

## 1. Purpose

Relais chooses a model and reasoning effort for a bounded coding task, supplies authoritative context, verifies the result, and escalates when necessary. Its objective is to reduce total cost per accepted change while preserving an explicit quality bar.

The user continues working in Claude Code. Relais provides an optional supervised execution path for tasks that benefit from controlled routing. It does not replace the coding harness or require an expensive model to supervise every tool call.

The core unit is a task with an immutable contract and a sequence of recorded attempts. The runner, not a model, owns transitions and acceptance.

## 2. Responsibilities

| Component | Owns | Relais consumes |
| --- | --- | --- |
| aval | Current architectural decisions, adopted rules and corpus validity | Verdicts, scoped decisions and applicable constraints |
| amont | Repository checks, declared-check trust and commit/push gates | Effective check inventory and actual check results |
| amont-agent | Tool-use guardrails and their observed behavior | Existing hooks; optional diagnostics for run reports |
| Claude Code | Model execution, tool use and its permission system | Supported programmatic execution and structured results |
| Relais | Task contracts, routing, context packages, attempts, verification and accounting | Evidence from all of the above |

Relais MUST NOT equate aval check with application compliance, an amont pass with semantic correctness, or a worker's completion message with acceptance. It MUST NOT change amont trust, skip settings or amont-agent stances to complete a task.

Relais supports one repository per run, with bounded independent work packages when decomposition is justified. It includes an owned training pipeline, a native Rust learned router, reusable context, controlled routing trials and backend adapters. It excludes unrestricted agent teams, automatic publishing, deployment, cross-repository writes, a transparent model proxy and general-purpose chat memory. The one exception to "no transparent model proxy" is the relais plugin's advisory session router (§30): inside an envelope the person grants once, it chooses the model of the person's own session requests, and it fails open.

## 3. User experience and integration modes

### Native assistance

`relais install --claude` proposes a small namespaced set of `.claude/agents/` advisory definitions and `.claude/skills/` skills, and installs the relais Claude Code plugin (`relais@relais-local`), which carries the mod, the agent definitions relais dispatches and the `/relais` skill: the plugin is embedded in the binary, written as a directory marketplace under the state directory, and registered and installed with `claude plugin marketplace add` and `claude plugin install` (`marketplace update` and `plugin update` when `claude plugin list --json` shows it installed). The plugin's version is relais's, so a release refreshes Claude Code's copy; `relais doctor` (`plugin`) warns when it is absent, disabled or at another version, and `relais uninstall --claude` runs `claude plugin uninstall` and `claude plugin marketplace remove` and removes the directory. Files an earlier relais wrote that the plugin replaced (the per-model worker agents and the `/relais` skill) are removed by install when unedited, and kept and reported when edited. Installation is preview-first; `--write` applies reviewed changes, merging configuration without replacing unrelated entries. User-level installation is explicit, not the default. Uninstall removes only owned, unchanged artifacts.

`relais install --claude --hooks` additionally wires the live hook (§23) into `.claude/settings.json`, on each of the seven targets named in the hook compatibility matrix below. `settings.json` is a file a person maintains by hand, so this is a separate, explicit ask, never implied by `--claude` alone: without `--hooks`, install reads and writes nothing under that name. Every write goes through a byte-for-byte round trip first — parse the file, re-render it with `serde_json` and compare — and a file that does not survive that trip is refused rather than rewritten: nothing is written, and the fragment to paste in by hand is printed instead, because editing a file that cannot be re-rendered losslessly would bury the one line relais wants to add inside a reformatting nobody asked for. The one tolerated difference is the trailing newline `serde_json`'s pretty printer does not emit and nearly every editor adds: a file whose only divergence is that newline is accepted, and is written back with the tail it arrived with, so the tolerance never becomes a change relais makes. Trailing blank lines, a `\r\n` tail or trailing spaces are refused like any other file relais cannot reproduce.

A handler somebody else already has on one of these events is joined rather than displaced when it sits on an entry whose matcher is the one relais installs; an entry on a different matcher gets its own, because a matcher states which tool calls a handler wants to see, and widening somebody else's is not relais's to do. Nothing at that granularity is ever deleted on uninstall except the leaf command relais itself added, because relais cannot tell an entry it left empty apart from one that was already empty when it arrived.

Native agent definitions provide convenient defaults for research, implementation and review. Native delegation remains the parent model's choice, except that the session router (§30), once its envelope is granted, may choose the model of a subagent the parent spawns — never of relais's own workers, and never of an agent the person pinned. Relais MUST label this mode advisory: it cannot guarantee a model, a dollar limit, an attempt limit, or acceptance for arbitrary work in the parent session.

### Supervised execution

The relais skill (`relais:relais`; the plugin's system-prompt line tells the parent to use it) has the parent express a bounded task as a task contract and invoke the Relais runner. The runner launches separate programmatic Claude Code sessions with explicit model, effort, tools and limits. It returns a compact report plus artifact paths.

The parent model routes a bounded task through relais on its own, writing the contract from the approved plan or the request rather than asking the person for its fields; `/relais:relais` invokes the skill by hand. A repository needs no setup before its first run. When a run reports `no_policy` or `missing_trust_grant`, the skill calls the plugin's `onboard` or `trust` tool. That tool asks the person in Claude Code's own dialog: whether to use the checks `relais init --detect` found (writing and committing `relais.toml`), and whether relais may run the listed commands (the grant, §5). It asks at most those two questions. Answering *Not now* writes nothing.

Once a task is accepted for execution, the parent does not supervise intermediate turns. The runner performs verification and bounded escalation. The parent receives the result and remaining decisions only.

Conversational examples:

- `Fix the JSON escaping defect in amont list output.` (routed without being asked)
- `Investigate why this check is inactive; do not edit files.`
- `/relais:relais Use Relais for this implementation; preserve the approved design.`

CLI examples:

```
relais doctor
relais init [--detect [--write] [--json] [--command TEXT]]
relais trust show
relais trust grant --key KEY --reviewed-by NAME
relais plan --task task.json
relais run --task task.json
relais status RUN_ID
relais explain RUN_ID
relais resume RUN_ID
relais report --since 2026-09-01
```

`plan` performs local preflight and explains the route without launching a model. `run` validates the contract and starts an execution. `resume` reconciles interrupted state; it never blindly repeats the last command.

Free-form prompting is a skill convenience. The runner accepts structured tasks; it does not invoke a classifier for every trivial request. The session router (§30) is the one exception, outside the runner: it makes one small classifier call per prompt the person types, and reports that call's cost.

## 4. Task contract

The contract is frozen and hashed before the first worker. Changing its objective, scope, acceptance criteria or budget creates a new contract revision and requires renewed verification.

```json
{
  "schema_version": 1,
  "kind": "change",
  "objective": "Preserve quotes and backslashes in amont list JSON output",
  "base_ref": "HEAD",
  "write_scope": ["crates/amont-runtime/**", "crates/amont/**"],
  "read_hints": ["crates/amont-runtime", "crates/amont"],
  "acceptance": [
    "Output parses as JSON and preserves original string values",
    "Existing fields and exit semantics remain unchanged",
    "The commit path acquires no external dependencies"
  ],
  "verification_profile": "rust-change",
  "architecture": {"keys": [], "scope": null},
  "risk_hints": ["public-output-contract"],
  "limits": {"attempts": 3, "wall_seconds": 1200},
  "review": "required"
}
```

Paths and profile names are illustrative and must be validated against the repository. `base_ref` resolves once to a commit SHA. Empty architecture keys mean no explicit mapping was supplied, not that architecture does not apply; repository mappings still run.

Required fields: version, kind, objective, acceptance, verification profile and base. A change also needs bounded write scope. Kinds are `inspect` and `change`. Unknown fields are rejected to catch misspelled controls. Inspection requires evidence criteria instead of a patch, and is judged by its report: on a base whose checks already fail it behaves as on a green one, and its bare, `llm_review` and unnamed-`test` criteria are settled by a read-only review of the report (§10). A `check`, a named `test`, a `human_sign_off` and an `amont_gate` criterion are settled as for a change.

An acceptance entry is either a bare string, judged by the verification profile as a whole, or a declared criterion naming the evidence that settles it. The two forms share one field, so a contract written entirely in bare strings is unchanged, byte for byte, and hashes as it always did.

```json
"acceptance": [
  "Existing fields and exit semantics remain unchanged",
  {
    "statement": "Output parses as JSON and preserves original string values",
    "id": "json-roundtrip",
    "mandatory": true,
    "evidence": {"kind": "check", "name": "make check"}
  },
  {
    "statement": "A regression test covers the escaping",
    "mandatory": false,
    "evidence": {"kind": "test", "authorship": "model_added"}
  }
]
```

Evidence is a named command in the verification profile (`check`), a test with its authorship recorded as pre-existing, human-added or model-added (`test`), a reviewing model's judgement (`llm_review`), or a person's explicit sign-off (`human_sign_off`). A criterion is mandatory unless it says otherwise, as a bare string always was. Its identity is the author's own id, or one derived from the statement's content, so reordering the list never renumbers a criterion. A criterion naming a check the profile does not define is refused before any dispatch: a criterion nothing can settle is not a narrower contract, it is a broken one.

A `test` criterion can also name the one test that settles it, which a verification command's exit status alone could never check:

```json
{
  "statement": "The api rejects malformed input",
  "evidence": {"kind": "test", "authorship": "pre_existing",
               "name": "tests.test_api::test_rejects_malformed_input"}
}
```

A command declares where its per-test results come from with `junit`, a path relative to the worktree it runs in, of a JUnit XML report it writes (`{ argv = ["make", "check"], junit = "target/nextest/ci/junit.xml" }` in `relais.toml`; INTEGRATIONS.md has the cargo-nextest, pytest and vitest recipes). relais clears that path before the command runs and reads it after, so a report the candidate committed is never mistaken for the run's. The test id is `classname::name`, or `name` when the report gives no classname. The criterion is met only when some command's report holds exactly that id as passed. A failed or skipped test is not met and refuses acceptance as a gap naming the outcome — a command can exit 0 over a skipped test — and the receipt records the test id, the command whose report held it, the outcome and the kept copy of that report. The copy sits beside the check logs and is recorded as a `junit_report` evidence row with its sha256; a report that was missing or unparseable leaves no copy. A test no report holds, or a profile in which no command declares `junit`, is a gap naming the test — as an unproduced check is — never a pass. A declared report that is missing or unparseable is recorded on the check outcome (`junit: missing`, `junit: unparseable (<reason>)`) and never changes the command's own pass or fail, which stays its exit status. A `test` criterion with no `name` settles as it always did, and `authorship` and independence are unchanged. relais's own policy declares no `junit` — its `make check` runs `cargo test` — so nothing changes here until a repository opts in.

Worker-supplied risk hints may increase caution but cannot lower repository risk floors. Acceptance text is checked for contradictions and missing prerequisites during preflight; unresolved requirements return `needs_decision`.

## 5. Configuration and authority

Repository policy lives in `relais.toml`; machine-owned settings hold spending ceilings, allowed providers/models, permissions and trust grants. Per-run options can narrow these grants. Effective authority is their intersection, never a last-writer-wins override that broadens permissions.

```toml
schema_version = 1

[models.research]
id = "haiku"

[models.implementation]
id = "sonnet"
effort = "medium"

[models.escalation]
id = "fable"
effort = "medium"

[execution]
max_attempts = 3
max_repairs_before_escalation = 1
max_wall_seconds = 1200
allow_nested_agents = true
max_agent_depth = 3
max_agents_total = 24

[integrations]
aval = "required"
amont = "required"
amont_agent = "required"

[[verification.profiles.rust-change.commands]]
argv = ["make", "check"]
timeout_seconds = 300

# A profile in a repository whose dependencies live in the tree declares
# the step that installs them; a verification worktree holds nothing else.
[[verification.profiles.web-change.setup]]
argv = ["npm", "ci"]

[[verification.profiles.web-change.commands]]
argv = ["npm", "test"]

[[risk]]
paths = ["**/trust/**", "**/restore/**"]
minimum_tier = "escalation"
review = "required"
```

These patterns are examples, not verified mappings of the three repositories. Explicit IDs are recommended for reproducible policies; aliases are permitted for convenience and the effective model must be recorded. A profile can omit effort for models without that capability.

### Effort is data

An effort is an identifier, not a member of a compiled list: `^[a-z][a-z0-9_-]{0,31}$`, checked when `relais.toml` is read, with a typed error naming the bad value. It serializes as the plain string it was written as, so the authority hash of a policy that already says `effort = "medium"` does not move. Nothing orders identifiers: no comparison on the type and no list of levels in the code. Which effort is above which comes only from the catalog below, and a new level (`ultra`) needs configuration, not a release.

The catalog is resolved per (harness version, model id) from three independent facts, each `known`, `unsupported` or `unknown`:

1. **CLI-accepted**, read from the harness `--help`: the list that follows `--effort` (it may wrap onto the next line). Membership only; the order the CLI prints is never used. `unsupported` when there is no `--effort` flag; `unknown` when the flag is there and no list can be read. There is no fallback set.
2. **Model support**, from machine.toml `[[efforts.models]] ids = [...], supported = [...]`. `supported = []` means the model has no effort control. A model with no entry is `unknown`.
3. **Order**, lowest first, from `[[efforts.models]] order = [...]` for that model, else machine-wide `[efforts] order = [...]`. `unknown` when neither is set; there is no built-in default.

```toml
[routing]
max_effort = "high"        # default; spend authority, never evidence

[efforts]
order = ["low", "medium", "high", "xhigh", "max"]

[[efforts.models]]
ids = ["claude-sonnet-5-5"]
supported = ["low", "medium", "high", "xhigh", "max"]
# order = [...]            # optional: this model's own order
```

Every key is optional, so an existing machine.toml parses unchanged. The admissible set is CLI-accepted ∩ model-supported ∩ authorized, in the configured order; authorized is `[routing] max_effort` read as a position in that order (that entry and every one below it). `next(e)` is the next admissible entry after `e` in the order, or none, stepping over gaps. `relais doctor` prints the admissible set for each model in the policy's tiers, or which fact is unknown or unsupported, and then the block to paste; `relais doctor --effort-template` prints just that block, with the CLI-accepted list pre-filled from `--help` and the model-support and order lines left as comments for the person to confirm. Routing reads the catalog (§6, "The rung"): a dispatch requests the effort the route's rung carries.

Repository commands, hooks and model launch configuration are executable authority. Relais requires a content-bound machine trust grant for a reviewed execution profile and relevant configuration. It delegates amont-specific trust to amont; it does not infer trust from repository ownership. Changed execution declarations invalidate the grant. Worker changes cannot update the frozen grant or commands during a run.

A grant is written by exactly one command, `relais trust grant --key <key> --reviewed-by <name>`, typed by a person in a shell or run by the relais plugin after the person answered the plugin's own question listing the exact commands (`$.ui.ask`, a dialog the model can neither word nor answer). `relais trust show` prints what a grant would authorize: the key, the repository identity, the resolved machine.toml (`$RELAIS_CONFIG_DIR`, else `~/.config/relais`), and every setup step and command with the models and integrations. `trust grant` recomputes the key from the policy on disk and refuses a different one (`stale_grant_key`, exit 18), takes an exclusive lock beside machine.toml, keeps the file's comments and mode (0600 when it creates it), and records the grant and the steps it authorized in the ledger. No run, recipe or worker ever issues a grant. A missing machine.toml is read as empty settings, so a fresh machine is told `missing_trust_grant` rather than failing to read a file. Outside the plugin's question, machine.toml is a plain file: the plugin denies the model's Write and Edit on it and flags a Bash command that names it or `relais trust grant`, but the protection there is Claude Code's permission prompt, and a session in bypass-permissions mode is outside this threat model.

A verification profile may declare a setup step: the commands that install the tree's own dependencies inside a verification worktree before the profile's commands run. It is executable authority like the commands (an installer runs the repository's lifecycle scripts), hashed into the grant, and never run unless a person confirmed it: relais may report that a lockfile is present and no setup is declared, and it never runs an installer that policy does not name. `relais init --detect` may propose setup steps and commands read from the repository's files, but a proposal is only a suggestion: what the person accepts is written to `relais.toml` and hashed into the grant, so nothing detected runs without that review.

Dependencies are required, optional or off. Missing required integration blocks execution. Optional gaps appear explicitly in the report; they are never reported as passed checks.

## 6. Routing algorithm

Eligibility, risk floors and acceptance are deterministic. Within the eligible arms (tier and effort), a Relais-trained predictor estimates acceptance and total cost using task outcomes collected by Relais. No upstream pretrained router weights or scores are loaded. The model supplies a proposed task, not its own exemption from policy.

1. Validate permissions, contract, base revision, verification profile and remaining limits.
2. Apply risk floors from declared scope, relevant repository rules and task kind. Unclassified writes use the conservative configured route, not Haiku.
3. Prefer an explicitly configured deterministic recipe when it fully covers the task. Relais does not infer arbitrary shell recipes from prose. A recipe is a versioned, hashed unit: each carries a `revision` and an `enabled` flag, and a content-derived `recipe_id`. When several recipes cover the task, the one selected is the highest `revision` among those that are `enabled` — a disabled recipe is never selected, even at the highest revision, and a recipe that does not cover the task is never selected whatever its revision. `relais.toml` refuses two recipes sharing `(name, revision)`, and refuses two recipes sharing a `recipe_id` (the same executable content declared twice). A learner may propose a retuned `relais.toml`; nothing runs it, writes it or grants it before `route::validate_candidate(incumbent, candidate, bounds)` accepts it. The boundary is DEFAULT-FIXED: a candidate may differ from the incumbent only in `recipes`, and within `recipes` a new revision may retune only `tier`, `enabled`, `models`, `execution`, `context` and `review` — never `name`, `kind` or `scope_within`, and never any other `RepoPolicy` field (`schema_version`, `models`, `execution`, `context`, `integrations`, `verification`, `risk`, `architecture`). History is append-only: a candidate's `recipes` must start with every recipe the incumbent already declared, in that order, and every appended entry must name a recipe the incumbent already has a family for, with a `revision` strictly past that family's highest, a `tier` at or above the floor re-derived from the candidate's own risk rules and the task kind (the same `kind_floor`/`risk_floor` a real task's floor uses), and a `review` no less cautious than the revision it tunes. The accepted form, `CandidateRecipe`, is a type only that function can mint — no other constructor exists. This package does not promote anything: `validate_candidate` returns a value; nothing writes a policy file, issues a trust grant, or enables a recipe.
4. Route mandatory high-risk work directly to its policy floor. For remaining tasks, extract pre-dispatch task features and select among the eligible ARMS using a validated Relais-trained model. An arm is a tier and an effort request (`route::Arm`, labelled `implementation@high`, `implementation:not_requested` or `implementation:control_unsupported`), so the learner chooses how hard a tier works, not only which tier. `route::learner_arms` enumerates them through the same rung resolution every route uses: for each eligible tier, each effort of its model's admissible set is tried as the initial rung's start, and kept only when the resolver accepts it and resolves to exactly that effort — so the risk floor, the authority ceiling, `allowed_models` and the unknown-catalog carve-out are the resolver's, and no effort arithmetic is restated. A tier whose admissible set is undetermined or empty contributes the one arm the resolver gives with no override; a tier whose rung is blocked contributes none. `select_learned` takes the cheapest priced arm that clears the quality floor, a tie going to the arm earlier in the list, and the ladder is then resolved with that arm's tier and, when it names an effort, that effort as the initial rung's start (repairs and the escalation derive from it as for any route). When trained evidence is absent or insufficient, use the configured conservative baseline and collect outcomes for training.
5. Emit the selected profile, rule IDs, reasons, verification requirements and maximum permitted attempts.

Which of these four steps actually decided (`RoutedBy`: `DeterministicRecipe`, `RiskFloor`, `LearnedArtifact` or `ConservativeBaseline`) is recorded on the dispatch itself (see §12), so a later reader can tell a route the learner actually chose from one the router only abstained to. A dispatch recorded before ledger migration v13 carries no such record at all — read back as absent, never as `ConservativeBaseline`, which is a positive claim about a decision that was never captured.

### The rung

The route carries the resolved initial dispatch, `Route.rung`: the tier (equal to `Route.tier`), the model id, and an effort request that is one of `Explicit(effort)`, `NotRequested` (the model has effort control and none is asked for) or `ControlUnsupported` (the model or the CLI has none). They are three states, never an `Option`. `route()` stays pure: the caller resolves an `EffortCatalog` (§5) for every model in play — the authority tiers' models and the covering recipe's — from the harness probe and machine.toml's `[efforts]` and `[routing] max_effort`, and hands them in as `RouteInputs::catalogs`; `relais plan`, the runner's preflight and the scheduler's planner read the same route.

- **Model.** A covering recipe's `models.<tier>` (model and start effort) is read when present, else the authority policy's `models.<tier>`. The model must be in the machine's `allowed_models`, or the route is `Blocked(ModelNotAllowed)` (exit 3) — never a fallback to another model.
- **Floor.** A `[[risk]]` rule may set `minimum_effort`. The floor of a route is the highest, in the catalog's order, of the touched rules' `minimum_effort`, and it binds the initial rung: the rung's effort is max(start effort, floor) in catalog order, snapped up to the lowest admissible effort at or above it. When the rung's model cannot satisfy it — its admissible set holds no effort at or above the floor, it is `ControlUnsupported`, or its order or support is unknown — the route is `Blocked` (exit 3), naming the floor, the model and the admissible set. The floor binds every rung the attempt budget reaches, not the initial one alone: the escalation rung is resolved the same way (see **Ladder**), so an escalation tier configured below the floor dispatches at the floor, and one that cannot carry it blocks the route.
- **Ceiling.** `authority_ceiling(tier)` is the AUTHORITY policy's `models.<tier>.max_effort` when set, else the top of the authority model's admissible set. It is resolved from the repository policy before any recipe or candidate effort is applied, so lowering or raising a recipe's start effort never moves it. The rung's effective ceiling is the smaller of `authority_ceiling(tier)` and the top of the dispatched model's admissible set. An effort above it is `Blocked(effort_above_cap)` (exit 3), never clamped. An effort inside the ceiling that the dispatched model's set does not hold is refused too.
- **Carve-out.** When the dispatched model's catalog is unknown (its order or support unconfigured, or the CLI list unparsed), a rung whose effort is exactly the authority policy's configured effort for that tier and model is allowed, as before P2. Any effort changed away from it — by a recipe or by a floor — needs a known catalog; otherwise the route is `Blocked` (exit 3) with the reason and the machine.toml keys to set (`[efforts] order`, `[[efforts.models]] supported`). A CLI with no `--effort` flag blocks a configured effort at route time as well as at launch.
- **Ladder.** `route()` resolves the rung of EVERY attempt the budget can reach, once, into `Route.ladder`, given the budget that actually applies: `max_attempts` and `max_repairs_before_escalation` for a run, and, for a decomposed package, the package's own budget (`attempts_per_package` narrows the package contract's attempts, and the package's route is resolved under it — never the root's `max_attempts`). An escalation is reachable when `max_attempts` is above `max_repairs_before_escalation` + 1. The rungs are: the initial rung (above); each repair, on the previous rung's tier and model, whose effort is the previous rung's stepped by the catalog's `next()` under `execution.repair_effort = "raise"` (the default) or unchanged under `"same"` — at the ceiling a repair stays at the ceiling, and `next()` finding nothing never becomes "no effort", while a `NotRequested` or `ControlUnsupported` rung stays so; and the escalation rung, the escalation tier at max(its configured effort, the floor), resolved like the initial rung (the covering recipe's `models[tier]` when it sets one, else the authority's). Every rung is validated at route time — admissible, at or above the floor, at or below its tier's effective ceiling, model allowed — and any failure is `Blocked` (exit 3) before attempt 1. An escalation tier whose model the machine's `allowed_models` removed is not a rung: the ladder ends before it, no other model is substituted, and nothing else is dispatched in its place.
- **Repair effort.** `repair_effort` is an `[execution]` key, `raise` or `same`, left out of the authority hash while it is the default. When a model's catalog is unknown (§5), `raise` resolves to `same` for that model: the configured effort is kept, nothing blocks, `relais plan` prints `repair effort held: effort order unknown for <model>` on stderr for the models THIS run's ladder holds, and `relais doctor`'s `effort` finding says `repair effort would be held (effort order unknown) for: <models>`: doctor has no task or budget, so it names every configured model with an effort a repair could run at, since a risk floor can make any tier the initial one. The receipt records the ladder that ran: the run's own for a single run, each package's own (by package id) for a decomposed run, whose root dispatches no worker rungs. A harness without `--effort` still blocks a configured effort, as above.
- **Dispatch.** The state machine (`decide`) chooses only the KIND of the next attempt (repair, escalation or stop) and the ladder index it moves to; the runner and the scheduler read tier, model and effort from `ladder[rung]` and nowhere else, and pass `--effort e` only for `Explicit(e)`. The run's first worker dispatch intent records the whole resolved ladder, and each attempt's intent records the rung index it ran with the model and effort exactly as dispatched; the receipt records the ladder too (an additive `ladder` field, absent on receipts written before it, which still parse). The reviewer's choice and profile are unchanged.
- **Output.** `relais plan` prints `effort: <model>@<effort>` (or `effort: <model>, no effort control`, or `effort: <model>, none requested`) directly after the unchanged `route:` line, with who set the effort when a recipe or a floor did, then the ladder one rung per line, each within 80 columns (`on failure: repair sonnet@high`, `then: escalation fable@medium`). `relais plan --json` prints the decision as one object carrying the rung and the ladder.

Candidates are held to the same sets. `validate_candidate` judges each recipe's per-tier effort against the admissible set routing uses for that model: an effort outside it, or above the authority ceiling, is `EffortNotAdmissible`; below the floor the recipe's own scope demands, `EffortBelowFloor`. A candidate may lower an effort. `TuningBounds` carries the per-model catalogs and per-tier ceilings its caller resolved, and a candidate may not change `max_effort`. A model whose catalog is unknown admits only the incumbent's configured effort. Admission shares the ladder with routing: `validate_candidate` resolves the very ladder `route()` resolves (one function, not a copy) for each new recipe, under the incumbent's models, budget and `repair_effort`, and admits the recipe only when every rung of it validates — so a candidate whose escalation rung a floor or a ceiling would block is refused (`LadderBlocked`) exactly when routing would block it. A candidate may not change `execution.repair_effort`.

Complexity and consequences are separate: a one-line authorization change may be simple and high-consequence. File count alone never determines risk. A non-sensitive task with uncertain entry points may need a bounded research phase, whose cost counts against the same run.

Example explanation:

```
route: implementation / sonnet / medium
reason: bounded change; existing output contract; executable acceptance checks
review: required by public-output-contract
on implementation failure: one repair, then escalation
on architecture conflict: needs_decision
```

No silent fallback is allowed. Unavailable models yield `blocked:model_unavailable`, or an explicitly pre-authorized alternative whose identity and cost are recorded. Provider-driven model substitution is detected where observable; an unapproved substitution stops further dispatch and invalidates any claim that the requested route was tested. A substitution the machine has reviewed in advance (`[routing].approved_substitutions` in `machine.toml`, naming the exact requested/effective pair) is accepted rather than refused — dispatch continues, and both identities are recorded, so the spend is attributed to the model that actually ran and the route's own request is not lost. This list is machine-owned only: accepting a costlier model is a spending decision a repository must not be able to widen on its own, the same reasoning that keeps `allowed_models` out of repository policy.

## 7. Context package

Relais assembles a manifest containing contract hash, base SHA, policy hash, tool versions, source paths and fingerprints, architecture evidence and verification plan.

The worker receives the objective, acceptance criteria, necessary constraints, a small set of entry points and exact failure evidence. Large files and logs are referenced by path and range and retrieved on demand. Existing applicable project instructions remain in force.

For aval, collect applicable active decisions and constraints using the established CLI or MCP interfaces. Path-to-decision mappings belong to Relais configuration; Relais does not assume aval already provides automatic source-code impact analysis. Retrieve full decision bodies only when needed. If required constraints exceed the configured context budget, return a sizing problem; never silently truncate them.

`contradiction` blocks affected work. `undecided`, `unknown` or `retired` require a decision only when the task depends on that answer; unrelated absent keys do not block every change. Corpus/tool failures remain distinct from verdicts.

Inherited context and harness overhead are measured separately where available. A small Relais prompt does not imply a small total request. A compact handoff preserves requirements, evidence and unresolved questions; it excludes the entire conversation and private reasoning traces.

## 8. Execution and workspace handling

Resolve the base commit and create an owned task worktree from that exact SHA. Relais requires the requested source changes to be committed first; it does not silently ignore or copy a dirty working tree. Other worktrees and the original checkout remain untouched.

Launch Claude Code as a child process, passing prompts through stdin or files and arguments as an argv array. Use explicit model, supported effort, structured output, turn limits and available budget controls. Permit bounded child-agent dispatch through the session adapter and shared admission controller; preserve parentage, inherited authority and aggregate budgets for all descendants. Load reviewed hooks, including amont-agent when required, without installing additional permissions implicitly.

The adapter capability-checks the installed Claude Code version and tests behavior against a documented compatibility matrix. It uses existing supported authentication and never extracts credentials. Missing permissions produce a `blocked` result; no permission-bypass flags are introduced. A refusal the harness reports is a missing permission and ends the attempt `blocked`; the old classification of a refusal's shape is gone with the sandbox that produced it (§8).

Write scope is checked on the actual diff after each attempt. A scope violation cannot be accepted and may require a revised contract. This is an acceptance boundary, not a claim of filesystem isolation. Worktrees share machine authority and Git metadata; tools with Bash access are not a security sandbox. Strong confinement requires a separately configured OS/container sandbox, an optional execution backend with explicit capability reporting.

A worker cannot commit, merge, push or publish through the normal allowed tool profile. The deny floor also lists `Agent` and `Task` (its legacy name): a subagent chooses its own model, and model choice belongs to the route. MEASURED 2026-09-28 (run-65c89ac482819-d958): in print mode the Agent tool needs no permission, and a sonnet worker handed its whole task to a background `general-purpose` subagent with `model: "opus"` — 124 Opus 5.5 turns against 14 of its own — which routed around the route and was noticed only after the spend. `execution.allow_nested_agents` is not a worker permission: it declares the agent-tree limits the coordinator enforces for hook-admitted sessions (§23), and a worker never spawns subagents whatever it says. Relais records an immutable candidate snapshot, including added files, outside model control. Sensitive repository configuration changes are rejected unless explicitly within an approved contract; they never take effect in the current worker launch.

### No OS sandbox

relais keeps no OS sandbox of its own. Every dispatch is a native agent of the person's Claude Code session and runs under that session's own sandbox and permissions: what confines a worker is what confines that session. relais still judges the actual diff against the contract's write scope, as above.

A native agent inherits the session's environment and permissions and runs with the tools its agent definition names, so relais carries no tool grant and no environment allowlist for a dispatch. `[permissions] allowed_tools` in `machine.toml` still parses and is no longer read; `relais doctor` says so. `[permissions] disallowed_tools` stays: it is part of the worker's written rules. A context manifest records no worker environment, `env_protection` `session`, and `session` as what confines each channel.

`[sandbox]` in machine.toml is no longer read. It still parses, with any keys, and `relais doctor` (and `doctor --json`) reports it in one line: `[sandbox] in machine.toml is no longer read: relais dispatches run as native agents under Claude Code's own sandbox`. `relais doctor --verify-sandbox` no longer exists and is a usage error. A ledger written under the old sandbox still reads: its `shape_refused` reason, its `sandbox_unavailable`, `sandbox_weakened` and `sandbox_unverified` block codes and its `sandbox_denials` evidence keep their spellings, though nothing writes them any more.

Every run delivers a named candidate — a ref under `refs/relais/candidates/<run>/` and the exported patch — with its base revision, and successful execution a verification receipt as well. The task worktree is released once everything tracked in it is exported: whatever no candidate of the run holds is named `…/final` and exported first, and only then does the directory go, ignored build output included; an interrupted run's worktree goes the same way at its end when every dispatch the ledger still holds live is provably dead, and is otherwise kept until `relais resume --retire` establishes that nothing may still be writing it. Integration into the user's branch is an explicit later action. Relais never force-cleans a worktree containing unexported changes.

## 9. Attempt lifecycle and escalation

States: `prepared`, `running`, `verifying`, `repairing`, `escalating`, `accepted`, `needs_review`, `needs_decision`, `blocked`, `failed`, `budget_exhausted`, `cancelled`, `interrupted`, `accepted_by_person`.

Every transition has a reason code, timestamp and evidence references. A worker can propose completion or blockage; the runner assigns every terminal state on its own, and a person's answer to `relais decide` can also assign one of them — `accepted` for `approve`, `cancelled` for every other ordinary answer, except `salvaged`, which assigns `accepted_by_person` — with what was answered recorded on the decision row, not the transition detail. `salvaged` answers a run whose candidate a person finished and merged, whichever way that run itself stopped: one that never awaited a person (`blocked`, `failed`, `budget_exhausted`, `cancelled`), where it both raises and resolves the decision row, or one still waiting on one (`needs_review`, `needs_decision`, `interrupted`), where it resolves the row already open rather than raising a second. `decided` resolves a question and says nothing about whether code shipped; `salvaged` is a person's acceptance of specific work, so the two are never interchangeable. `relais decide --answer salvaged --candidate <sha>` records that a person finished and merged the candidate such a run left behind; the run's own reason for stopping is kept on the decision row alongside the salvage's own resolution, never replaced by it, and the primary accepted-change metric (§11) counts `accepted_by_person` exactly like `accepted`.

An evidence reference names what kind of evidence it is, from a closed set rather than free text, and may name the tool that produced it, that tool's own identifier for it, the subject it attests to and the acceptance criterion it answers. Evidence produced outside a run is recorded the same way, and recording it decides nothing: it assigns no state, settles no criterion and clears no gap.

| Observation | Transition |
| --- | --- |
| Required checks and review pass on the same candidate | accepted |
| Behavioral failure with a localized correction | repairing, if repair allowance remains |
| Repair fails or diagnosis remains ambiguous | escalating, if authorized and budget permits |
| Same failure and unchanged candidate recur | escalate or fail immediately |
| Missing dependency/service/permission | blocked; do not buy a stronger model |
| Required architectural choice unresolved | needs_decision |
| Diff exceeds contract or changes protected verification | needs_decision |
| Required semantic review unavailable | needs_review |
| Cost, turn, attempt or wall limit reached | budget_exhausted |
| Process crashes or terminal result is missing | interrupted |

Default: at most three worker attempts total—initial, one repair, one stronger attempt. Starting on the strongest profile does not create another escalation tier. Optional review is one separate call per candidate requiring it, with its own bounded turn allowance and included total budget. Research calls also count toward total run resource ceilings. For decomposed work, this attempt limit applies per work package, and explicit aggregate limits bound the entire run; the scheduler cannot multiply the total budget by creating more packages.

Escalation uses a fresh context with the contract, candidate diff, exact verification failures and relevant evidence. Previous model claims are labelled unverified. Continue from a failed candidate only if its scope and integrity checks pass; otherwise preserve it for diagnosis and restart from the base. Escalation does not change acceptance criteria.

## 10. Verification and acceptance

Verification runs after all candidate-writing descendants have stopped or relinquished their write leases, against an immutable copy of the candidate. Record its identity, base SHA, contract hash, policy hash, commands, exit statuses, timeouts and log hashes. No concurrent worker may modify the verified candidate.

Preflight captures relevant baseline failures so pre-existing failures are visible. Existing failures are not automatically waived. A waiver must already be in policy or become an explicit contract revision. An inspection is the exception: it changes nothing, so a check that already fails at the base is recorded on the receipt (`baseline_failures`) and is not its to repair. Its candidate's failures exclude the base's, a gap about the profile's own checks (a missing or undefined check command, an amont gap) is recorded in `gaps_not_judged` instead of stopping it, and it is accepted when no gap remains and every failed check failed at the base. Acceptance-evidence gaps and a check that fails only on the candidate still stop it. Its worker's result text is its deliverable: an empty one is a failure (`empty_report`) the worker may repair once, as an empty candidate is for a change, and with a permission denial it is "produced nothing" and blocks. Whether a repair changed anything is judged by the tree, as for a change, so the same new failure twice ends the run. A sign-off-only inspection stores its pending receipt with the same `gaps_not_judged`.

An inspection's verification is one report review: a research-tier dispatch on the worker's own model, in the candidate worktree, with exactly the tools `Read`, `Grep` and `Glob` in both launch modes. Its prompt quotes the objective, the numbered criteria and the worker's report as data, adds the candidate's tracked tree taken from git by relais (the top-level entries, directories ending in `/`, and the first 400 tracked file paths, with a count of the rest; unavailable when git fails) because its tools cannot see directories, and asks it to check each claim it relies on against the files and to answer one line per criterion, `[n] met: <what you checked>` or `[n] not_met: <what you checked>`. It is independent of the contract's `review` field, which governs only the patch review (an inspection has no patch); the receipt notes a contract that says `review: off` in its `notes`, never among `gaps_not_judged`. Each report review is kept as `report-review-<attempt>.txt`, recorded against its attempt, and its usage carries the phase `report_review`, so it never counts as a patch review in the ledger or the report. Settling fails closed: a criterion with no line, an unparseable line or a duplicated number is not met, a number outside the criteria is ignored, and a review that cannot be dispatched ends the run `needs_review`. Each settled criterion records `settled_via` (`profile_checks`, `report_review`, `declared_evidence` or `human_sign_off`) beside its declared evidence, and the receipt records its `kind`; a criterion settled by the report review is not independent evidence, and a receipt that records neither is read as it always was. The first of these decides an inspection's end: a `check` or named-`test` criterion that is not met fails the run (`criteria_unmet`, naming the criterion and the check) with no repair, even when a report criterion is also unmet; a report criterion not met is repaired once, on the same tier and never escalated, by a repair that names the unmet criteria and the reviewer's reasons and never a base check failure, then fails (`criteria_unmet`); every criterion met except human sign-offs is `needs_decision`, listing only the unrecorded sign-offs, and `relais decide --answer approve --criterion` re-seals the receipt as `human_sign_off` and accepts an inspection when every mandatory criterion is met by its recorded settlement; otherwise it is accepted.

A declared setup runs in every worktree the profile's commands run in — the base, the task worktree, each candidate's copy — before them, and stops at its first failure. Its logs are evidence, never checks: a setup that succeeded passed nothing. A setup that does not succeed in a worktree the run owns blocks the run before a worker is launched; at a candidate it is a verification gap, since no check ran. A baseline check that ran to no verdict — a program not found, or one the wall clock or a signal cut off before it reported a status — is `BaselineUnrunnable`, distinct from a baseline check that ran and failed: the run is blocked before any dispatch, the base's logs are kept, nothing is cached, and the block names the check, how it ended and, for a wall-clock or signal ending, the limit it ran against — pointing at contention or too low a limit, not at a base that is broken — or, for a missing program, the remedy: the setup to declare, or, when one is declared and succeeded, the setup to look at rather than another install step. A candidate check that runs to no verdict this same way is still, unlike at the base, a failure: there is no base to compare it against, and a hanging or killed candidate check is a candidate that failed.

Use amont's effective inventory to identify checks and gaps. Run configured acceptance checks through their documented interfaces; do not assume amont check represents every pre-push gate. A skipped, inert, unavailable or untrusted required check is a gap, not a pass. Relais neither forges nor reuses amont attestations on another tree.

Acceptance requires all mandatory criteria to have evidence. Existing tests cover existing behavior; requested behavior may need new regression tests or explicit human evaluation. Tests added by the implementing model are useful but are not independent ground truth. Changes to required checks, fixtures or acceptance tests receive explicit review and cannot silently weaken the contract.

A mandatory criterion whose declared evidence produced nothing is a gap in this same report, refused by the mechanism that already refuses gaps — never a second acceptance path. A criterion settled only by a model-added test or an LLM review is still accepted once the checks pass; the receipt records that its evidence was not independent rather than imposing a stricter rule than the contract asked for. A criterion asking for a human sign-off is met only by a recorded sign-off: reading its answer off the passing checks would report a sign-off nobody gave. That sign-off is recorded by nothing but `relais decide --answer approve --criterion <id>`, naming the criterion by the id the contract declares (or derives); every other declared evidence kind is answered by the run itself, this one only by a person. A run stopped by a human-sign-off gap alone still reaches `needs_decision` without a reviewer ever being launched for it. The receipt carries, per criterion, whether it was met and by which evidence, and one summary of how independent the mandatory criteria's evidence was; `relais decide` re-seals that same receipt once a sign-off it names clears the gap.

A declared criterion can also name an amont gate as its evidence, alongside a named check, test, review or human sign-off. Whether the gate is covered is asked of amont through its own documented interface — `amont attest covered`, run against the candidate's own tree — never by reading `refs/notes/amont-attest`, parsing an attestation format, or verifying a signature here: the gate and its attestation stay amont's to own (§18). A gate amont reports covered settles the criterion met and independent — it ran outside this run, and a valid signed attestation is amont's own verdict, not a claim this run makes about itself — and records an external-attestation evidence row against the run and the criterion it answers, naming amont as the tool and the candidate SHA as the subject. amont's own interface is fail-open by design: it prints nothing and exits 0 whether an attestation is absent or one whose signature failed to verify, so a gate it does not report as covered becomes a gap through the same mechanism that already refuses gaps, never a silent pass — its message says it cannot tell which of the two amont meant, and names `amont attest covered` as the command a person can run to see the same answer. amont being absent, too old to have the subcommand, or failing for any other reason is likewise a gap naming the cause relais actually observed, never a silent pass and never a crash.

Semantic review is risk-dependent. A separate reviewer gets the contract, candidate, pertinent source and architecture evidence. It reports concrete findings with file/range, violated criterion, evidence and suggested verification; it cannot edit or waive checks. Findings are triaged by evidence. Lack of findings is not mathematical proof, and reviewer disagreement returns `needs_review` when it cannot be resolved within limits.

An accepted receipt means the declared acceptance policy passed on that candidate. It does not authorize merge, certify all correctness, or remain valid after edits. Every changed candidate requires fresh applicable verification.

## 11. Cost and resource accounting

Record requested and effective model, effort, harness version, token categories where provided, provider-reported cost, estimated cost, pricing-version reference, duration, attempt, phase and final outcome. Mark missing usage `unknown`; never replace it with zero.

Costs include preparation performed by a model, research, implementation, repairs, escalation and review. Outer interactive-session overhead is unknown unless correlated from its transcript; reports must label that boundary. Native assistance cannot be compared as if its parent session were free.

Provider-reported totals and child usage are alternative aggregation sources: do not add an inclusive parent total to its children. Deduplicate stable request/event IDs. Persist accounting incrementally and reconcile terminal totals after completion. An interrupted run may have an incomplete lower-bound cost.

A native worker's cost (§23) is estimated from the usage the plugin reports, at the machine's `[pricing.models]` rates; a model with no entry has an unknown cost, and an unknown cost settles as the attempt's whole reservation, which is the run's budget, so the next attempt would be refused `budget_exceeded` for a spend that never happened. A run therefore never goes on to burn its budget on a model relais cannot price. Up front, before anything is dispatched, it checks every model in `[models.*]`: priced when an entry matches the alias as written or the effective model the ledger last observed for it (the pair with the latest `last_seen`); refused `native_unpriced` when the latest observed effective model has no entry, even if an older one had, or when there is no price table at all; a never-observed alias that is not priced as written goes on with a warning that the runtime check applies. The refusal names each alias, the id it runs as and `machine.toml`. As a backstop, when a native attempt books a record whose model has no price, that attempt is booked and judged as usual (an accepted attempt stays accepted) and the run then ends `blocked:native_unpriced` naming the model instead of requesting admission for another attempt.

Use an integer or decimal money representation, not floating-point accumulation. Distinguish API spend, usage-credit spend, estimated API-equivalent cost and subscription consumption. Do not convert token savings directly into subscription fee savings.

The runner enforces dispatch, attempt, turn and wall-time ceilings. API dollar controls are best effort across in-flight requests and delayed reporting; reserve a margin before dispatch and stop admitting work once exhausted. Report overshoot and unknown usage. Do not advertise an exact financial cap where the provider cannot guarantee it.

Primary outcome metric: all recorded cost, including failed runs, divided by accepted tasks in the same cohort. Also report acceptance rate, review corrections, escalation rate, duration and later user-reported regressions. Compare like task classes and policy versions.

`relais report --by recipe` adds a recipe dimension through the same cohort machinery as `task-class`, `tier`, `model`, `policy` and `repository`. A task's cohort is the recipe that routed its root run's first worker dispatch, read from the dispatch intent (`recipe_name`, `recipe_revision`, `recipe_id`) and the `routed_by` column, labelled `<name>@<revision> [<first 12 characters of the recipe id>]`. A task routed with no recipe is its own cohort named after its `routed_by` (`no recipe (conservative_baseline)`); a task with nothing recorded is `unrecorded` and is never folded into another cohort. Runs whose `runs.purpose` is `replay` or `trial_arm` are not ordinary work: a replay is left out of its source task's cohort figures, and a live trial arm's task is its own cohort, labelled `(trial arm)`. Their spend is printed on every `--by recipe` render as `trial spend: replay $X (n runs), live $Y (m runs)`, each figure with its completeness label, and is never netted out of or into any cohort. Every `--by recipe` render also states, once, that the figures are observational — cohorts differ in which tasks they received, so they are not a comparison between recipes, and a comparison is what `relais recipe evaluate` produces (§17). The JSON carries the same as `observational` and `trial_spend`.

`relais report --by effort` groups tasks by the effort their first run's first worker attempt REQUESTED. Every effort figure — this dimension, the repair section below and the receipt's `efforts_used` — is attributed through one ledger reader, `Ledger::requested_effort(attempt)`, read from the attempt's worker dispatch intent (the dispatch whose `attempt_id` is the attempt, `kind` not `review`) in this order: the intent's `ladder[rung].effort` when the intent carries a ladder and a rung (`{"explicit":"<id>"}` is the id itself, `"not_requested"` is `none requested`, `"control_unsupported"` is `no effort control`); else the intent's flat `effort`; else the attempt's first non-review `usage_events.requested_effort`; else `unknown`. The label is whatever the ledger recorded, so an effort id the code has never heard of forms its own cohort, and a task with no recorded effort (or no worker attempt) is `unknown`, never dropped.

Every report, `--by` or not, carries a repair-outcome section (`repair_outcomes` in the JSON, report schema 10): one entry per requested effort over every attempt with `phase = 'repair'` whose run is in the window, or is a package run beneath one (the window lists root runs; each package run is walked and counted with its own status). Each entry gives the number of repairs; the repair's IMMEDIATE verification outcome — passed, failed, or no verdict with the no-verdict kinds counted (`blocked`, `crash`, `cancelled`, a `needs_decision` reason, the reason of a stop before the checks ran, `in flight`, `unrecorded`); its observed cost (mean and range over the repairs that have a usage row, and the number with none counted as unknown — an attempt with no usage row is unknown cost, never `$0.00`); and, on a separate line, the eventual statuses of the runs those repairs belong to, each run counted once per effort. The immediate outcome is read from `transitions`, whose `attempt_id` is NULL in practice: walking a run's transitions in id order, the k-th transition into `repairing` opens the run's k-th repair attempt, and that repair's outcome is the first later transition into any state but `running` or `verifying`. `repairing`, `escalating`, `failed` and `budget_exhausted` entered from `verifying` are a failed verification, and entered from `running` (a limit reached while dispatching, an unapproved substitution: the checks never ran) are no verdict named by the transition's reason; `accepted`, `needs_review` and `accepted_by_person` a passed one (the runner reaches `needs_review` and `accepted` from `verifying` only after the checks passed with no gap, so review is not verification); `blocked`, `interrupted`, `cancelled` and `needs_decision` are no verdict (`blocked`, `crash`, `cancelled`, the decision's reason). A repair with no such transition yet is `in flight`. A repair's own verification is never confused with how its run ended: a repair that passed inside a run a reviewer later stopped is a passed repair in a `needs_review` run.

A task accepted because a person answered `relais decide --answer approve` counts in that denominator exactly like one relais accepted on its own checks and review — work a person took is work that landed, and excluding it measures everything except the cases a person cared enough to judge. The two are reported as separate counts beside the metric, never as one number that mixes them, because how much of the denominator relais itself vouched for is the question the metric is asked to answer.

A task whose run stopped without relais's own acceptance — terminal on its own, or still waiting on a person — but whose candidate a person finished and merged anyway (`relais decide --answer salvaged`, §9), also counts in the denominator — the cost was already spent and the work already landed; excluding it understates the metric by exactly the work relais discarded or stalled on and a person rescued. `salvaged` is the only answer that counts here: `decided` resolves the question a run raised and says nothing about whether code shipped, so a task answered `decided` is not in the denominator, whereas `salvaged` is a person's acceptance of specific work — the candidate it names.

A task whose accepted candidate a person says was never shipped (`relais feedback <run> --outcome not-shipped`, §20) leaves both `accepted_tasks` and `standing_tasks` — it was abandoned for reasons unrelated to its quality, so it is neither an accepted change nor a failure — but its cost stays in the numerator, because the money was spent. It is not a regression and is not treated as `reverted`. `relais report` prints how many accepted tasks were marked never-shipped beside the feedback-coverage label on the primary metric's line, so the exclusion is visible rather than silent.

The "outer interactive-session overhead" above is not left unknown by construction: `relais usage import` (and `relais report`, which imports before rendering unless `--no-import` is given) reads the orchestrating Claude Code session's own transcript — `~/.claude/projects/<slug>/<session>.jsonl`, plus every subagent file beside it — and prices its usage from a machine-owned `[pricing]` table (`~/.config/relais/machine.toml`), never from constants in the code. A session is imported when it is the `root_session` of at least one run on record; a relais worker's own session is never imported this way, because it is already paid for as a usage event of its run. Import is idempotent, keyed by the transcript's own `message.id`. A native worker (§23) is a subagent of that session, so its messages are in a subagent file import reads: they are booked to their run, once, in either order. When a native attempt books its usage the runner records the message ids of the agent's transcript (`native_usage_messages`, ledger step v18; §23) in the same transaction as the usage event, and deletes any `orchestration_usage` row with one of those ids, which an import that ran first made; an import skips a record whose id is recorded there, and skips the whole `subagents/agent-<id>.jsonl` file of an agent the ledger records as a relais dispatch's (`dispatches.agent_id`), so a dispatch whose transcript ids could not be recorded (`rollback_ids_missing`) is still never booked twice; its summary line says how many records it skipped as already booked to a relais run. `relais report` prints this orchestration total beside worker spend, on every render, and two cost-per-accepted-task figures: the existing one, which leaves orchestration out and so under-states cost, and one with orchestration folded in, labelled an upper bound — one orchestrating session can do work unrelated to relais runs too. Orchestration spend is never netted against worker spend, hidden behind a flag, or folded silently into the worker figure. A session recorded before the session-identity fix (`root_session` a bare shell PID) can never have a transcript and is reported `unattributable`; a session that should have one but none was found at the searched path is `transcript missing` — both are counted, never guessed at.

### Alias drift

A requested model alias (`sonnet`) can start running a different effective model without anything in relais changing — a Claude Code release moves the alias — and every figure straddling that move mixes two models. Detection reads the `usage_events` rows that carry both `requested_model` and `model`, grouped by the pair with the first and last `at` and a count, and applies one rule per alias (`learn::drift`, pure): model B **supersedes** the alias's current model A when B's first event is after A's last event and B has at least two events. A model whose events overlap A's span, or that has a single event, is an anomaly — for example a worker spawning a subagent on another model, an unapproved substitution — and produces no switch. Successive switches chain: A→B→C is two switches, never A→C.

It surfaces in two places. `relais doctor` prints a `models` warning per switch (`alias sonnet now runs … (since …); before: … (until …)`) or `no alias drift`, and a `pricing` warning once per model seen in `orchestration_usage` in the last 30 days that `[pricing]` has no entry for (only orchestration usage is priced from that table; worker usage carries its provider-reported cost) (one line saying pricing is unconfigured when there is no table; it never suggests a price). `relais recipe evaluate` adds a `caveat: alias sonnet ran X in incumbent runs and Y in candidate runs` line, and a `model_caveats` JSON field, when the runs it compares — each replay pair's source and replay run, every live control and candidate-arm run — used different effective models under the same alias. A run's models are read over its whole tree, the root and every child run. A trial row records the run its arm executed as (`trials.arm_run_id`, ledger step v17: the replay's own run, or a live row's own run); a row written before v17 has none, contributes nothing, and is counted on one line, `model caveats could not check N trial(s) recorded before arm runs were stored`. It is a caveat, not a gate: `failures()` and `promotable()` do not read it.

## 12. Persistence and crash recovery

Use a machine-local data directory with a SQLite ledger and an artifact directory per run. Suggested records: tasks, contract revisions, attempts, transitions, evidence, usage events and outcomes. Store payload schemas with explicit versions and support additive migrations.

An evidence row's kind is a typed value, not free text — a stored name the reading binary does not know is a corrupt row, never silently dropped or coerced. A row may also name where it came from (the tool that produced it and that tool's own identifier for it) and what it is about (the subject it attests to and, when it answers one, the acceptance criterion). All four are optional. `relais evidence attach` records one such row for evidence a tool outside relais produced — against a run and, optionally, the criterion it answers — and decides nothing about it: recording is not deciding, so it never marks a criterion met, changes a run's state, or clears a gap. It refuses a run it does not know, and a criterion id the run's contract does not declare, naming the ids it does.

Artifacts include context manifest, worker results, candidate patch, check logs and receipt. Raw transcripts are opt-in; avoid retaining credentials or private reasoning. Redaction and retention policies apply to artifacts as well as logs. Relais sends no product telemetry.

Persist dispatch intent before spawning a process, then its PID/session identifier. On restart reconcile liveness, terminal output and artifacts before scheduling another attempt. Never assume an absent terminal result means nothing executed. Mark uncertain state `interrupted`, preserve changes, and avoid retrying commands with unknown side effects.

Dispatch intent also records which of the router's four strategies actually decided the route (`routed_by`, §6), a nullable column added by ledger migration v13. A dispatch recorded before v13 reads that column back as absent, not as the conservative baseline — "nobody recorded how this was routed" and "the router ran and abstained to the baseline" are different facts, and only one of them was ever true of a pre-v13 row. A stored value this binary does not recognise is a corrupt row, reported as one, never silently read as the baseline either. Dispatch intent also names the recipe that covered the task, if any — id, name and revision — beside the model, effort, harness, tier and kind it already carries.

A receipt includes run ID, candidate identity, contract/policy hashes, outcome, verification evidence, actual models (`models_used`), attempt count and cost completeness. It also records `efforts_used`, the distinct efforts the run's worker attempts REQUESTED in first-use order (§11, through the same reader as `relais report --by effort`); the field is additive — absent from a receipt written before it existed, which still parses, and left out when empty. It also names the recipe that covered the run — id, name and revision, together, or their absence when no configured recipe covered it — and binds `verification_profile_hash`, the hash of the verification PROFILE that actually judged the candidate. `policy_hash` moves whenever anything in the policy moves, including a recipe promotion that touches no check, so it cannot answer "was this judged by the same checks"; `verification_profile_hash` does, because it hashes only that one profile's setup, commands, amont checks/waivers, inputs and cache setting. Both fields default so a receipt written before they existed still parses; a receipt written after hashes differently from one written before, because it now records more. A returned worker JSON document cannot impersonate a runner receipt.

A controlled comparison (§17) has somewhere to live: ledger migration v14 adds a `trials` table, a nullable `runs.purpose` and a nullable `dispatches.trial_id`. A `trials` row records ONE ARM of one comparison — which task and source run it derives from, the incumbent and arm recipe ids, the arm's index and the assignment probability it was drawn with, the seed, the base sha, the contract hash, the verification profile hash and how the workspace was isolated — and, once settled, its outcome, whether it was accepted without escalation, its cost, its cost completeness and its duration. A cost of `NULL` means unknown, never zero: a trial whose usage was never reported does not read as free. `runs.purpose` is a typed value distinguishing ordinary work from a replay and a live trial arm; `NULL` means ordinary work, the overwhelming majority of runs and every run that predates this column, and is never confused with either recorded purpose. Reading a stored row with an outcome, cost completeness or purpose this binary does not recognise is a corrupt row, reported as one, never silently normalised. **Nothing assigns a trial yet**: no routing decision consults the `trials` table, no run is given a purpose, and no dispatch is given a trial id by any code path in this release — this is storage and typed readers only, and the table is written by nothing a worker or a model can reach. Assignment is a later package.

## 13. Implementation outline

Implementation language: Rust throughout the CLI, dataset construction, training, evaluation, inference and model registry, consistent with amont and aval. Use serde for validated boundaries and SQLite for the ledger. A zero-dependency constraint is unnecessary because Relais is not on the commit-hook path.

Modules: contract, policy, route, context, workspace, claude_adapter, coordinator, admission, verify, ledger, report, install. Keep the routing engine a pure function over validated inputs. Keep transport-specific event parsing in the adapter. Invoke aval and amont through versioned public interfaces rather than sharing private implementation code.

Ship the CLI, Claude skill, native routing module, dataset builder, trainer, evaluator, model registry, context index, bounded scheduler and evidence reporting together. Native agent templates derive from the same profiles but remain advisory. A backend interface supports additional execution providers without requiring every possible provider to ship; the Claude Code adapter is mandatory.

Telemetry feeds the owned dataset, training pipeline and controlled trial policy. Relais can automatically promote or roll back calibrated routes within a previously authorized experimentation envelope. Changes outside that envelope require an explicit policy edit. Risk floors and required checks never become trainable parameters.

## 14. Release acceptance

The product is usable when these scenarios pass:

- A bounded change routes to the configured model, passes required verification and produces a receipt bound to the candidate.
- A failed implementation gets at most the configured repair and escalation attempts; all costs remain attributed to one task.
- An unresolved architectural dependency stops execution without inventing a decision.
- Missing required integrations, unavailable models and missing permissions produce explicit blocked outcomes.
- A worker cannot obtain acceptance by reporting success, omitting a failed check, modifying policy, or changing files after verification.
- Budget exhaustion preserves the patch and evidence and prevents further dispatch.
- Interrupted execution resumes without duplicate live workers or blind command replay.
- Installation and uninstall preserve unrelated configuration and modified owned files.
- Reports distinguish actual, estimated, incomplete and unobserved cost; inclusive totals are not double-counted.
- Dirty source worktrees, out-of-scope edits and existing test failures are handled explicitly.

These are product acceptance scenarios, not a requirement to assemble thirty synthetic coding tasks before building the companion. A few real tasks can establish the initial baseline; broader evaluation follows actual routing decisions.

## 15. Source and compatibility notes

The product design above is proposed. Existing companion roles are grounded in the repositories inspected during this discussion: amont, amont-agent, and aval.

Claude Code currently documents model and effort fields for subagents, with an invocation-level model taking precedence over frontmatter. This is why native definitions are defaults rather than a complete spending policy. See subagents.

Claude Code also offers a programmatic mode (`claude -p`, structured JSON results). relais does not use it: every dispatch is a native agent (§23), and the only `claude` processes relais starts are `claude --version` and `claude --help`, to learn which harness is installed and what it accepts.

Model, effort, turn limits and an API dollar-budget flag are documented CLI capabilities. Their availability and exact behavior must be capability-checked; they do not establish an exact subscription or in-flight financial cap. See CLI reference.

Quality-constrained cost measurement follows the approach in Anthropic's cost-optimization cookbook. Its example savings are not forecasts for Relais.

## 16. Native Rust learning, informed by RouteLLM

Relais MUST NOT load RouteLLM's pretrained router checkpoints, derive training labels from their scores, or use those scores as routing features. It owns its training data, feature pipeline, labels, learning objective and model artifacts.

RouteLLM is a research and design reference, not a dependency or compatibility target. Learn from its separation of scoring, selection thresholds and evaluation, while defining native Rust interfaces around Relais task outcomes and complete execution profiles. Do not copy its stock preference objective or interpret a strong-model call percentage as a coding-quality guarantee. No Python process, RouteLLM package, OpenAI-compatible routing proxy or external embedding service is required.

The runner selects a profile at a task boundary and launches the existing harness. Training and inference use the same Rust feature extraction and prediction code. Artifacts store a versioned feature schema, coefficient arrays, normalization data and any validated calibration parameters. Loading rejects incompatible schemas and non-finite values. No cross-language model export is needed. The external coding harness remains its own dependency; Rust-only refers to the Relais application and its learning implementation.

The initial owned learner is regularized logistic regression for acceptance without escalation, with a separate simple cost estimator for complete-strategy cost. This is an implementation choice within the complete product, not a staged product version. Matrix factorization or a larger learner may replace it only if evaluation establishes a benefit. The chosen learner does not reproduce RouteLLM's matrix-factorization architecture.

The initial feature extractor is deterministic: task category, language, scope breadth, existing implementation examples, relevant risk indicators, verification availability, unresolved decision dependencies, and the recipe identity of what produced the run — model/effort/harness alongside the `recipe_id` of the deterministic recipe that covered the task, when one did. Optional hashed task-text features use the same frozen tokenizer and hashing configuration at train and inference time. No external embedding API is required by default.

Only information available at dispatch enters initial-routing features. Actual patch size, later failures and final outcomes are labels or recovery-stage evidence, not initial features. Unavailable features are represented explicitly. Free-text task inputs are untrusted data, not policy instructions.

An inference result contains artifact ID, feature-schema version, input hash, eligible arm labels, acceptance estimates, cost estimates (each per arm), supported cohorts and an abstention reason if applicable. An arm's identity is its tier's authority profile with the arm's own effort, plus the covering recipe's id, and the artifact vouches for an arm only when training observed that identity at that tier. The evaluation selects arms the same way inference does: for each record it scores, at every trained tier at or above the record's floor, each identity training observed there with that identity (not the record's own), and counts a test record as evidence for the selection only when its tier and its model, effort and harness are the selected arm's. Predictions are not guarantees. The deterministic runner remains responsible for policy enforcement and final acceptance.

### Rust learning implementation

Use regularized logistic regression over task/profile features for acceptance without escalation. Include task/profile interaction features so capability varies by task class rather than only assigning a global strength to each model. Fit a separate regularized cost estimator over complete execution strategies; evaluate its behavior on skewed costs and retain empirical cohort estimates as a baseline. Neither predictor may invent coverage for unseen profiles.

Training runs on the CPU. Implement or select a maintained Rust numerical solver after a focused dependency review; choosing Rust does not require rewriting general numerical infrastructure. Use numerically stable logistic loss, explicit convergence criteria, seeded data ordering, feature scaling fitted only on training data, and bounded iteration counts. Class imbalance and sparse features must be represented in evaluation rather than hidden by aggregate accuracy.

Keep modules small: features, dataset, learner, predict, evaluate, and registry. Share transformations between training and prediction. Test objective/gradient consistency, convergence on known fixtures, serialization round trips, missing features, non-finite values and decision-boundary behavior. Reproducibility means recording solver settings, seed and dependency versions; do not promise bitwise identity across all CPU architectures.

No GPU, neural network framework or embedding model is necessary for this design. An optional research experiment in another language must not become a build, training or runtime requirement for the shipped product.

## 17. Owned dataset, training and promotion

### Collection and labels

A task is the unit of sampling — a task retried to acceptance contributes one training example, not one per attempt. An execution profile combines model, effort, harness, context policy and bounded recovery policy. Capture the starting candidate identity and task contract so comparisons can reproduce the same task. The candidate identity is keyed on the recipe that produced the run, not only the model profile beneath it: a run under one revision of a recipe and a run under another are never pooled as if they were the same thing, and a byte-identical recipe (across two runs, whatever it is named) is one identity.

The labelling rule is versioned, and every dataset and artifact records which version produced it, so a dataset built under an earlier rule is distinguishable from one built under this one rather than silently comparable.

For every dispatch record pre-dispatch features, eligible profiles, selected profile, actual selection probability, versions, timestamps, verification results, human corrections where supplied, usage completeness and costs. Keep attempt-level outcomes distinct from complete-strategy outcomes: a cheap worker rescued by a stronger worker did not succeed without escalation, while the complete strategy may still have delivered an accepted result.

Labels are evidence-backed: verified acceptance, rejected candidate, blocked environment, interruption, escalation, user correction or confirmed regression. Infrastructure failures and unknown results are not silently converted into reasoning failures. Absence of later feedback is not proof of defect-free output. Maintain observation windows for delayed outcomes and identify still-pending labels.

A record whose dispatch carries no recorded route (§6, §12 — a pre-v13 dispatch, or one whose routing was otherwise never captured) still trains the acceptance model, but is excluded from any per-route breakdown of the dataset: folding it into a route it was never observed taking would misattribute it. That exclusion is counted and reasoned about, not silently dropped.

Primary optimization is expected complete-strategy cost subject to the configured quality requirement. The acceptance predictor is a measured proxy; independent verification is still mandatory. Track immediate acceptance and later confirmed quality separately.

### Acquiring comparative evidence

Ordinary usage observes only the chosen route. Deterministic historical logs cannot establish how an unchosen profile would have performed.

Relais includes an opt-in dataset collection command that replays suitable completed tasks from the original input snapshot on alternative profiles in isolated workspaces. Freeze the contract and verifier before replay; do not expose the accepted solution as context. Replay is allowed only for tasks whose actions and test environment can be reproduced without unintended external effects. Its cost is an explicit training expense.

Alternatively, authorized low-risk trials randomly assign among eligible strategies and log their actual probabilities. Routing trials stay within machine-owned risk, resource and data-handling limits. Inverse-propensity or other off-policy estimates require adequate action support; do not infer performance for profiles with zero observation probability.

Collect difficult failures as well as easy successes. Avoid counting many retries or near-identical task variants as independent examples. Pairwise labels, if used by an alternative learner, require comparable observed outcomes rather than an LLM judge's unsupported preference.

### Dataset construction and training

`relais dataset build` snapshots a versioned dataset from the ledger, references immutable evidence and reports exclusions, missing usage, class distribution and profile coverage. Dataset export is explicit; learning remains local by default.

`relais train` fits feature transformations on the training split only, trains regularized acceptance and cost predictors, and produces a candidate artifact. `relais evaluate` compares it with deterministic baselines on held-out data. Group task families and duplicate replays together; use temporal splits to test future-task behavior. Reserve a distinct calibration split where required and keep the final test set out of tuning.

Report calibration, failure/acceptance rates, cost including escalation, latency, abstention, cohort coverage and uncertainty. A cheaper policy that fails the quality requirement cannot pass promotion. Sparse or shifted cohorts abstain to baseline. Do not define readiness by one universal task-count threshold.

Every artifact contains dataset fingerprint, feature schema, label policy, learner configuration, dependency versions, destination profile identities, training date, evaluation report and supported cohorts. Store artifacts in a local registry outside worker write authority. No automatic retraining within a running task; each run pins its policy and predictor.

### Activation and continued learning

`relais promote ARTIFACT_ID` activates an evaluated artifact atomically. Scheduled retraining and automatic promotion are permitted only within an explicitly authorized envelope with independent evaluation gates. Preserve the previous artifact for immediate rollback. A user can disable learned routing without disabling execution, verification or accounting.

Cold-start operation uses conservative rules while collecting real outcomes. Training can run whenever data is available, but promotion requires evidence. The training pipeline ships in the complete release; the product cannot honestly ship with demonstrated knowledge of model/repository combinations it has never measured.

Detect drift through profile/version changes, feature distribution and observed outcomes. New model or harness versions, and new revisions of a recipe, receive new identities; evidence is not blindly inherited. Prediction abstains — never guesses — when asked about an identity the active artifact has not observed, whether the change is a model swap or a recipe revision bump. Confirmed regressions trigger investigation, trial suspension or rollback according to policy. Retraining must not relax risk floors, acceptance criteria or permission boundaries.

The complete learning loop is: execute → verify → label → build dataset → train → evaluate → promote or reject → monitor. RouteLLM informs the design; Relais implements and owns the entire learning loop in Rust.

## 18. Reusable context and verification evidence

Maintain a local index linking files, symbols, decision keys, tests and previously verified findings. Use repository paths, language-aware symbol extraction where supported and lexical retrieval; no general-purpose vector-memory service is required. Unknown language or missing impact information falls back to broader retrieval and verification.

Each finding records source fingerprints, repository revision, originating evidence, applicability and invalidation dependencies. Current aval resolution outranks remembered architectural claims. Context reuse never silently drops required constraints. Model summaries remain interpretations, with links to evidence.

Evidence another tool produced — a coverage report, an external scan, a signed attestation — is attached to the run and, where it answers one, to the acceptance criterion it speaks about, carrying the tool's name, that tool's own identifier for it and the subject it attests to. Attaching is recording, never deciding: relais stores what the other tool said and leaves the judgement where the contract put it. Preserve amont/attest ownership and never forge attestations.

An amont gate a declared criterion names as its evidence is settled the same way, automatically: relais asks amont's own documented interface, `amont attest covered`, against the candidate's own tree, and records what amont said as an external-attestation evidence row — never by reading amont's notes ref, parsing its attestation format, or verifying a signature itself. The gate and its attestation are amont's to own; relais asks and records, and decides nothing amont did not already say (§10).

Cache verification only when all declared relevant inputs match: candidate content, dependency lockfiles, toolchain, command, configuration and required environment identity. Checks with undeclared external dependencies or nondeterministic behavior are not cacheable by default. Preserve amont/attest ownership and never forge attestations. Cache misses rerun checks. Targeted impact analysis supplements mandatory checks rather than removing them. A profile's setup step is part of its configuration, and the programs the setup runs are part of the toolchain identity; a cached baseline skips the base's setup, not the task worktree's.

## 19. Bounded decomposition and recovery

A bounded planner may propose a dependency graph of work packages when a single task contains separable deliverables. Deterministic validation requires explicit scope, dependencies, per-package acceptance, integration acceptance and aggregate resource limits. Planning overhead is included. Simple tasks remain single-worker runs.

Each package uses an owned worktree at its declared input revision. Independent non-overlapping packages may run concurrently up to a machine-owned limit. Overlapping writes are serialized or reconciled in an explicit integration package. Workers may request child agents through the managed dispatch path. Each descendant inherits the root run budget and a scope no broader than its parent. Native child agents can also be observed, with enforcement capability reported explicitly; untracked recursive spawning is not accepted as supervised execution.

A scheduler assembles completed candidates and verifies the integrated revision. Independent receipts do not constitute final acceptance. Integration failures consume the same aggregate budget and cannot restart an unlimited task graph.

At bounded checkpoints, recovery chooses between retrieving missing context, a focused repair, higher effort, a stronger profile, or an explicit stop. Environment failures are not automatically escalated to expensive models. Strategy changes preserve the contract; scope or intent changes require a new contract revision.

## 20. Execution backends and final outcome feedback

The adapter contract includes launch, events, cancellation, resume/reconciliation, effective profile, permission capability and usage completeness. The mandatory Claude Code adapter uses supported native authentication and launches explicitly selected models. An optional alternative adapter can run another harness or local model. No adapter may advertise guarantees its backend cannot enforce.

The adapter never drops an effort silently. When a dispatch requests an effort and the CLI-accepted fact (§5) is `unsupported`, or `known` without that effort, the launch fails with `EffortUnsupported { effort, model, harness_version }` and the run ends blocked (`effort_unsupported`, exit 3) with no worker started. When the fact is `unknown` — the flag exists and its help lists no levels — the adapter passes the effort the repository policy explicitly configured through: a compatibility carve-out, so a harness whose help is unreadable does not silently lose spend controls a policy asked for. The carve-out covers only the configured effort, never one the adapter picked.

After acceptance, allow the user to record accepted unchanged, corrected, reverted or confirmed regression. Feedback is attributed to the candidate and strategy, with correction magnitude, evidence and an optional free-text note where available. The candidate is optional: a run accepted through a person's approval on a contract interrupted before verification completed never wrote a receipt, and feedback about it is still worth recording. Absence of feedback is not a positive quality label. Retain both immediate verification and delayed outcomes.

Feedback about a run a person salvaged (§9) is attributed to the candidate the salvage named, not to any candidate the run's own discarded attempt produced — that attempt's work was never what was merged.

`relais feedback --outcome not-shipped` records `never_shipped`: a person's statement that the accepted candidate was abandoned for reasons unrelated to its quality. It is never inferred, takes no `--magnitude`, and differs from `reverted` (shipped, then backed out): a never-shipped task leaves the accepted count (§11), while a reverted one stays accepted and stops standing. For labelling it is neither a positive nor a negative quality label: the task is excluded from training with a stated reason.

A task's later recorded outcome overrides its run's terminal state for labelling purposes: a reverted change or a later confirmed regression is never a positive label, whatever its run's state said. A task accepted through a person's approval rather than verification alone is recorded as such — the dataset names which route accepted it — and does not earn the positive label reserved for verified acceptance.

A receipt stored before `CheckOutcome::ended` existed (§10) has no such field. It still parses, and the check's end reads as unrecorded: never as an exit, a timeout, a cancellation or a signal, which would invent a termination the receipt never claimed. Only deserialization can produce the unrecorded state — a live check always records how it ended, enforced by the type — and an unrecorded end never counts as a pass.

Report savings as measured comparisons only when supported by comparable trials; otherwise label projections and assumptions. Final delivery includes candidate, verification receipt, route explanation, actual cost boundary, reusable evidence and unresolved decisions. Publishing and deployment remain outside product scope.

## 21. Additional final-product acceptance criteria

- No upstream pretrained router weights or scores are loaded in inference, feature generation or label generation.
- The dataset builder reconstructs dispatch-time features without future information and distinguishes independent worker success from escalation-assisted completion.
- Missing trained evidence produces conservative execution without disabling the rest of the product.
- Trial probabilities, eligible alternatives and complete-strategy outcomes are recorded; alternative-route claims require observed support.
- Dataset splits prevent related-task leakage, and promotion rejects artifacts that miss the quality or coverage gates.
- Context and verification caches invalidate on relevant input changes; incomplete impact information does not authorize skipping checks.
- Decomposed tasks remain within aggregate limits and the assembled candidate receives independent final verification.
- Artifact activation and rollback are atomic; workers cannot change the active training dataset, model or policy.
- Native training and loaded-artifact inference use identical feature transformations and agree within declared numerical tolerances; malformed artifacts and incompatible schemas are rejected.
- Training and replay costs, unknown usage and later regressions remain visible in reports.

## 22. RouteLLM implementation references

Inspected upstream files:

- README, blob `45cbe52c3d430869c3b0e7b07dadba7628bc17e6`.
- Controller, blob `8a02a05de8bf61d666758335b397d692bf4aa86e`.
- Router implementations, blob `0096c0aa18c5c42e27e117e5aef377a7895e727a`.
- MF model and embedding call, blob `09fbb25e5d0d958d661dfd5ddd9bfc7492cbe276`.
- Dependencies, blob `829339748a37da3d2ed52cacfc2f08c1e7ebaac7`.

These source hashes identify the inspected files, not a tested dependency lock or benchmark result. The upstream package carries Python/PyTorch and other dependencies. These references inform the design only. Relais does not depend on their interfaces, pretrained models, embedding calls or Python stack.

## 23. Multiple Claude Code sessions and nested agents

### Required normal workflow

Multiple interactive Claude Code sessions, each spawning multiple agents, are a first-class supported workflow. Three tabs may work on different repositories or different worktrees of the same repository simultaneously. Each can launch foreground or background workers and bounded nested agents. Relais must not impose a universal single-worker mode or require the user to serialize normal sessions.

The existing one-repository-per-run contract remains: many runs across many repositories can coexist. A session can contain several runs, and a run can contain an agent tree. An agent finishing one invocation may later resume; agent identity and invocation identity are distinct.

### One coordinator per OS user

A shared Rust coordinator, started lazily by the CLI/integration, manages registrations, admission, resource leases and aggregate accounting. Use a permission-restricted local Unix socket on macOS/Linux and an equivalent local IPC mechanism on other supported platforms. Atomically elect one coordinator; simultaneous startup must not create independent schedulers.

All participating tabs connect to this coordinator. A CLI process exiting does not cancel an ongoing background run. Expose session/run/agent-tree status, queued work, limits and cancellation through the CLI. Share one model registry and ledger, using short SQLite transactions; no database write transaction stays open during a model call, build or training job.

Identify root session, run, work package, attempt, agent, parent agent, invocation, repository, worktree and candidate separately. Register joins/resumes idempotently. Missing parent evidence is reported as unknown rather than guessed. Coordinator requests are validated; a child's self-reported budget or permissions cannot enlarge its grant.

The root session a CLI call attributes to comes from `CLAUDE_CODE_SESSION_ID` — the variable Claude Code itself exports to its Bash tool — unless `RELAIS_SESSION_ID` is set explicitly, which always wins. Neither present, the parent process id names the tab instead, and `relais run`/`relais plan` print which of the three happened rather than let a PID stand in silently for a session.

A coordinator restart adopts every dispatch the ledger still calls `launched` and bound to a live process, so caps already in force keep binding across the restart: the row it adopts from names the session it belongs to, the money it reserved, its source, its parent dispatch and its depth, and `Ledger::live_dispatches` returns all five rather than only a dispatch id, run id and pid. `Coordinator::start` builds the adopted dispatch's admission request from those values, not from placeholders — the per-session cap that refused a fourth agent before a restart still refuses it after one, because the adopted dispatch still names the real session rather than a literal `"unknown"` every restart used to share. A row written before the migration that added `source`/`parent_dispatch`/`depth` carries `NULL` in all three forever: adoption reports that as `Attribution::PreMigration` rather than guessing root/depth-zero and calling it fact, and `relais coordinator status` counts those dispatches apart (`adopted_pre_migration`) so a person can see how much of what is live rests on a complete record. This crate's own managed dispatches are always root (depth 0, no parent) — a deeper, hook-admitted spawn never reaches the ledger at all (see "Managed and observed execution" below) — so `parent_dispatch`/`depth` on a post-migration row are always `NULL`/`0` and never a guess either.

### Managed and observed execution

For Relais-managed dispatch, reserve capacity and budget atomically before launch. Return a dispatch ID; retries with the same ID cannot create duplicate agents. The adapter binds the resulting harness agent/session ID to that reservation.

For ordinary native Claude Code subagents, collect available lifecycle and transcript events and track the tree without requiring users to replace every delegation with a manual CLI command. Where the installed harness provides a reliable pre-dispatch interception point, the adapter can enforce admission and select permitted invocation parameters. This capability must be verified by integration tests, including nested starts, resumes, forks and cancellation.

`SubagentStart` is an observation/context event, not a creation veto. A post-start hook cannot enforce an atomic global spawn cap. Claude Code's own session-local concurrency/depth settings are defense in depth, not a cross-tab resource scheduler. Hook/tool names and payloads are version-dependent; probe them through the compatibility matrix.

If a launch/resume path cannot be intercepted, label its enforcement `observed`, include known usage, and do not claim hard aggregate concurrency/model guarantees over it. Strict runs use managed dispatch for such paths. Coordinator outage must not silently turn a strict managed launch into an unmanaged launch; preserve the request and report unavailable admission. Ordinary unwrapped Claude Code sessions remain usable and are not forcibly terminated.

### The hook compatibility matrix

`relais hook --probe --record <dir>` captures what the installed Claude Code actually sends, so tests are written against a transcription of a real session rather than a belief about one. It is run by hand, from a settings file you write; relais itself wires it nowhere.

`relais hook --probe --record <dir>` is record-only: it reads one hook payload on stdin and writes it to `<dir>` byte for byte, alongside the event name and the order it arrived. It parses nothing beyond a best-effort event name for the file name, decides nothing about the payload's shape, and always exits 0 without writing to stdout — a hook that exits non-zero or writes to stdout can block or alter the tool call that triggered it, and a probe that changed the session it was measuring would be measuring itself. A payload that is not valid JSON, exceeds a sane size cap, or arrives for an event the handler was not wired for is still recorded and still exits 0.

The seven installed targets are named once, in `hook::TARGETS`, and a test fails if the target set `--hooks` installs disagrees with it. The three tool-scoped events install matched to the Agent tool and its legacy name (`Agent|Task`, since the harness still sends either name); the four lifecycle events install with no matcher, because they are not tool calls. Every handler that is not `PreToolUse` carries an explicit timeout of 10 seconds. relais installs nothing on `WorktreeCreate` or for `SendMessage`: Claude Code makes its own worktrees, and the plugin starts, continues and stops relais's agents (§23). **Migration from the hook-side native path (#171):** that relais installed the matcher `Agent|Task|SendMessage` and a `WorktreeCreate` handler. Installing over such a file removes relais's own command from an entry on that matcher and from every `WorktreeCreate` entry (an entry goes when relais's command was its only hook, and a `WorktreeCreate` array that is left empty goes with it; another hook on either stays), so one Agent call never runs `relais hook` twice and no handler without code behind it answers `WorktreeCreate`, and wires the `Agent|Task` entry; every foreign entry on any event is left as it was. Uninstall removes relais's command from the current matcher, the old one and `WorktreeCreate`. `relais doctor` reports (`hook-wiring`) a settings file that still has the `SendMessage` matcher or relais's `WorktreeCreate` entry as needing `relais install --claude --hooks --write`.

### Reporting the hook by exercising it

`relais doctor`'s ordinary run reports the live hook by running it, not by reading `settings.json` back: it takes the exact command recorded there, spawns it with a fixture `PreToolUse` spawn payload on stdin, and redirects its config and state directories to a scratch environment of their own — one whose machine settings say to refuse admission when the coordinator cannot be reached, which a scratch state directory holding no coordinator socket never can be. This never touches the person's real hook journal or reserves a seat in their real coordinator; measuring the hook must not change the thing it measures.

Every settings file the Claude Code harness merges is read, not just the first that matches: the project's own `.claude/settings.json`, the project's `.claude/settings.local.json`, then the user's `~/.claude/settings.json` (issue #94 — Claude Code merges these and runs every matching handler, so a hook recorded only in the local file must be found, exercised and timeout-checked exactly as one in the committed file is). Outcomes, reported apart because one of them is the dangerous one: no relais command recorded on `PreToolUse` in any settings file this repository can see; a recorded command that was exercised and refused, exit 0 with a deny on stdout, as configured; a recorded command that ran but printed no deny — wired in and enforcing nothing, a dead guard that reads as enforcement to anyone who has not gone looking, reported together with how it ended, since a hook that hung and a hook that exited 0 with nothing to say are different defects; and a relais command recorded in more than one settings file, which is a finding of its own naming every file it is in, since Claude Code runs both on every spawn and nobody asked for that deliberately. A further case is not a verdict on the hook at all: when the exercise could not be staged or the command could not be spawned, doctor says whether it refuses is unknown and why, because "it enforces nothing" is a claim about a run, and that run did not happen. This check costs nothing and touches no network — it is part of the ordinary `relais doctor` run. `relais install --claude --hooks` reads the same list before writing: a relais command already recorded in one of the OTHER files it refuses to write, so it never produces the duplicate this issue is about.

### Turning a hook payload into a typed event

`relais::hook::event::parse` turns the bytes a hook receives on stdin into a typed `HookEvent`, against the nine payloads transcribed under `crates/relais/tests/fixtures/hooks` rather than an invented shape. This is parsing only: the module decides nothing, admits nothing and records nothing — it establishes what a payload IS, so the packages that act on it argue about policy rather than about JSON.

A payload lands in one of: `SessionStart`, `SessionEnd`, `SubagentStart`, `SubagentStop`, `AgentToolCall` (a `PreToolUse`/`PostToolUse`/`PostToolUseFailure` event whose tool is the Agent tool, or its legacy name `Task`), or `NotOurs`. `NotOurs` is one case for two different reasons: a tool event for any tool other than the Agent tool (`0001-PreToolUse.json`, a `Read`, lands there), and a payload that is not JSON, is not an object, names no event, names an event this binary does not know, or exceeds a one-mebibyte cap. A hook cannot refuse to answer, so `parse` never returns an error and never panics.

The fields this module reads are deliberately not `#[serde(deny_unknown_fields)]`, unlike this crate's usual habit: the recorded payloads carry `cwd`, `effort`, `permission_mode`, `transcript_path`, `background_tasks`, `session_crons`, `stop_hook_active` and more this module has no use for, and a future Claude Code release will add others it has not carried yet. Rejecting fields this module does not model would turn every harness release into an outage.

The caller's agent (`agent_id` at the top level) is optional because it is genuinely absent there: `0002-PreToolUse.json`, the spawn itself, carries none, while `0004-PreToolUse.json`, a call the subagent made, carries one — pinned to that measurement rather than to a guess. The session, tool use, agent, agent type and prompt each get their own newtype (`SessionId`, `ToolUseId`, `AgentId`, `AgentType`, `PromptId` in `ids.rs`), so a function taking two of them cannot be called with them swapped.

`ids::derive_dispatch_id(session, tool_use)` and `ids::derive_run_id(session)` are pure: the same inputs always derive the same identity, reading no clock, no counter and no environment, unlike `IdSource`'s minted `RunId`/`DispatchId`. This is what lets a hook admission decide that a hook firing twice for one tool call is the same request rather than a second one — the retry derives an identical `DispatchId` without asking the coordinator.

`MachineSettings.admission` (`policy::HookAdmissionSettings`) carries what a hook-admitted agent needs that no other machine-owned type states: `binding_lease_secs`, how long a binding may be held before it lapses; `dispatch_reserve_micros`, what one dispatch reserves against its session's money before the provider reports real usage, defaulting to zero — observation rather than refusal; and `on_coordinator_unreachable`, a named `CoordinatorUnreachableBehavior` (`carry_on` or `refuse`) rather than a boolean, defaulting to `carry_on` so a coordinator restart does not turn into blanket admission refusal. The agent and depth caps stay on `ConcurrencyLimits` (`max_active_agents_per_session`, `max_agent_depth`, enforced by the coordinator, whose defaults that module states) rather than being redeclared here. All three fields are optional with defaults, so a `machine.toml` written before this section existed still parses and still means what it meant. None of this admits an agent, binds a dispatch, reserves money or contacts the coordinator — those decisions belong to the admission and coordinator packages these settings exist for.

### Binding a hook-admitted agent, with no process id and no certain join

A hook-admitted agent never reports a pid — the hook path has none to report — so the pid-checked `AdmissionState::bind`, which refuses a pid that is not alive, cannot serve it: there is nothing to check it against. `AdmissionState::bind_agent_lease(dispatch_id, agent_id, provenance, now)` binds it instead, on a lease held for `agent_lease_ttl` (`set_agent_lease_ttl`, defaulting to `DEFAULT_AGENT_LEASE_TTL`) rather than a pid the process table can check. `coordinator::Coordinator::start` reads `HookAdmissionSettings::binding_lease_secs` from machine.toml and calls `set_agent_lease_ttl` with it before serving, so the machine setting governs the lease in force rather than mirroring `DEFAULT_AGENT_LEASE_TTL` by hand; that constant remains only as the fallback for an `AdmissionState` nothing reconfigures. The two kinds of binding coexist on `Dispatch` — one dispatch is bound by pid or by lease, never both — and a reader of `StatusSnapshot` can tell which: `bound_processes` for the pid-checked kind, `leased_agents` for the leased kind.

Nothing in a single hook payload joins the tool call that admitted an agent to the agent that then ran, at the time it starts: the spawn (`PreToolUse` on the Agent tool) carries a `tool_use_id` and no `agent_id`; its own `SubagentStart` carries an `agent_id` and no `tool_use_id` (§"Turning a hook payload into a typed event", above). Only the spawn's `PostToolUse`, once the call returns, names both — `tool_response.agentId` — which is what `relais hook` binds the seat with (§"Answering for real: `relais hook`", below). A binding built from that join records how sure the join is as `Provenance` — `Known`, `Inferred { basis }`, or `Unknown` — rather than in a comment, so a later reader of a seat, a cost or a parentage that rests on this binding can tell evidence from a guess.

`relais::hook::pairing::pair` makes the join from arrival order, over an already-typed stream of `HookEvent`s: it decides nothing about admission and parses nothing. A spawn with exactly one `SubagentStart` still awaiting it pairs `Known` — `tests/fixtures/hooks-concurrent` recorded five agents spawned at once, genuinely overlapping, and every `PreToolUse:Agent` was immediately followed by its own `SubagentStart`, five for five, never interleaved. A start that arrives with more than one spawn still waiting pairs the oldest as `Inferred`, naming the ambiguity as its basis; no recorded session has ever produced that case (the harness stages spawns roughly 30ms apart), so the code path rests on reasoning rather than evidence, and its test is built from an invented sequence rather than from a fixture, visibly so.

Reconciliation reports a lapsed lease — past `agent_lease_ttl` with no heartbeat — as `ReconcileReport::expired`, never folded into `dropped` (a bound pid found dead in the process table) or `unbindable` (a pid-bind that never arrived after `UNBINDABLE_AFTER` grace periods). The three mean different things: a lapsed lease says a hook stopped reporting; a dropped lease says a process ended; an unbindable one says a launcher died before it ever bound. Conflating any two would lose a distinction the reader needs.

### Deciding what a hook says

A hook cannot refuse to answer, and it must never say yes on a person's behalf: doing so would override that person's own permission settings, which is not this binary's business. So `relais::hook::decide::decide(event, settings, coordinator)` resolves every tool-call case to `HookAnswer::Silent` or `HookAnswer::Refuse { refusal }`. There is no variant for an affirmative answer at all, and a test greps the module's own production source for the word that would spell one out, so the rule survives a future author who has not read this paragraph. There is also no variant that only advises: this hook denies or stays silent, and neither widens what a call may do. A `WorktreeCreate` event and a `SendMessage` call are not events relais acts on (`HookEvent::NotOurs`): `relais hook` exits 0 with no output for them, so Claude Code makes its own worktrees and runs its own continuations. A broader plan for this crate once sketched `Observe`/`Advise`/`Deny`; this module narrows that to the dispositions the hook actually has occasion to produce, recorded here as a deliberate omission rather than left to look like one nobody noticed. This is also not a shape `amont-agent` shares: that crate's `decision::Decision` (`Silent`, `Advise`, `Deny`, `Context`, `Assert`) is a separate, richer vocabulary tied to its own events and channels, and the two stay independent — a third type claiming to unify them is exactly how one evidence envelope in this fleet ended up with three vocabularies, two of them falsely claiming to reuse the first. Convergence, if it ever happens, would have to precede any shared crate, not follow from one type here quietly standing in for both.

`HookAnswer::Refuse` carries a `hook::decide::Refusal`, not a bare string: `rule: RefusalRule` (spanning this module's own rules — `QueueTimeout`, `CoordinatorUnreachable` — and admission's own `Refusal` codes under `RefusalRule::Admission`), `reason` (what was exceeded), `remedy` (what the person can do about it), and an `Availability` (`Decided` or `Unknown`) that `RefusalRule::availability()` DERIVES from the rule rather than a field carried beside it. `Refusal::sentence()` renders these into the one sentence a person and the model both see, built FROM the fields rather than stored beside them as a second, independently-maintained copy — the wording for each rule is unchanged from before this type existed, only what assembles it moved. `availability` is the field that answers a question the prose alone could not: a cap refusal and a coordinator that could not be reached under `on_coordinator_unreachable = "refuse"` both deny the spawn, and only `availability` says which happened — `Decided` for every admission `Refusal` code and for a queue that timed out (relais reached the coordinator, or gave up waiting on a real answer), `Unknown` only when the coordinator could not be reached at all and relais is refusing because it could not tell. It is derived and not stored, which is what makes the guarantee real: carried as a field it was set by hand at each producer, so `rule: CoordinatorUnreachable` beside `availability: Decided` compiled and would have journalled an outage as a decision — the very confusion the value exists to remove. One exhaustive match with no `_` arm, over the rule and over admission's codes within it, means a new variant on either enum cannot compile until it says which it is, and an inconsistent pair cannot be built at all.

`decide` is a pure function of the typed `HookEvent`, `policy::HookAdmissionSettings`, and whatever the coordinator answered (`Option<admission::Decision>`; `None` for "could not be reached"). It reads no clock, touches no filesystem and makes no network call, so every case can be read and tested without running a session; it also touches no coordinator, journal or ledger, and installs nothing — deciding and admitting are different packages. Only a `PreToolUse` on the Agent tool is ever refused: every other event — the four lifecycle events, `NotOurs`, a `PostToolUse`, and a `PostToolUseFailure` — is `Silent`, because a tool call that has already returned cannot be blocked. `PostToolUseFailure` is matched explicitly to `Silent` rather than folded into a wildcard: across five probe sessions (`tests/fixtures/hooks/README.md`, `tests/fixtures/hooks-concurrent/README.md`) it has never fired, including one whose tool call genuinely failed and arrived as an ordinary `PostToolUse` carrying the error, so installing real handling for it would claim a coverage this crate does not have. If a later probe run records one, that recording is what should reopen this.

For a spawn, a coordinator that could not be reached is handled by `HookAdmissionSettings::on_coordinator_unreachable` rather than by a decision made here: `CarryOn` (the default) stays silent, `Refuse` declines and names the coordinator as unreachable. `Decision::Granted` and `AlreadyAdmitted` resolve `Silent` — neither is a refusal, and neither is spoken as an affirmative answer either. `Decision::Queued` is not decided on directly: `hook::respond::wait_for_a_seat` polls the coordinator again at `runner::ADMISSION_POLL`'s cadence — the same re-asking protocol `AdmissionState::request` already serves `relais run`'s own managed dispatch — for up to `HookAdmissionSettings::queue_behaviour`'s bound, and only the answer that poll settles on (still `Queued` at the deadline, or whatever the coordinator answers next) reaches `decide`. `Decision::Refused { code, detail }` resolves to `Refuse`, whose `Refusal` is built from a table over every admission `Refusal` variant: `rule: RefusalRule::Admission(code)`, `reason` the coordinator's own `detail` (what was exceeded), and `remedy` one line of advice (what the person can do about it — raise a limit, wait for a reservation to settle, start a new run), because a refusal a person cannot act on gets ignored, and this one arrives in the middle of their work. A `Queued` answer still resolves `Refuse` once the wait is exhausted, but the reason now says how long this firing actually waited, not that a hook cannot hold a call open — see below, it can. Every `Decision` and `Refusal` variant is matched by name, with no `_` arm, so a variant added to either fails this module's compile until the table says what it means.

Three facts, measured directly against a real Claude Code 2.1.282 session, govern every deadline in this package: a `PreToolUse` hook holds its tool call open for as long as it runs (a 5s hook produced a 5s wait before the call proceeded); a hook that outlives the harness's own handler `timeout` is killed and its answer discarded while the guarded tool call proceeds anyway — an expired hook is an admission nobody decided, not a refusal; and with no `timeout` field configured at all there is effectively no ceiling (a 300s hook ran to completion, and the session waited the full 312s). `queue_wait_secs` under `[admission]` in machine.toml (default: 2 seconds, `policy::DEFAULT_QUEUE_WAIT_SECS`) parses into `policy::QueueBehaviour::{RefuseImmediately, WaitUpTo(Duration)}` — zero meaning refuse immediately, exactly as before this existed — and `relais install --claude --hooks` derives the `PreToolUse` handler timeout it writes from that wait plus `ipc::CONNECT_TIMEOUT`, twice `coordinator::REQUEST_TIMEOUT` and a stated margin (`install::settings::derived_pretooluse_timeout`), so the hook always has enough of its own budget left to withdraw its request and print a refusal before the harness would kill it. Every other installed handler carries a modest, explicit timeout of its own, because an absent one waits indefinitely. `relais doctor` reads the timeout actually recorded in settings.json and fails — not warns — when it no longer covers the currently configured wait, because the hook itself cannot read its own handler timeout to check.

A hook killed mid-wait — by the harness's own timeout, or an interrupt — cannot strand the seat it was polling for: `AdmissionState::reconcile` frees a dispatch that `drain` admitted but that nothing ever claimed after `admission::UNCLAIMED_GRACE` (five seconds, well short of `admission::LEASE_GRACE`'s 300, and well longer than `runner::ADMISSION_POLL`'s 250ms cadence, so an ordinary poller is never reaped out from under itself). The hook journal records `waited_ms` and an `outcome` of `immediate`, `admitted_after_waiting`, or `refused_after_waiting` for every firing, so backpressure and stalling are distinguishable in the record.

`decide` never panics in its own right, but `decide_or_silent` wraps it in `catch_unwind` anyway and turns any panic into `HookAnswer::Silent`: a hook that dies mid-decision would otherwise leave the tool call it was watching unanswered. `HookAnswer::stdout_payload` renders the decision for stdout — `None` for `Silent` (nothing is printed at all) and, for `Refuse`, a `hookSpecificOutput.permissionDecision: "deny"` JSON string carrying `permissionDecisionReason` — without performing the write itself; the caller that owns stdout does that. The hook always exits 0. That is the current spelling rather than the older top-level `{"decision":"block"}` one; both were run against real Claude Code 2.1.282 and each blocked the tool call, reaching the model identically as a hook error, so the choice is for longevity and not for behaviour. If a refusal ever stops taking effect, re-run that comparison rather than trusting this paragraph: a deny that is ignored exits 0, the call proceeds, and nothing reports a failure.

### Answering for real: `relais hook`

`relais hook`, run with no flags, is the caller `decide.rs` used to say did not exist: `hook::respond::handle(payload, settings, gate)` parses the payload, asks `gate.admit` about a spawn — built from `ids::derive_dispatch_id`/`derive_run_id` over the session and tool-use IDs, and only for a `PreToolUse` on the Agent tool; every other event is answered with no call at all, because `decide` never refuses one and a request this function has no later chance to release would reserve a seat for nothing — applies `decide_or_silent`, and, on a refusal, calls `gate.withdraw` for the dispatch it asked about before returning. That withdrawal is what keeps a spawn relais refuses from being left holding whatever its own request reserved: `admission::AdmissionState::request` never actually reserves anything for a decision that comes back `Refused` (the hard limits are checked before anything is admitted), so the call is usually a no-op today, but the refusal path calls it unconditionally rather than depending on that staying true. `handle` itself is wrapped in `catch_unwind`, the one layer below `decide_or_silent`'s own — this is where a socket and a clock are actually touched, neither of which `decide` reaches.

The first spawn of an ordinary session — one no `relais run` ever started — finds no run on record, because nothing before it ever registers one: `gate.admit` answers `Refused { code: UnknownRun, .. }`. Rather than let that stand as the hook's only possible answer, `ask_coordinator` registers the run itself before giving up: it derives the run id from the session already in hand exactly as `admit` did (never one a caller handed it, which is what keeps a registration from laundering a run relais never derived), calls `gate.register_run` with no budget, agent-cap or depth override at all, and retries `admit` once. What the retry answers — admitted, still unknown, or anything else — is what the hook reports; there is no second retry, because a hook cannot hold a tool call open while it tries again. An unreachable coordinator (`gate.admit` returning `Err`) is not registered against at all: that failure is `CoordinatorAnswer`'s `None`, resolved by `on_coordinator_unreachable` as before, and registering against a daemon that just failed to answer would turn every ordinary tool call on a down machine into a second connection attempt. This is what makes the caps in `[concurrency]` on machine.toml reachable on the hook path at all: before this, every session `relais run` had not already started saw `UnknownRun` on its very first spawn and never anything else, so no cap below it — the per-session agent count, the per-run aggregate limit — had ever had the chance to be the thing that actually refused a spawn.

For the per-session cap to count agents rather than spawns, the seat has to follow the AGENT, not the tool call, and the two end at different moments: an Agent `PostToolUse` fires when the call returns, and a real Claude Code 2.1.282 session showed it returning at LAUNCH — ~100ms after its `PreToolUse`, `duration_ms: 8`, `tool_response.isAsync: true`, `tool_response.status: "async_launched"` — while the agent it launched ran 6–16 seconds more (`tests/fixtures/hooks/0009-PostToolUse.json`, `0010-SubagentStop.json`). A seat given back on `PostToolUse` was given back at launch, and the cap did not bind. So `handle` admits on `PreToolUse`, binds on `PostToolUse`, and releases on `SubagentStop`:

- **Admit on `PreToolUse`**, as above.
- **Bind on `PostToolUse`.** Its `tool_response.agentId` names the agent the call launched — the only place a tool call and the agent it produced appear in one payload — and `event::ToolCallPhase::Post { launched_agent }` is the only phase that carries it, so the type says so. `handle` calls `gate.bind_agent_lease(dispatch_id, agent_id, Provenance::Known)` (wire request `BindAgentLease`), the dispatch id derived from the same `tool_use_id` the spawn carried. Nothing is settled or released here. A `PostToolUse` that names no agent, and a `PostToolUseFailure` (which launched nothing), bind nothing and release nothing.
- **Release on `SubagentStop`.** It names the agent (`agent_id`) and no tool call, so its dispatch id cannot be derived; `handle` calls `gate.settle_by_agent(session_id, agent_id, None)` (wire request `SettleByAgent`), and the coordinator settles and releases whichever dispatch is bound to that agent in that session. `AgentSettleOutcome::NothingBound` is an ordinary answer, not an error: an agent spawned before the hook was wired, or while the coordinator was down, ends like any other.

The order of the last two does not matter. A synchronous spawn's `SubagentStop` arrives BEFORE its own `PostToolUse` (`tests/fixtures/hooks-concurrent`), finding nothing bound; the coordinator remembers that stop (bounded, like finished dispatch ids), and the bind that follows ends the dispatch at once. Settlement is `None` rather than a figure, because a hook payload carries no usage: `None` books the reservation as a lower bound and marks the run uncertain, which is what not knowing looks like when it is recorded honestly, where zero would be a claim that the agent was free.

The failure mode is stated rather than implied away: **an agent whose `SubagentStop` never arrives holds its seat until its lease lapses** — `binding_lease_secs` for a bound agent, `LEASE_GRACE` plus `UNBINDABLE_AFTER` reconciles for a dispatch no `PostToolUse` ever bound — and reconciliation then settles it as unknown. The cap over-counts for that long; it never under-counts because an end was missed. Nothing renews a hook-admitted agent's lease while it runs, so an agent that outlives `binding_lease_secs` is expired by reconciliation before its `SubagentStop` arrives.

Adding the two calls changed the wire protocol: `coordinator::PROTOCOL_VERSION` became 3 with these two calls, and a v2 daemon left running is named as a version skew at the first call (`relais coordinator stop` fixes it). The native-dispatch calls below made it 4: a v3 daemon has none of them. The relais plugin's calls (`native_hello`, `native_hello_age`, `native_bound`, `native_stopped`) made it 5.

### Native dispatches

A native dispatch is a worker relais asks the parent session to run as a subagent, so Claude Code renders it as its own. It is an ordinary dispatch the runner already admitted under its run (a `DispatchRequest` the runner sent, `ManagedRun`), plus one record in the coordinator's native registry (`admission::native`), keyed by dispatch id. A record moves `Requested { ask }` → `Claimed` → (for a spawn) `TreeGiven { name }` → `Bound { agent_id }` → `Stopped { agent_id, status, usage, answer }`, or ends `Failed { reason }`. `ask` is `Spawn { subagent_type, model, prompt, worktree }` or `Continue { agent_id, message }`: registration (`RegisterNative`) is refused for a dispatch that is not admitted, is finished, belongs to another session, or already has a record. The plugin's flow (below) registers a request, binds its agent and reports its stop; `Claimed` and `TreeGiven` belong to the retired hook-side path (#171), which nothing drives any more: `relais hook` neither claims a call nor answers `WorktreeCreate`, and no marker travels in a prompt. The record is dropped when its dispatch reaches a terminal state, and `NativeStatus` then answers that the dispatch finished, as `finished` does for admission. A cancelled dispatch, or run, is still on record until the cancellation is acknowledged, and `RegisterNative` refuses it in that window (`cancelled`), as ordinary admission refuses a cancelled run (`RunCancelled`).

### The native presentation (`relais run`, from the relais plugin)

`relais run` launches every dispatch — each worker attempt, the reviewers and the planner — as a native subagent the relais plugin of the parent Claude Code session spawns, and judges it the same way every time: the same snapshot, scope check, verification, review, repair and escalation. relais never launches `claude -p`; a test fails if a source builds that argument. A native worker runs under the parent session's sandbox and permissions; relais keeps none of its own (§8). The conversation with the plugin is protocol lines on stdout (§29) and the short `relais native` processes the plugin calls.

`NativeBackend` (`adapter::native`) is the one `Backend` that launches; it keeps the harness prober (`adapter::claude`) only to report which Claude Code is installed. For each dispatch it decides **spawn or continue**: it CONTINUES the agent of the task's last native attempt when that attempt bound an agent and its model equals this launch's model (the continuation keeps the agent's own effort; when the attempt asked for another, that is recorded as the attempt's `native_note` evidence), and SPAWNS otherwise (a first attempt, an escalation to another model, no agent known). The text sent is the attempt's prompt, with no marker. It registers the request (`RegisterNative`; a refusal ends the launch with the refusal as its failure detail), and writes one protocol request line through the protocol writer (§29), never `println!`: a `spawn` line for a fresh agent, a `continue` line for a repair on the same model, and a `stop` line when the attempt is cancelled while its agent runs. The text lines `RELAIS-SPAWN` and `RELAIS-CONTINUE` are gone.

It then polls `NativeStatus` every admission poll. A `Stopped` record ends the wait: with status `completed`, the agent's answer is the result and the agent id the session id; with `failed` or `killed` the attempt ends `Exited(1)` with a failure detail naming the agent and the status and no result text. A dispatch still `Requested` after the **spawn wait** (120 s, internal: not a flag) ends the attempt with a detail starting `native_spawn_missing:` (no session spawned it), so the run stops interrupted and is never silently unmanaged. Each poll also asks when the session's plugin last said hello; none for 60 s (`native::HELLO_FRESH`) ends the attempt with a detail starting `mod_gone:`. A `Failed` record ends it with that reason; the attempt's wall time ends it timed out; a cancellation ends it cancelled; a record the coordinator no longer holds ends it with a detail saying so. Each of these has no result text, so the attempt is interrupted. The runner's own heartbeat keeps the dispatch's lease alive for the whole wait.

**Usage** is what the plugin reports in `stopped`, summed by it over the dispatch's turns: `input_tokens`, `output_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens` (priced at the 5-minute cache-write rate, the one total having no tier) and the `model` that ran. The cost is tokens times the machine's `[pricing]` table, booked as `Cost::Estimated` with `CostKind::EstimatedApiEquivalent`, and is unknown (never zero) when there is no table, the report carries no usage or no model, or the model has no price. The settlement of the dispatch's reservation and the ceilings see the estimated figure. The effective model is the reported `model`.

**Rollback ids.** The agent's transcript is read for message ids only, never for usage. On a stop, relais looks for `~/.claude/projects/*/<session>/subagents/agent-<agent>.jsonl` (`$CLAUDE_CONFIG_DIR/projects` when set), parses it with `orchestration::parse_transcript`, and returns its message ids as `LaunchResult.booked_message_ids`; the runner records them in `native_usage_messages` in the same transaction as the usage event (§11), so an older relais's `usage import` still skips them after a revert. When the file is missing or yields no id, the usage is booked all the same and the run emits a `decision` event `rollback_ids_missing` whose `reason` is the dispatch id.

**Shipped worker definitions.** The relais plugin (installed by `relais install --claude`) ships one agent file per (model, effort) in `native::worker_agent_types()`: `agents/relais-worker-<model>-<effort>.md` for the models `haiku` (effort `default` only), `sonnet`, `opus` and `fable` (efforts `default`, `low`, `medium`, `high`, `xhigh`, `max`), named by `native::worker_agent_type`. The frontmatter has `name` (the type), a `description` saying the agent is spawned only by relais and never picked by hand, `tools: Read, Grep, Glob, Edit, Write, Bash` (no `Agent`: a worker cannot spawn), `model`, and `effort` except for `default`. The body says the task, rules and acceptance arrive in the prompt, that verification decides acceptance, and that the worker does not commit, push or publish. They are the plugin's files, not `.claude/` files: a relais that wrote them there is cleaned up by install (owned, unedited ones are removed). A spawn whose (model, effort) has no shipped definition is refused before anything is registered or printed: the attempt ends interrupted with a detail starting `native_worker_missing:` naming the model and effort and saying to use a model alias (`haiku`, `sonnet`, `opus`, `fable`). The plugin names them `relais:<type>`, and a `spawn` line's `subagent_type` is that name. A continuation names an existing agent and needs none.

**The native rules.** A worker prompt carries the native rules text: the worker runs as a Claude Code subagent in the task worktree, its working directory; it works only there; it commits, merges, pushes and publishes nothing; it cannot spawn agents; pipes, redirects and `&&` work under the session's own permissions; it leaves no scratch files in the directory (they would become part of the candidate) and gives findings in its final message; it finishes with `DONE` or `relais-blocked: <reason>`. It has no sandbox or `$TMPDIR` wording.

**Only from the plugin.** `relais run` is always native for workers; `--native` and `--native-spawn-wait` are removed. It is refused before anything runs (exit 2, `CliError::Usage`) unless `--protocol` is given, `RELAIS_HOST=claude-code-mod` is set (the plugin sets it on the child), and the session (`RELAIS_SESSION_ID`, else `CLAUDE_CODE_SESSION_ID`, as `coordinator::resolve_session` reads them) said hello within the last 60 s; the message says to start runs from Claude Code with the relais plugin. It is also refused on a Claude Code outside `native::SUPPORTED_CLAUDE_CODE` (`>= 2.1.291, < 2.2.0`), read from `claude --version` through the capability probe, and the message names the range and the installed version; a version that cannot be read is outside the range. This is a guard against mistakes, not a security boundary: a person who sets both variables and a hello by hand gets a run that waits for spawns nobody makes and ends `native_spawn_missing`.

The run also checks, before anything is dispatched, that every model in `[models.*]` can be priced, and ends after an attempt that booked a record it could not price `blocked:native_unpriced`, so an unknown cost never settles as the whole reservation and ends the run on a budget refusal (§11).

A native attempt is admitted with `source = native_run` (`DispatchSource::NativeRun`), and its ledger dispatch row carries `source = native_run` and the agent id once the launch returns.

**The plugin's calls.** `relais native` holds short processes the plugin calls over the coordinator (`coordinator::Client`); exit 0 acknowledges and non-zero asks the plugin to retry:

- `relais native hello --session <id>` records the session's last hello (`NativeHello`; `NativeHelloAge` reads it back).
- `relais native bound --dispatch <d> --agent <a>` binds the dispatch's agent lease to the agent and moves the record to `Bound` from `Requested` or `Claimed`; no tree name is needed, since the plugin sets the cwd itself. Idempotent: the same agent again acknowledges; another agent is refused (exit 3) and nothing changes.
- `relais native stopped --dispatch <d>` reads `{"agent", "status": "completed"|"failed"|"killed", "usage": {"input_tokens", "output_tokens", "cache_read_input_tokens", "cache_creation_input_tokens", "model"}, "answer"}` on stdin and records `Stopped { agent_id, status, usage, answer }`. Idempotent per dispatch: a second `stopped` acknowledges and changes nothing. A stop before `bound` is kept, and the `bound` that follows finds it. A dispatch bound to another agent refuses the report; an unknown dispatch is answered non-zero.
- `relais native status [--run <run>]` prints one JSON object read from `<artifacts>/<run>/events.jsonl` (the run whose events were written last when no id is given): `phases`, `decisions`, `cost`, `outcome` and at most the last 40 lines of check `output`; never the whole file.

The registry keeps its hook-side states (`Claimed`, `TreeGiven`) for #171's path until it is removed; this flow uses `Requested` → `Bound` → `Stopped`. The calls above made `coordinator::PROTOCOL_VERSION` 5.


Two limits of this path follow from what a hook payload does and does not carry, and are stated here rather than implied away. **Depth is enforced only where a caller resolves**: as of Claude Code 2.1.283, a nested spawn's own `PreToolUse` — one made from inside a subagent — carries `agent_id`, naming the caller (`hook::event::AgentToolCall::caller_agent_id`; see `tests/fixtures/hooks/README.md`). `DispatchRequest::caller_agent_id` carries that agent id on the wire, and `AdmissionState::resolve_caller` turns it into `parent_dispatch` — the dispatch this coordinator holds bound to that agent, in that session — before `effective_depth` runs at all, so a resolved caller's depth is one deeper than its own, exactly like a managed dispatch's explicit `parent_dispatch`. No second depth calculation exists for this path: `resolve_caller` is the only place a caller id becomes a parent, and `effective_depth` is unaware that a parent it is handed ever came from one. A top-level spawn names no caller and is never given a fabricated one, so it stays at depth 0; a caller this coordinator holds no binding for (never bound, or its lease lapsed and was reaped) resolves to no parent the same way and is admitted rather than refused for it — missing knowledge, not a violation. **No money limit is enforced**: the run the hook registers carries no budget, and `check_hard_limits` refuses on budget only for a run that has one, so `dispatch_reserve_micros` above zero changes what a spawn reserves against an unbounded total and refuses nothing. A hook-admitted session is capped by agent count and, where a caller resolves, depth — never by spend.

Every `DispatchRequest` names a `DispatchSource` — `HookAdmitted` for this path, `ManagedRun` for `relais run`'s own dispatch, `Observed` for a dispatch this coordinator only ever watched — a field the caller that builds the request already knows, never inferred afterward from what the dispatch looks like. `StatusSnapshot` carries live and admitted counts broken down by source, plus how many hook-admitted leases are past `agent_lease_ttl` and not yet reaped, and `admission::enforcement_line` renders the one sentence `relais coordinator status` prints and `relais report`'s `enforcement` key carries as the same structured data: what IS capped for a hook-admitted spawn (agent count per session and per run, the seat held from `PostToolUse` bind until `SubagentStop` or its lease, and depth through a resolved caller) and what is still NOT (spend, and depth for a top-level or unresolvable caller, for the reasons stated in the paragraph above). `Enforcement` itself gained `Observed` — no hook wired, or the coordinator unreachable with `carry_on` in force — alongside `InProcess` and `Coordinator`, so the sentence never claims a guarantee nothing is holding.

`caller_agent_id` is additive and `#[serde(default)]`, not a `PROTOCOL_VERSION` bump: an older relais's request has no such key and deserializes with `None`, and neither `DispatchRequest` nor the `Request` wire enum denies unknown fields inside it (`Request`'s `deny_unknown_fields` covers its own variants' fields, not a struct one of them merely embeds), so a newer request's extra key is simply unread by an older daemon. Both directions of that mixed-version pair are exercised directly, with no live daemon of either version, in `admission::tests::a_mixed_version_pair_of_dispatch_requests_stays_wire_compatible`. Upgrading past this change needs no `relais coordinator stop`.

A cancellation on this path holds only while the coordinator still remembers the run. `relais cancel` marks it terminal; the next reconcile — `RECONCILE_EVERY`, 15 seconds — reaps a terminal run with nothing outstanding, and the spawn after that finds no record, registers the run again and is admitted. The cancellation lives in coordinator memory and nothing on disk distinguishes a reaped-cancelled run from a session never seen, so a hook cannot refuse on a cancellation it can no longer read. Cancelling a tab's agents is not a durable stop.

The CLI command wraps `hook::respond::handle` in one more `catch_unwind` and always returns exit 0: a non-zero exit from a hook is reported to the session as a failure of the tool call it was watching, so a relais that cannot answer — an unreadable machine.toml, an unreachable coordinator, a payload that is not JSON, a panic anywhere in between — must be indistinguishable from a relais that had nothing to say. A missing or invalid machine.toml is not surfaced either; `HookAdmissionSettings::default()` applies instead, and `relais doctor` is where a settings problem is reported as a finding.

Every firing is journalled as one JSON line — the raw payload (whatever arrived, including fields this crate does not model, since a payload names a person's transcript path and working directory), the classified event, whatever the coordinator answered, and the decision, its reason, and, on a refusal, the `rule` and `availability` off `hook::decide::Refusal` — appended to `hook_journal.jsonl` under the state directory (`paths::hook_journal_path`), which `hook::respond::append_journal` creates and keeps at mode 0600 so it is never group- or world-readable. `rule` and `availability` are `null` on a `Silent` answer and always present together on a `Refuse`, so a reader can group firings by which rule fired and tell a session that hit its limit from a machine whose coordinator was unreachable, without parsing the rendered `reason` sentence back apart. Journalling is best effort, like everything else in this path: a write failure there is swallowed rather than turning an answered hook into a failed one.

### Resource policy

Use separate limits for active remote model work, local heavy commands, indexing and training. A lightweight remote research agent does not consume the same resource class as a compiler or test container. Parent processes waiting on children retain identity but relinquish active-execution capacity where waiting can be reliably observed; avoid a deadlock in which all slots are held by parents awaiting queued children. Otherwise reserve child capacity before starting a delegation-capable parent, or decline that decomposition explicitly.

Configuration supports global, per-session and per-run limits, plus maximum nesting depth, total invocations and aggregate spend. No single child or new work package resets the root budget. Use fair scheduling across sessions with queue aging and configurable interactive priority; a tab spawning many children cannot starve the other tabs.

Illustrative machine-owned configuration, not benchmark-derived sizing:

```toml
[concurrency]
max_active_agents = 6
max_active_agents_per_session = 3
max_heavy_commands = 2
max_training_jobs = 1
max_agent_depth = 3
max_agents_per_run = 24
training_when_idle = true
```

Interactive root sessions outside managed execution are not automatically constrained by these limits. Show which workload the cap covers. Known unmanaged work may reduce admission headroom, but unknown machine activity prevents claims of complete host utilization control.

On the user's Intel MacBook Pro, expose configurable CPU threads and memory thresholds for local jobs. Training yields to interactive work. No fixed RAM or speed claim is made without measurement. The shipped macOS target includes x86_64; multiple tabs do not load separate training services or duplicate the full model registry.

### Shared repository safety

Use distinct owned worktrees for independently writing runs. Read-only agents may share an immutable input snapshot. Within a run, concurrent writers need separate worktrees or explicitly non-overlapping write leases; scope checks alone are not filesystem isolation.

Root verification waits for all relevant write leases to be released and verifies the integrated candidate. Agent completion does not imply all descendants have stopped writing. Other tabs cannot invalidate a receipt silently: a changed candidate has a different identity and requires verification again. Shared Git metadata operations, integration and cleanup are serialized where required. Do not remove another run's branch, worktree or unexported work.

### Budgets, usage and learning

Reserve from the root budget before parallel dispatch so children cannot independently spend the same remaining allowance. In-flight reporting still makes exact financial caps best effort. Settle reservations with actual usage or mark them uncertain until reconciliation; unknown usage is not zero.

Record exclusive agent costs and aggregate tree totals separately, using stable event/request IDs. Never sum an inclusive parent total with its descendants. Attribute review, integration, retries and training trials correctly. Native agents without a bounded contract may contribute observational data, but cannot become supervised success labels without matching acceptance evidence.

Pin a routing artifact per run. A newly trained model may be used by new runs while existing trees retain their original policy. Feature collection includes execution role, depth, relevant contention and parent-provided context at dispatch, without treating sibling outcomes discovered later as initial features.

### Recovery and cancellation

Leases use heartbeats and reconciliation. Lease expiry does not prove a worker died: check live processes/harness status before admitting a duplicate or freeing exclusive write access. Distinguish cancelling one agent subtree, one run and one session. Never cancel unrelated tabs. Reconcile orphaned descendants after coordinator or parent failure and preserve their output before cleanup.

### Required concurrency acceptance tests

- Three simultaneous Claude Code sessions can each request multiple agents; managed work observes global/per-session limits and progresses fairly.
- Nested and resumed agents retain correct parentage and root-budget attribution without duplicate usage or dispatch.
- Parents waiting for children cannot exhaust all execution slots and deadlock the scheduler.
- Simultaneous dispatches cannot reserve the same remaining budget twice; missing terminal usage remains uncertain.
- Concurrent writers are isolated and the assembled candidate is not verified while a descendant still holds write access.
- Coordinator restart, lost hooks and duplicate lifecycle events reconcile without duplicate live workers or premature worktree cleanup.
- Cancelling one subtree leaves other runs and tabs operational.
- Unsupported native admission paths are visibly observed-only; strict guarantees are limited to managed paths.
- Concurrent training cannot change a running task's pinned routing artifact or block normal ledger writes for the duration of fitting.

Compatibility references: Claude Code hook semantics and subagent depth/concurrency controls. These establish adapter constraints, not evidence that Relais has been implemented or tested.

## 24. Task replay

`relais dataset replay --task <id> --recipe <candidate.toml>` re-runs a task's already-accepted run under a candidate recipe, so the candidate has to earn its own result rather than borrow the incumbent's. This is NOT a promotion mechanism and does not compare, score, rank or promote anything: it produces exactly one arm's result for one task. A single replay, or a small deliberately-run set of them, is not evidence that a recipe should be promoted — that judgment, if it is ever made, is a later mechanism's job, not this command's.

### Admission

A candidate is admitted through the same door a learner's own proposal is: `route::validate_candidate` (§17), against `TuningBounds` derived from the repository's own current policy — never from the candidate, and never from a second, replay-specific configuration file that could drift from the boundary a learner's proposal already has to clear. A candidate `validate_candidate` refuses is refused here with that same rejection; replay is not a second door into an unvalidated policy.

The source run must be `Accepted` — the runner's own verification settled it, not a person's salvage — because only that run wrote a receipt with a verification profile hash to check a replay's comparability against. Replay additionally refuses when the verification profile that would judge the replay differs, by content hash, from the one recorded on the source run's receipt: a comparison judged by different checks is not a comparison, even though the candidate's own `verification` field cannot itself move (it is one of the fixed fields `validate_candidate` never lets a candidate touch) — the check exists for the case where the repository's policy, not the candidate, moved between the source run and the replay.

### Workspace isolation

The replay workspace MUST NOT contain the accepted answer. It is built as a fresh, single-commit checkout at the source run's recorded base SHA — `git init`, a shallow `git fetch` of exactly that one commit, then `git checkout --detach FETCH_HEAD` so the working tree is actually the source repository's tree at that commit — never a `git worktree add` of the live repository. A linked worktree shares the live repository's object store, where the accepted candidate's own commit (reachable from later history, past the base) is one `git show` away; a fresh checkout's object store holds nothing but the base commit and its own history, so the accepted answer is not merely absent from the working tree, it was never fetched. From that checkout, the rest of a run's ordinary machinery — routing, dispatch, verification, snapshotting — proceeds exactly as `relais run` takes it, needing no replay-specific path through it. Without the checkout step, anything that reads the working tree before dispatching — preflight resolving architecture decisions from a tracked `.adr.yaml`, for one — finds nothing, and a repository requiring that decision blocks every replay at $0.

### Spend and ceilings

A replay dispatches a real worker and spends real money: usage is recorded as ordinary `CostKind::ApiSpend`, under the same admission gate, machine limits and per-run ceilings every other run goes through — there is no separate cost kind or bypass that would let a replay's spend escape a ceiling ordinary work is subject to. `--dry-run` reports what would run (the source run, the candidate's covering recipe, the verification profile hash checked) and spends nothing: no worker is dispatched, no trial row is written, no usage is recorded.

### Record

A completed replay writes exactly one `trials` row (§17): `assignment_probability` is `1.0`, because nothing was sampled — a replay is a deliberate re-run, not a draw among several arms — and `workspace_isolation` records `fresh_checkout_no_accepted_answer`, distinct from whatever an ordinary or sampled trial's workspace records. The row is settled once, through the same settle-once path (`Ledger::settle_trial`) every other trial's outcome is. The run itself carries `RunPurpose::Replay`, distinguishing it on the ledger from ordinary work.

## 25. Recipe comparison

`relais recipe evaluate <candidate.toml>` reads the settled trials for a candidate recipe and reports a comparison. It is a reader, not an actor: nothing in this command promotes a recipe, writes a policy, or issues a trust grant. `relais promote` — which activates a learned artifact — is the only thing in this product that activates anything, and it takes an artifact id, not a recipe.

The candidate is admitted through the same door a replay's candidate is (§24): `route::validate_candidate`, against the repository's own current policy. A candidate's ARM ids are its recipe ids MINUS those of the repository's current policy (the incumbent): a candidate is that policy plus appended revisions and a recipe id is a content hash, so the base recipes' ids are shared and name no change. One function (`candidate_arm_ids`) defines them, and both trial selectors — replay pairing below and the randomized estimator — use it. "The trials for this candidate" are the settled trials whose arm is one of those ids; a candidate is a whole policy and different tasks may cover under different recipes within it, so the comparison is keyed on that set, not on a single recipe id.

### Pairing

The unit of comparison is the task, not the run. Each matched trial is one task's candidate arm; it is paired against the incumbent result that task's source run already earned — the accepted run a replay re-ran. A task replayed more than once still contributes exactly one paired observation (the earliest settled arm), so a task with more runs never outweighs one with a single run.

An `Errored` trial — the runner's own infrastructure fault, never a verification verdict — is "not comparable evidence for or against the arm" and never becomes that observation: a task's pairing skips it and uses its earliest settled NON-errored trial instead, so an errored trial followed by a real one pairs on the real one. A task whose only settled trials errored contributes no pair at all. These trials are set aside, not silently dropped: the report counts how many were, and prints that count alongside the pairing it made from what was left.

### What is reported

- **Per-arm acceptance**, an inverse-propensity-weighted estimate over each arm's observations. A replay's assignment probability is always `1.0`, so this collapses to a plain mean; a future randomized trial's recorded probability would not.
- **Cost per accepted change**, the mean `TrialCost` figure over the accepted observations whose cost is known. A cost that was never reported is never folded into that mean as zero (§11); the report separately counts how many accepted observations carried a known figure and how many did not, so an unknown does not vanish from view.
- **A paired bootstrap interval** over the candidate-minus-incumbent acceptance difference, resampling whole tasks (never splitting a pair across resamples) with replacement. The seed is a deterministic hash of the sorted paired task ids — never the platform's own randomness and never dependent on read order — and is carried in the report, so the same trials produce the same interval on any machine.

### The comparison basis

`ComparisonBasis` names what the comparison rests on: `Replay` (what `relais dataset replay` produces), `Randomized` (relais's own random assignment among eligible arms — not produced by anything shipped yet), or `Observational` (data that was never assigned at all). An observational basis can never promote, whatever the numbers say, because it is not a comparison; the report's rendered output names that gate explicitly rather than burying it in a footnote.

### Off-policy abstention

An arm's acceptance estimate abstains — names what is missing, produces no number — rather than guessing when it has no support: no trials matched it, or every trial's assignment probability is zero or non-finite and so cannot carry an inverse-propensity estimate (§17: "do not infer performance for profiles with zero observation probability"). An abstained estimate is itself a failed promotion gate.

A replay trial counts toward a candidate only when its `arm_recipe_id` is an arm id; a replay whose arm was an unchanged incumbent recipe did not change that task's route, so it is excluded from pairing and counted in the report (`replay trials on an unchanged incumbent recipe set aside: N`, `incumbent_replay_trials_set_aside` in JSON, additive), never silently dropped.

### The randomized estimator

Live trials (§28) are estimated separately, without pairing. WHICH ROWS: a settled trial with `workspace_isolation = "live_worktree"` counts when its `arms_json` — parsed as the JSON list it is, never substring-matched — names one of the candidate's ARM ids among its non-control arms (`arms[1..]`), i.e. the candidate was among the arms at draw time. Matching the incumbent's shared ids instead would count every control row. That includes CONTROL rows (`arm_index` 0, whose own `arm_recipe_id` is the incumbent's); a control row drawn without the candidate never counts. The row reader is `Ledger::settled_live_trials_for_arms`; a malformed `arms_json` is a corrupt ledger row. As for replays, a task keeps one observation per arm (its earliest settled non-errored trial) and errored live trials are set aside and counted.

ESTIMATOR: per arm (control, and the candidate's arm) acceptance and cost per accepted change are Hajek inverse-propensity estimates, `Σ(w·y)/Σ(w)` with `w = 1/assignment_probability` of each row. An unknown cost is never folded in as zero; known and unknown are counted separately. An arm with no rows, or none whose weight is finite and positive, abstains through `AbstentionReason`.

INTERVAL: a bootstrap over the candidate-minus-control acceptance difference that resamples tasks WITH replacement WITHIN each arm independently (unpaired: no row is ever paired with one of the other arm), each resample re-estimated with the same weights. The seed is a deterministic hash of the sorted trial ids and is carried in the report, so the same trials give the same interval.

The report keeps its replay section unchanged and gains `randomized` (additive in JSON), rendered as its own block headed `basis: randomized, control n=…, candidate n=…`. It replaces the earlier live-trial set-aside line.

### The verdict

Promotion is gated, and the gate is unspellable to defeat: `ComparisonReport::promotable` is the only way to obtain a `Promotable` value, which has a private field and no public constructor, and it hands one back only when `ComparisonReport::failures` — RECOMPUTED from the report's own stored figures on every call, never read from a cached verdict — is empty. Editing a stored report's figures changes what `failures` returns; nothing about the report can disagree with itself and still say it passed.

The gates: the basis must not be `Observational`; at least 20 paired tasks (below that, a comparison is a proof of mechanism, not evidence about a recipe — the first intended real use, a three-task replay, is exactly this case and must refuse); and neither arm's acceptance estimate may be abstained. A refused report's rendered output states its basis and paired count together (`basis: replay, n=3`), names every unmet gate in words, and contains no sentence — no "PASSED", no approval — a reader could mistake for a weak pass.

A randomized section has its own gates: each arm has at least 20 observations (`MIN_PAIRED_TASKS`), neither arm abstains, and the minimum recorded assignment probability is above zero. `failures` is still recomputed from the stored figures on every call; `promotable` hands out a `Promotable` when the replay section has no failure OR a randomized section exists and has none (the `Promotable` constructor stays private). A report that promotes on randomized evidence says so: `gates: PASSED (basis: randomized)`, beside the randomized block. `relais recipe promote` needs nothing beyond what `promotable` now allows.

## 26. Recipe inspection

Three read-only commands let a person see what a policy declares before anything is replayed or evaluated. None of the three grants, replays, evaluates or writes anything.

`relais recipe list` prints every recipe the repository's `relais.toml` declares — name, revision, enabled, tier, kind, scope and `recipe_id` — one per line, in declaration order. A policy that declares no recipes says so in words; empty output would be indistinguishable from a failure to read.

`relais recipe show <name>` prints every revision of that recipe, including the per-recipe `models`, `execution`, `context` and `review` blocks when set. The `models` block is read by routing (§6, "The rung"); `execution`, `context` and `review` carry the caveat their field docs do: DECLARED AND HASHED, NOT YET READ by routing. An unknown name is a refusal naming the recipes that do exist, not an empty success.

`relais recipe diff <candidate.toml>` compares the candidate's recipes to the repository's own, per recipe and revision, and names every field that differs, using each side's EFFECTIVE value rather than its compact serialized form — a field a candidate leaves at its default, and so omits from the file, compares as that default, never as an absent `null`. The set of compared fields is still derived from `RecipeSpec`'s own shape rather than a hand-picked field list, so a field added to the type later appears in the diff without anyone remembering to add it. It also states whether `route::validate_candidate` would admit the candidate — quoting the rejection when it would not — and says in the same breath that admissible is not approved: an admitted candidate still needs its own trust grant before anything runs, evaluates or replays under it (§24, §25).

`recipe_id` is always read from `RecipeSpec::recipe_id`, never recomputed by these commands, so the id `list` and `show` print is the id `replay` and `evaluate` record.

## 27. Recipe promotion and rollback

Two commands change which recipe revisions a repository's `relais.toml` declares. Both are human-gated, append-only, never grant, and never run anything: no worker is dispatched, no replay is started, no trial is written, and `machine.toml` is never opened.

`relais recipe promote <candidate.toml> [--write]` admits the candidate through `route::validate_candidate` exactly as `recipe diff`, `dataset replay` and `recipe evaluate` do, then RECOMPUTES the comparison with `learn::comparison::evaluate_candidate` from the ledger's settled trials — it never reads a verdict a previous `evaluate` printed. It proceeds only when `ComparisonReport::promotable` hands back a `Promotable`. A refusal lists the report's own failed gates (§25), states the basis and paired count (`basis: replay, n=3`), writes nothing, and exits 17 (`PromotionRefused`), distinct from a candidate `validate_candidate` refuses (3). `Promotable` keeps its private constructor: the only path from a report to a write is `Amendment::for_promotion`, which takes one by value.

Without `--write`, a promotable candidate prints the `[[recipes]]` fragment(s) it adds — the new revisions only — the evaluation it rests on (basis, n, per-arm acceptance and cost, the paired interval), and the trust block for the policy that would result, in the shape `relais plan` prints for a missing grant. Nothing is written.

`relais recipe rollback <name> [--write]` restores the revision below the currently effective one — the effective one being the highest enabled revision, as routing selects it (§6) — by appending a NEW revision N+1 whose fields equal revision N-1's, enabled. It never edits or disables an existing revision: recipe history in a policy is append-only, as `validate_candidate` already enforces for a candidate. Removing a change is always allowed, so rollback needs no evaluation. A recipe with fewer than two revisions, one with no enabled revision, or an unknown name is refused, naming what exists (exit 2). Without `--write` it prints the fragment and the resulting trust block.

`--write` APPENDS the fragment(s) to `relais.toml` as text and never re-serializes the file: every existing byte, comment and ordering is kept, so the file before is a byte-exact prefix of the file after. Before appending, the text is parsed back and must equal the policy the amendment means to produce; a file the fragment cannot be appended to is refused, not corrupted. The new policy has a new authority hash (§5), so no existing grant covers it: the next `relais plan` reports `missing_trust_grant` until a person reviews the printed block and pastes it into `machine.toml`. Neither command issues that grant.

## 28. Live trial assignment

`[trials]` in `machine.toml` lets `relais run` draw, for an eligible task, one arm of a randomized comparison — the incumbent (control) or one candidate recipe policy — with a recorded probability, run it, and settle it. Randomized evidence then accrues from ordinary work, inside hard daily caps. It is OFF by default and BYTE-IDENTICAL when off: with no `[trials]` block, or `enabled = false`, `relais plan` prints what it always printed, `relais run` says nothing about trials, no candidate file is read, and no `trials` row is written.

**Machine-owned.** The envelope lives in `MachineSettings`, never in `relais.toml`, so a repository can neither enable trials nor widen them. Every field defaults to running nothing: `seed` absent, `eligible_kinds` empty (no task is eligible), `candidates` empty, `max_daily_trials = 0`, `max_trial_cost_micros = 0`. `enabled = true` with no `seed` makes every task not eligible — an unseeded draw is never made — and `relais doctor` reports it as a failure. A machine.toml that sets only the original three fields (`enabled`, `max_daily_trials`, `max_trial_cost_micros`) still parses.

**The draw.** `route::trial::assign_trial` is pure. It draws with `SplitMix64` seeded by a stable hash (SHA-256) of the machine seed, the task id and the UTC day, uniformly over {control} ∪ the admitted candidates, so the recorded probability is `1/(1+k)`, the same inputs always give the same answer, and a recorded seed reproduces the assignment. A task is not eligible, with a typed reason that is printed, when: the envelope is disabled, unseeded, or the task's kind is not in `eligible_kinds`; the day's trial count or spend has reached its cap; or no candidate is admitted.

**Admission is at run time, never trusted.** Each candidate path is parsed, admitted through `route::validate_candidate` against the repository's CURRENT policy (so tier floors, risk rules and review are untouched by construction — a candidate that lowers review is dropped), and must hold a trust grant in `machine.toml` for its own authority hash (§5). A candidate failing any of those, or covering the task with the incumbent's own recipe or another arm's, is dropped from the arm set, and `plan` and `run` print which one and why. An ungranted or inadmissible policy never runs.

**Caps are counted from the ledger**, never from process memory: the trials created since 00:00 UTC, and their recorded cost, where a trial whose cost is unknown — in flight, or settled with unknown usage — counts as the full `max_trial_cost_micros`, never zero. Either cap reached makes the next task not eligible.

**Recording.** `relais plan` prints the assignment it WOULD make (`trial: control (p=0.50, arms=2)`, `trial: candidate <path> (p=0.50)`, `trial: not eligible (<reason>)`) and writes nothing, not even a ledger that does not exist yet. `relais run` on an eligible task writes exactly one `trials` row at assignment — for control too, with `arm_index` 0 and the incumbent's recipe id — carrying the assignment probability, the seed, the arm list (`arms_json`, ledger step v16) and `workspace_isolation = "live_worktree"`. A run assigned a candidate executes under that candidate's policy. The row is settled once, at the run's terminal state, through `Ledger::settle_trial`, by the same mapping `dataset replay` uses: accepted, rejected, or errored. A live row's `source_run_id` names the arm's OWN run — the run it executed as, whose `runs.purpose` is `trial_arm` (control included) — never an empty placeholder. That run is not a replay source, which is why `recipe evaluate` does not pair live rows: it estimates them without pairing (§25).

**Evaluation.** `relais recipe evaluate` pairs each replay trial with a replay SOURCE run (§25), which a live trial does not have. Live trials take the randomized estimator instead (§25): rows selected by `arms_json` membership, control included, Hajek inverse-propensity estimates per arm, an unpaired bootstrap, and its own gates. A ledger holding only replays evaluates as before.

## 29. The protocol channel and the run's events

Nothing a run does is out of sight. Every step is an **event**, appended as one JSON line to `<artifacts>/<run>/events.jsonl` whether or not anything is listening — a file, not a ledger table, so there is no migration — and, under `relais run --protocol`, also written to stdout.

**The channel.** `--protocol` keeps the real stdout for protocol lines only. At startup, before anything prints, relais duplicates fd 1 for its one protocol writer (a mutex around the kept stdout, so lines never interleave), then points fd 1 and fd 2 at a pipe. A reader thread copies each line of that pipe to the real stderr, which relais also keeps by duplication, and, once the run's artifacts directory is known, appends it to `events.jsonl` as a `stderr` event (lines before that are only copied). Every `println!`, every `eprintln!` and a panic's message therefore reach stderr and never stdout, and a panic is in the run's record. Each `stderr` event carries at most 4 KB of one line. At exit the descriptors are restored and the pipe drained, for a bounded time. Without `--protocol` nothing changes: stdout and stderr are as they were, and `events.jsonl` is still written. On Windows `--protocol` is refused before anything runs, saying it is not supported there yet (exit 2).

**The line.** Every protocol line is a JSON object with a `relais` key naming its kind. This section defines `event`:

```
{"relais":"event","run":"<run id>","seq":0,"at":"<RFC 3339>","event":{"kind":"…", …}}
```

`seq` starts at 0 for each run and rises by one per event, in `events.jsonl` and on stdout alike, so the two hold the same events in the same order. The `stderr` events are the one exception: they exist in the file only (a reader of the channel has the child's stderr itself), carry no `seq`, and are numbered apart by `stderr_seq`, so a mirrored line never takes a number the channel would skip. Field names are `snake_case`.

**The event kinds.**

| `kind` | fields | emitted |
|---|---|---|
| `phase` | `state`, `reason`, `detail` | by every transition the runner records, with the ledger's own values |
| `dispatch_started` | `dispatch`, `agent_kind` (`worker`, `reviewer`, `planner`), `attempt` (a worker's number, else `null`), `model`, `effort` | before a managed launch |
| `dispatch_ended` | `dispatch`, `outcome`, `usage`, `cost` | after it; `usage` and `cost` are `null` when the dispatch never ran |
| `cost` | `booked` (micro-USD, `null` when unknown), `completeness` | after each usage event is recorded |
| `check_started` | `label`, `argv` | before a verification or setup command |
| `output` | `label`, `text`, `elided_bytes` | while it runs |
| `check_ended` | `label`, `exit` (`null` when it timed out or was killed), `duration_ms` | after it |
| `decision` | `what` (`repair`, `escalate`, `accept`, `stop`, …), `reason` | where the machine picks the next step; the `phase` of the same decision follows |
| `outcome` | `state`, `receipt` (the path, when accepted) | at the end of each run |
| `stderr` | `text` | for each line the process wrote under `--protocol`; file only |

**The requests.** Besides events, a run under `--protocol` writes requests to the relais plugin through the same writer, each one JSON line with a `relais` key:

| `relais` | fields | written |
|---|---|---|
| `spawn` | `run`, `dispatch`, `agent_kind` (`worker`), `subagent_type` (`relais:` + the shipped agent's name, `native::worker_agent_type`), `model`, `description` (`relais <dispatch>`), `prompt`, `cwd` (the attempt's worktree) | a worker attempt on a fresh agent: the first attempt, or an escalation to another model |
| `continue` | `run`, `dispatch`, `agent`, `message` | a repair on the same model: the same agent, a new dispatch |
| `stop` | `run`, `dispatch`, `agent` | the attempt is cancelled while its agent runs |
| `done` | `run`, `outcome` (the run's end state), `receipt` (the path, or `null`), `summary` (`files_changed`, `insertions`, `deletions` of the last candidate patch, or `null` when the run kept none) | once, as the last line of a root run |

The plugin answers through `relais native` (§23): `hello`, `bound`, `stopped` and `status`. Under `--protocol` every stdout line of a whole run, accepted or not, is one of these objects or an `event`, and the last is `done`; a package run's child runs write events but no `done`.

**Streaming check output.** A command's stdout and stderr go to its log file, which stays the record. While it runs, a follower reads the growing file with a descriptor of its own: it never touches the command's pipes, so it cannot slow or block the check. New bytes are merged every 100 ms into `output` events of at most 4 KB of text each. A check puts at most 64 KB of output into events in all; the rest is skipped, counted, and reported in one final `output` event with empty `text` and `elided_bytes` set. When the follower falls behind it skips ahead and counts what it skipped. It stops, after one last read, when the command ends. The log file holds all of the output, and a check is no slower with the follower than without it.

## 30. Session routing

The relais plugin routes the model of the person's own Claude Code session (plan `docs/plans/2026-10-07-session-router.md`; the wire contract between the plugin and relais is `docs/router-protocol.md`; decision record `.adr/records/0001-relais-routes-the-session-model.md`). It is advisory (§3) and fails open (§23): any error, timeout or unreadable answer leaves a request as it was, or keeps the task's current tier, and never moves it down. relais's own supervised workers are never routed (§6).

**The task.** The plugin keeps a small summary of the work in progress. A classifier call (haiku, low effort, one per prompt the person types; task notifications and relais's own prompts are never classified) reads the prompt against that summary and returns the relation (`new_task`, `continuation`, `correction`), the kind, the difficulty (1–5), the scope, the uncertainty, whether the work is verifiable, any explicit acceptance or correction, a model the person named, and a confidence. A continuation inherits the task's classification; only its difficulty may rise.

**Initial routing.** A capability table, versioned and served by `relais native router-state`, maps the classification to a tier: escalation for difficulty ≥ 4 or high uncertainty beyond `local`; research for difficulty ≤ 2 with low uncertainty when the task is verifiable or a question; implementation otherwise. A model cheaper than the session's own is chosen only through the research rule (or, later, a learned adjustment), never on confidence alone; below 0.6 confidence a new task keeps the session's tier.

**Recovery.** A failed check after an edit in the task (a test, lint or build command: the repository's profile commands and a built-in list), two failed repairs in a row, or a correction raises the task's tier by one, from the next request, at most twice; then the effort rises. Nothing moves a task down. A growing scope or a new subagent re-runs the table, which can only raise the tier. The person never has to say "try again".

**Cache-aware switching.** A switch down at a task start happens only when the predicted saving (the median tokens of completed tasks of the same kind and difficulty, else a fixed prior of 20k, 60k, 150k, 400k or 800k tokens, times the price difference) exceeds the cost of rewriting the context into the new model's cache at the 1-hour write rate. An unknown price blocks a switch down; a switch up always passes.

**Subagents.** The router decides a subagent's model through the same classifier and table, overriding the parent's `model` parameter within the envelope. A model the person named in the prompt, a model pinned by an agent definition the person maintains (`~/.claude/agents/*.md`, `.claude/agents/*.md`, read by relais) or `[session_routing] pinned_agents` is never overridden. A subagent's failed check escalates that subagent only.

**Outcomes and evidence.** A task ends `completed_verified` (its last check passed, or a relais run in it was accepted), `completed_accepted` (the person said so), `corrected` (the person corrected or flagged it), or `unknown`. Inferred signals (an abort, no complaint, a respawn) are diagnostics, never outcomes. A task record sent again replaces the stored outcome only with a higher-ranked one: `corrected` > `completed_*` > `unknown`. `relais native router-observe` writes each prompt's batch of decisions, per-request usage (tokens and model, never a cost), reassessments and tasks in one transaction, idempotently, into the ledger's router tables (step v20); a bad payload exits 2 and writes nothing. These usage rows are the router's own accounting and are never added to `orchestration_usage` totals.

**Authority.** `[session_routing]` in machine.toml holds the envelope (`granted_at`, `by`, `epsilon_max`, `source`), the hold-out rate (0.1), the exploration rate (0.1, never above `epsilon_max`, 0 without an envelope), the alias table (`model_ids`: `turn.step` needs full ids), `pinned_agents`, `excluded_models` and the learning floor. The mode is `on` whenever the envelope is recorded; `RELAIS_SESSION_ROUTING=off|shadow` can only narrow it; without an envelope it is `shadow` (decide and record, switch nothing). The plugin follows that mode. The envelope is recorded once, without a further question: by `relais install --claude --write` when it installs the plugin (installing is the authorization; source `install`), or by `/relais-routing` after the plugin asks through `$.ui.ask` (source `plugin-ask`). `/relais-routing off` removes it and records an `off` tombstone, which keeps a later install from recording it again. `relais native router-envelope` writes both with the same locked, atomic, comment- and mode-keeping writer as `trust grant`, and records every write with its source (`install`, `plugin-ask`, or `cli (unattributed)`). There is no hand-labelled evaluation: the classifier's quality is judged from automatic outcomes, and missed failures are measured from later corrections and reverts.

**The bypass limit.** Every session runs with permission prompts bypassed, so the model can do anything the person's shell can. The plugin's `tool.check` guards refuse its writes to machine.toml and its Bash calls of `router-envelope` and `relais install --claude`. That stops an accidental or "helpful" self-grant. A deliberate edit of machine.toml from the shell is not stopped: that is the accepted limit, the same one §5 states for the trust grant.

**Measurement.** 10% of sessions are held out, drawn by `SplitMix64` seeded from the session id: they decide and record and apply nothing. `relais report` prints a "session routing" section: the total cost per completed task in routed against held-out (and shadow) sessions, priced from `[pricing]` at read time with the cache rebuild at the 1-hour write rate, a 95% cluster-bootstrap interval over sessions, counts, the unknown-outcome rate, the classifier's own cost and the cache tokens rewritten after a switch — all labelled an API-equivalent estimate, and an unpriced model shown as unknown, never zero (§11). Learned adjustments (seeded exploration, the activation gates, `router evaluate` and `rollback`) come later and activate only inside the envelope.

---

Companion repositories: [amont](https://github.com/fredericrous/amont), [aval](https://github.com/fredericrous/aval), [amont-agent](https://github.com/fredericrous/amont-agent).
