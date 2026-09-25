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

Relais supports one repository per run, with bounded independent work packages when decomposition is justified. It includes an owned training pipeline, a native Rust learned router, reusable context, controlled routing trials and backend adapters. It excludes unrestricted agent teams, automatic publishing, deployment, cross-repository writes, a transparent model proxy and general-purpose chat memory.

## 3. User experience and integration modes

### Native assistance

`relais install --claude` proposes a small namespaced set of `.claude/agents/` definitions and a `/relais` skill. Installation is preview-first; `--write` applies reviewed changes, merging configuration without replacing unrelated entries. User-level installation is explicit, not the default. Uninstall removes only owned, unchanged artifacts.

`relais install --claude --hooks` additionally wires the live hook (§23) into `.claude/settings.json`, on each of the seven targets named in the hook compatibility matrix below. `settings.json` is a file a person maintains by hand, so this is a separate, explicit ask, never implied by `--claude` alone: without `--hooks`, install reads and writes nothing under that name. Every write goes through a byte-for-byte round trip first — parse the file, re-render it with `serde_json` and compare — and a file that does not survive that trip is refused rather than rewritten: nothing is written, and the fragment to paste in by hand is printed instead, because editing a file that cannot be re-rendered losslessly would bury the one line relais wants to add inside a reformatting nobody asked for. The one tolerated difference is the trailing newline `serde_json`'s pretty printer does not emit and nearly every editor adds: a file whose only divergence is that newline is accepted, and is written back with the tail it arrived with, so the tolerance never becomes a change relais makes. Trailing blank lines, a `\r\n` tail or trailing spaces are refused like any other file relais cannot reproduce.

A handler somebody else already has on one of these events is joined rather than displaced when it sits on an entry whose matcher is the one relais installs; an entry on a different matcher gets its own, because a matcher states which tool calls a handler wants to see, and widening somebody else's is not relais's to do. Nothing at that granularity is ever deleted on uninstall except the leaf command relais itself added, because relais cannot tell an entry it left empty apart from one that was already empty when it arrived.

Native agent definitions provide convenient defaults for research, implementation and review. Native delegation remains the parent model's choice. Relais MUST label this mode advisory: it cannot guarantee a model, a dollar limit, an attempt limit, or acceptance for arbitrary work in the parent session.

### Supervised execution

The `/relais` skill asks the parent to express the requested work as a task contract and invoke the Relais runner. The runner launches separate programmatic Claude Code sessions with explicit model, effort, tools and limits. It returns a compact report plus artifact paths.

Once a task is accepted for execution, the parent does not supervise intermediate turns. The runner performs verification and bounded escalation. The parent receives the result and remaining decisions only.

Conversational examples:

- `/relais Fix the JSON escaping defect in amont list output.`
- `/relais Investigate why this check is inactive; do not edit files.`
- `Use Relais for this implementation; preserve the approved design.`

CLI examples:

```
relais doctor
relais init
relais plan --task task.json
relais run --task task.json
relais status RUN_ID
relais explain RUN_ID
relais resume RUN_ID
relais report --since 2026-09-01
```

`plan` performs local preflight and explains the route without launching a model. `run` validates the contract and starts an execution. `resume` reconciles interrupted state; it never blindly repeats the last command.

Free-form prompting is a skill convenience. The runner accepts structured tasks; it does not invoke a classifier for every trivial request.

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

Required fields: version, kind, objective, acceptance, verification profile and base. A change also needs bounded write scope. Kinds are `inspect` and `change`. Unknown fields are rejected to catch misspelled controls. Inspection requires evidence criteria instead of a patch.

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

Repository commands, hooks and model launch configuration are executable authority. Relais requires a content-bound machine trust grant for a reviewed execution profile and relevant configuration. It delegates amont-specific trust to amont; it does not infer trust from repository ownership. Changed execution declarations invalidate the grant. Worker changes cannot update the frozen grant or commands during a run.

A verification profile may declare a setup step: the commands that install the tree's own dependencies inside a verification worktree before the profile's commands run. It is executable authority like the commands (an installer runs the repository's lifecycle scripts), hashed into the grant, and never inferred: relais may report that a lockfile is present and no setup is declared, and it never runs an installer that policy does not name.

Dependencies are required, optional or off. Missing required integration blocks execution. Optional gaps appear explicitly in the report; they are never reported as passed checks.

## 6. Routing algorithm

Eligibility, risk floors and acceptance are deterministic. Within the eligible profiles, a Relais-trained predictor estimates acceptance and total cost using task outcomes collected by Relais. No upstream pretrained router weights or scores are loaded. The model supplies a proposed task, not its own exemption from policy.

1. Validate permissions, contract, base revision, verification profile and remaining limits.
2. Apply risk floors from declared scope, relevant repository rules and task kind. Unclassified writes use the conservative configured route, not Haiku.
3. Prefer an explicitly configured deterministic recipe when it fully covers the task. Relais does not infer arbitrary shell recipes from prose.
4. Route mandatory high-risk work directly to its policy floor. For remaining tasks, extract pre-dispatch task features and select among eligible profiles using a validated Relais-trained model. When trained evidence is absent or insufficient, use the configured conservative baseline and collect outcomes for training.
5. Emit the selected profile, rule IDs, reasons, verification requirements and maximum permitted attempts.

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

The adapter capability-checks the installed Claude Code version and tests behavior against a documented compatibility matrix. It uses existing supported authentication and never extracts credentials. Missing permissions produce a `blocked` result; no permission-bypass flags are introduced.

Write scope is checked on the actual diff after each attempt. A scope violation cannot be accepted and may require a revised contract. This is an acceptance boundary, not a claim of filesystem isolation. Worktrees share machine authority and Git metadata; tools with Bash access are not a security sandbox. Strong confinement requires a separately configured OS/container sandbox, an optional execution backend with explicit capability reporting.

A worker cannot commit, merge, push or publish through the normal allowed tool profile. Relais records an immutable candidate snapshot, including added files, outside model control. Sensitive repository configuration changes are rejected unless explicitly within an approved contract; they never take effect in the current worker launch.

Every run delivers a named candidate — a ref under `refs/relais/candidates/<run>/` and the exported patch — with its base revision, and successful execution a verification receipt as well. The task worktree is released once everything tracked in it is exported: whatever no candidate of the run holds is named `…/final` and exported first, and only then does the directory go, ignored build output included; an interrupted run keeps its worktree until `relais resume --retire` establishes that nothing may still be writing it. Integration into the user's branch is an explicit later action. Relais never force-cleans a worktree containing unexported changes.

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

Preflight captures relevant baseline failures so pre-existing failures are visible. Existing failures are not automatically waived. A waiver must already be in policy or become an explicit contract revision.

A declared setup runs in every worktree the profile's commands run in — the base, the task worktree, each candidate's copy — before them, and stops at its first failure. Its logs are evidence, never checks: a setup that succeeded passed nothing. A setup that does not succeed in a worktree the run owns blocks the run before a worker is launched; at a candidate it is a verification gap, since no check ran. A baseline whose commands could not run at all (a program not found) has no verdict: the run is blocked before any dispatch, the base's logs are kept, nothing is cached, and the block names the remedy — the setup to declare, or, when one is declared and succeeded, the setup to look at rather than another install step.

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

Use an integer or decimal money representation, not floating-point accumulation. Distinguish API spend, usage-credit spend, estimated API-equivalent cost and subscription consumption. Do not convert token savings directly into subscription fee savings.

The runner enforces dispatch, attempt, turn and wall-time ceilings. API dollar controls are best effort across in-flight requests and delayed reporting; reserve a margin before dispatch and stop admitting work once exhausted. Report overshoot and unknown usage. Do not advertise an exact financial cap where the provider cannot guarantee it.

Primary outcome metric: all recorded cost, including failed runs, divided by accepted tasks in the same cohort. Also report acceptance rate, review corrections, escalation rate, duration and later user-reported regressions. Compare like task classes and policy versions.

A task accepted because a person answered `relais decide --answer approve` counts in that denominator exactly like one relais accepted on its own checks and review — work a person took is work that landed, and excluding it measures everything except the cases a person cared enough to judge. The two are reported as separate counts beside the metric, never as one number that mixes them, because how much of the denominator relais itself vouched for is the question the metric is asked to answer.

A task whose run stopped without relais's own acceptance — terminal on its own, or still waiting on a person — but whose candidate a person finished and merged anyway (`relais decide --answer salvaged`, §9), also counts in the denominator — the cost was already spent and the work already landed; excluding it understates the metric by exactly the work relais discarded or stalled on and a person rescued. `salvaged` is the only answer that counts here: `decided` resolves the question a run raised and says nothing about whether code shipped, so a task answered `decided` is not in the denominator, whereas `salvaged` is a person's acceptance of specific work — the candidate it names.

## 12. Persistence and crash recovery

Use a machine-local data directory with a SQLite ledger and an artifact directory per run. Suggested records: tasks, contract revisions, attempts, transitions, evidence, usage events and outcomes. Store payload schemas with explicit versions and support additive migrations.

An evidence row's kind is a typed value, not free text — a stored name the reading binary does not know is a corrupt row, never silently dropped or coerced. A row may also name where it came from (the tool that produced it and that tool's own identifier for it) and what it is about (the subject it attests to and, when it answers one, the acceptance criterion). All four are optional. `relais evidence attach` records one such row for evidence a tool outside relais produced — against a run and, optionally, the criterion it answers — and decides nothing about it: recording is not deciding, so it never marks a criterion met, changes a run's state, or clears a gap. It refuses a run it does not know, and a criterion id the run's contract does not declare, naming the ids it does.

Artifacts include context manifest, worker results, candidate patch, check logs and receipt. Raw transcripts are opt-in; avoid retaining credentials or private reasoning. Redaction and retention policies apply to artifacts as well as logs. Relais sends no product telemetry.

Persist dispatch intent before spawning a process, then its PID/session identifier. On restart reconcile liveness, terminal output and artifacts before scheduling another attempt. Never assume an absent terminal result means nothing executed. Mark uncertain state `interrupted`, preserve changes, and avoid retrying commands with unknown side effects.

A receipt includes run ID, candidate identity, contract/policy hashes, outcome, verification evidence, actual models, attempt count and cost completeness. A returned worker JSON document cannot impersonate a runner receipt.

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

The programmatic adapter can use `claude -p`, structured JSON results and schema-constrained output. Validate the actual event/result shape for each supported release. See programmatic usage.

Model, effort, turn limits and an API dollar-budget flag are documented CLI capabilities. Their availability and exact behavior must be capability-checked; they do not establish an exact subscription or in-flight financial cap. See CLI reference.

Quality-constrained cost measurement follows the approach in Anthropic's cost-optimization cookbook. Its example savings are not forecasts for Relais.

## 16. Native Rust learning, informed by RouteLLM

Relais MUST NOT load RouteLLM's pretrained router checkpoints, derive training labels from their scores, or use those scores as routing features. It owns its training data, feature pipeline, labels, learning objective and model artifacts.

RouteLLM is a research and design reference, not a dependency or compatibility target. Learn from its separation of scoring, selection thresholds and evaluation, while defining native Rust interfaces around Relais task outcomes and complete execution profiles. Do not copy its stock preference objective or interpret a strong-model call percentage as a coding-quality guarantee. No Python process, RouteLLM package, OpenAI-compatible routing proxy or external embedding service is required.

The runner selects a profile at a task boundary and launches the existing harness. Training and inference use the same Rust feature extraction and prediction code. Artifacts store a versioned feature schema, coefficient arrays, normalization data and any validated calibration parameters. Loading rejects incompatible schemas and non-finite values. No cross-language model export is needed. The external coding harness remains its own dependency; Rust-only refers to the Relais application and its learning implementation.

The initial owned learner is regularized logistic regression for acceptance without escalation, with a separate simple cost estimator for complete-strategy cost. This is an implementation choice within the complete product, not a staged product version. Matrix factorization or a larger learner may replace it only if evaluation establishes a benefit. The chosen learner does not reproduce RouteLLM's matrix-factorization architecture.

The initial feature extractor is deterministic: task category, language, scope breadth, existing implementation examples, relevant risk indicators, verification availability, unresolved decision dependencies, and model/effort/harness identity. Optional hashed task-text features use the same frozen tokenizer and hashing configuration at train and inference time. No external embedding API is required by default.

Only information available at dispatch enters initial-routing features. Actual patch size, later failures and final outcomes are labels or recovery-stage evidence, not initial features. Unavailable features are represented explicitly. Free-text task inputs are untrusted data, not policy instructions.

An inference result contains artifact ID, feature-schema version, input hash, eligible profile IDs, acceptance estimates, cost estimates, supported cohorts and an abstention reason if applicable. Predictions are not guarantees. The deterministic runner remains responsible for policy enforcement and final acceptance.

### Rust learning implementation

Use regularized logistic regression over task/profile features for acceptance without escalation. Include task/profile interaction features so capability varies by task class rather than only assigning a global strength to each model. Fit a separate regularized cost estimator over complete execution strategies; evaluate its behavior on skewed costs and retain empirical cohort estimates as a baseline. Neither predictor may invent coverage for unseen profiles.

Training runs on the CPU. Implement or select a maintained Rust numerical solver after a focused dependency review; choosing Rust does not require rewriting general numerical infrastructure. Use numerically stable logistic loss, explicit convergence criteria, seeded data ordering, feature scaling fitted only on training data, and bounded iteration counts. Class imbalance and sparse features must be represented in evaluation rather than hidden by aggregate accuracy.

Keep modules small: features, dataset, learner, predict, evaluate, and registry. Share transformations between training and prediction. Test objective/gradient consistency, convergence on known fixtures, serialization round trips, missing features, non-finite values and decision-boundary behavior. Reproducibility means recording solver settings, seed and dependency versions; do not promise bitwise identity across all CPU architectures.

No GPU, neural network framework or embedding model is necessary for this design. An optional research experiment in another language must not become a build, training or runtime requirement for the shipped product.

## 17. Owned dataset, training and promotion

### Collection and labels

A task is the unit of sampling — a task retried to acceptance contributes one training example, not one per attempt. An execution profile combines model, effort, harness, context policy and bounded recovery policy. Capture the starting candidate identity and task contract so comparisons can reproduce the same task.

The labelling rule is versioned, and every dataset and artifact records which version produced it, so a dataset built under an earlier rule is distinguishable from one built under this one rather than silently comparable.

For every dispatch record pre-dispatch features, eligible profiles, selected profile, actual selection probability, versions, timestamps, verification results, human corrections where supplied, usage completeness and costs. Keep attempt-level outcomes distinct from complete-strategy outcomes: a cheap worker rescued by a stronger worker did not succeed without escalation, while the complete strategy may still have delivered an accepted result.

Labels are evidence-backed: verified acceptance, rejected candidate, blocked environment, interruption, escalation, user correction or confirmed regression. Infrastructure failures and unknown results are not silently converted into reasoning failures. Absence of later feedback is not proof of defect-free output. Maintain observation windows for delayed outcomes and identify still-pending labels.

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

Detect drift through profile/version changes, feature distribution and observed outcomes. New model or harness versions receive new identities; evidence is not blindly inherited. Confirmed regressions trigger investigation, trial suspension or rollback according to policy. Retraining must not relax risk floors, acceptance criteria or permission boundaries.

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

The adapter contract includes launch, events, cancellation, resume/reconciliation, effective profile, permission capability, sandbox capability and usage completeness. The mandatory Claude Code adapter uses supported native authentication and launches explicitly selected models. An optional alternative adapter can run another harness or local model. No adapter may advertise guarantees its backend cannot enforce.

After acceptance, allow the user to record accepted unchanged, corrected, reverted or confirmed regression. Feedback is attributed to the candidate and strategy, with correction magnitude and evidence where available. The candidate is optional: a run accepted through a person's approval on a contract interrupted before verification completed never wrote a receipt, and feedback about it is still worth recording. Absence of feedback is not a positive quality label. Retain both immediate verification and delayed outcomes.

Feedback about a run a person salvaged (§9) is attributed to the candidate the salvage named, not to any candidate the run's own discarded attempt produced — that attempt's work was never what was merged.

A task's later recorded outcome overrides its run's terminal state for labelling purposes: a reverted change or a later confirmed regression is never a positive label, whatever its run's state said. A task accepted through a person's approval rather than verification alone is recorded as such — the dataset names which route accepted it — and does not earn the positive label reserved for verified acceptance.

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

A coordinator restart adopts every dispatch the ledger still calls `launched` and bound to a live process, so caps already in force keep binding across the restart: the row it adopts from names the session it belongs to, the money it reserved, its source, its parent dispatch and its depth, and `Ledger::live_dispatches` returns all five rather than only a dispatch id, run id and pid. `Coordinator::start` builds the adopted dispatch's admission request from those values, not from placeholders — the per-session cap that refused a fourth agent before a restart still refuses it after one, because the adopted dispatch still names the real session rather than a literal `"unknown"` every restart used to share. A row written before the migration that added `source`/`parent_dispatch`/`depth` carries `NULL` in all three forever: adoption reports that as `Attribution::PreMigration` rather than guessing root/depth-zero and calling it fact, and `relais coordinator status` counts those dispatches apart (`adopted_pre_migration`) so a person can see how much of what is live rests on a complete record. This crate's own managed dispatches are always root (depth 0, no parent) — a deeper, hook-admitted spawn never reaches the ledger at all (see "Managed and observed execution" below) — so `parent_dispatch`/`depth` on a post-migration row are always `NULL`/`0` and never a guess either.

### Managed and observed execution

For Relais-managed dispatch, reserve capacity and budget atomically before launch. Return a dispatch ID; retries with the same ID cannot create duplicate agents. The adapter binds the resulting harness agent/session ID to that reservation.

For ordinary native Claude Code subagents, collect available lifecycle and transcript events and track the tree without requiring users to replace every delegation with a manual CLI command. Where the installed harness provides a reliable pre-dispatch interception point, the adapter can enforce admission and select permitted invocation parameters. This capability must be verified by integration tests, including nested starts, resumes, forks and cancellation.

`SubagentStart` is an observation/context event, not a creation veto. A post-start hook cannot enforce an atomic global spawn cap. Claude Code's own session-local concurrency/depth settings are defense in depth, not a cross-tab resource scheduler. Hook/tool names and payloads are version-dependent; probe them through the compatibility matrix.

If a launch/resume path cannot be intercepted, label its enforcement `observed`, include known usage, and do not claim hard aggregate concurrency/model guarantees over it. Strict runs use managed dispatch for such paths. Coordinator outage must not silently turn a strict managed launch into an unmanaged launch; preserve the request and report unavailable admission. Ordinary unwrapped Claude Code sessions remain usable and are not forcibly terminated.

### The hook compatibility matrix

Before any package reasons over a hook payload, `relais hook --probe --record <dir>` and `relais doctor --probe-hooks` establish what the installed Claude Code actually sends, so later packages test against a transcription of a real session rather than a belief about one.

`relais hook --probe --record <dir>` is record-only: it reads one hook payload on stdin and writes it to `<dir>` byte for byte, alongside the event name and the order it arrived. It parses nothing beyond a best-effort event name for the file name, decides nothing about the payload's shape, and always exits 0 without writing to stdout — a hook that exits non-zero or writes to stdout can block or alter the tool call that triggered it, and a probe that changed the session it was measuring would be measuring itself. A payload that is not valid JSON, exceeds a sane size cap, or arrives for an event the handler was not wired for is still recorded and still exits 0.

`relais doctor --probe-hooks` wires that handler, via a throwaway settings file it writes under the state directory (never the user's own `settings.json`), into the seven targets Relais uses: `PreToolUse`, `PostToolUse` and `PostToolUseFailure` on the Agent tool, plus `SubagentStart`, `SubagentStop`, `SessionStart` and `SessionEnd`. It runs one real `claude -p` session through that settings file, prompted to force at least one nested agent call, then reads the recordings back and writes a compatibility record naming the Claude Code version observed and, per target, whether it fired and which top-level fields its payloads carried — reported as what was seen, never as what was expected. A target that did not fire is recorded as not fired, since a version that sends no such event is a fact about that version, not a probe failure. This needs a real Claude Code, costs money and touches the network, so it is never part of the local check suite; it has its own `make probe-hooks` target.

`relais doctor`'s ordinary run reads that compatibility record back and reports it stale when the Claude Code on PATH no longer matches the version the record names, in the same finding shape doctor uses elsewhere, rather than silently trusting a record from a different version.

The seven installed targets are the same names the compatibility matrix probes, and the two lists never drift apart silently: a test fails if the target set `--hooks` installs disagrees with the target set `--probe-hooks` records. They differ only in the matcher, on purpose. The three tool-scoped events install matched to the Agent tool and its legacy name (`Agent|Task`, since the harness still sends either); the four lifecycle events install with no matcher, because they are not tool calls. `--probe-hooks` matches none of the seven to anything, deliberately: matched to the Agent tool alone, it would record only the top-level spawn and nothing a subagent then does, which is the one thing that probe exists to see.

### Reporting the hook by exercising it

`relais doctor`'s ordinary run reports the live hook by running it, not by reading `settings.json` back: it takes the exact command recorded there, spawns it with a fixture `PreToolUse` spawn payload on stdin, and redirects its config and state directories to a scratch environment of their own — one whose machine settings say to refuse admission when the coordinator cannot be reached, which a scratch state directory holding no coordinator socket never can be. This never touches the person's real hook journal or reserves a seat in their real coordinator; measuring the hook must not change the thing it measures.

Three outcomes, reported apart, because the last one is the dangerous one: no relais command recorded on `PreToolUse` in any settings.json this repository can see; a recorded command that was exercised and refused, exit 0 with a deny on stdout, as configured; and a recorded command that ran but printed no deny — wired in and enforcing nothing, a dead guard that reads as enforcement to anyone who has not gone looking, reported together with how it ended, since a hook that hung and a hook that exited 0 with nothing to say are different defects. A fourth line is not a verdict on the hook at all: when the exercise could not be staged or the command could not be spawned, doctor says whether it refuses is unknown and why, because "it enforces nothing" is a claim about a run, and that run did not happen. This check costs nothing and touches no network — it is part of the ordinary `relais doctor` run, unlike `--probe-hooks`.

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

A hook cannot refuse to answer, and it must never say yes on a person's behalf: doing so would override that person's own permission settings, which is not this binary's business. So `relais::hook::decide::decide(event, settings, coordinator)` resolves every case to exactly one of two things — `HookAnswer::Silent`, or `HookAnswer::Refuse { reason }` — and nothing else, ever. There is no variant for an affirmative answer at all, and a test greps the module's own production source for the word that would spell one out, so the rule survives a future author who has not read this paragraph.

`decide` is a pure function of the typed `HookEvent`, `policy::HookAdmissionSettings`, and whatever the coordinator answered (`Option<admission::Decision>`; `None` for "could not be reached"). It reads no clock, touches no filesystem and makes no network call, so every case can be read and tested without running a session; it also touches no coordinator, journal or ledger, and installs nothing — deciding and admitting are different packages. Only a `PreToolUse` on the Agent tool is ever refused: every other event — the four lifecycle events, `NotOurs`, a `PostToolUse`, and a `PostToolUseFailure` — is `Silent`, because a tool call that has already returned cannot be blocked. `PostToolUseFailure` is matched explicitly to `Silent` rather than folded into a wildcard: across five probe sessions (`tests/fixtures/hooks/README.md`, `tests/fixtures/hooks-concurrent/README.md`) it has never fired, including one whose tool call genuinely failed and arrived as an ordinary `PostToolUse` carrying the error, so installing real handling for it would claim a coverage this crate does not have. If a later probe run records one, that recording is what should reopen this.

For a spawn, a coordinator that could not be reached is handled by `HookAdmissionSettings::on_coordinator_unreachable` rather than by a decision made here: `CarryOn` (the default) stays silent, `Refuse` declines and names the coordinator as unreachable. `Decision::Granted` and `AlreadyAdmitted` resolve `Silent` — neither is a refusal, and neither is spoken as an affirmative answer either. `Decision::Queued` is not decided on directly: `hook::respond::wait_for_a_seat` polls the coordinator again at `runner::ADMISSION_POLL`'s cadence — the same re-asking protocol `AdmissionState::request` already serves `relais run`'s own managed dispatch — for up to `HookAdmissionSettings::queue_behaviour`'s bound, and only the answer that poll settles on (still `Queued` at the deadline, or whatever the coordinator answers next) reaches `decide`. `Decision::Refused { code, detail }` resolves to `Refuse`, whose `reason` is built from a table over every `Refusal` variant: the coordinator's own `detail` (what was exceeded) plus one line of advice (what the person can do about it — raise a limit, wait for a reservation to settle, start a new run), because a refusal a person cannot act on gets ignored, and this one arrives in the middle of their work. A `Queued` answer still resolves `Refuse` once the wait is exhausted, but the reason now says how long this firing actually waited, not that a hook cannot hold a call open — see below, it can. Every `Decision` and `Refusal` variant is matched by name, with no `_` arm, so a variant added to either fails this module's compile until the table says what it means.

Three facts, measured directly against a real Claude Code 2.1.282 session, govern every deadline in this package: a `PreToolUse` hook holds its tool call open for as long as it runs (a 5s hook produced a 5s wait before the call proceeded); a hook that outlives the harness's own handler `timeout` is killed and its answer discarded while the guarded tool call proceeds anyway — an expired hook is an admission nobody decided, not a refusal; and with no `timeout` field configured at all there is effectively no ceiling (a 300s hook ran to completion, and the session waited the full 312s). `queue_wait_secs` under `[admission]` in machine.toml (default: 2 seconds, `policy::DEFAULT_QUEUE_WAIT_SECS`) parses into `policy::QueueBehaviour::{RefuseImmediately, WaitUpTo(Duration)}` — zero meaning refuse immediately, exactly as before this existed — and `relais install --claude --hooks` derives the `PreToolUse` handler timeout it writes from that wait plus `ipc::CONNECT_TIMEOUT`, twice `coordinator::REQUEST_TIMEOUT` and a stated margin (`install::settings::derived_pretooluse_timeout`), so the hook always has enough of its own budget left to withdraw its request and print a refusal before the harness would kill it. Every other installed handler carries a modest, explicit timeout of its own, because an absent one waits indefinitely. `relais doctor` reads the timeout actually recorded in settings.json and fails — not warns — when it no longer covers the currently configured wait, because the hook itself cannot read its own handler timeout to check.

A hook killed mid-wait — by the harness's own timeout, or an interrupt — cannot strand the seat it was polling for: `AdmissionState::reconcile` frees a dispatch that `drain` admitted but that nothing ever claimed after `admission::UNCLAIMED_GRACE` (five seconds, well short of `admission::LEASE_GRACE`'s 300, and well longer than `runner::ADMISSION_POLL`'s 250ms cadence, so an ordinary poller is never reaped out from under itself). The hook journal records `waited_ms` and an `outcome` of `immediate`, `admitted_after_waiting` or `refused_after_waiting` for every firing, so backpressure and stalling are distinguishable in the record.

`decide` never panics in its own right, but `decide_or_silent` wraps it in `catch_unwind` anyway and turns any panic into `HookAnswer::Silent`: a hook that dies mid-decision would otherwise leave the tool call it was watching unanswered. `HookAnswer::stdout_payload` renders the decision into the one shape a hook may speak in — `None` for `Silent` (nothing is printed at all) and, for `Refuse`, a `hookSpecificOutput.permissionDecision: "deny"` JSON string carrying `permissionDecisionReason` — without performing the write itself; the caller that owns stdout does that. That is the current spelling rather than the older top-level `{"decision":"block"}` one; both were run against real Claude Code 2.1.282 and each blocked the tool call, reaching the model identically as a hook error, so the choice is for longevity and not for behaviour. If a refusal ever stops taking effect, re-run that comparison rather than trusting this paragraph: a deny that is ignored exits 0, the call proceeds, and nothing reports a failure.

### Answering for real: `relais hook`

`relais hook`, run with no flags, is the caller `decide.rs` used to say did not exist: `hook::respond::handle(payload, settings, gate)` parses the payload, asks `gate.admit` about a spawn — built from `ids::derive_dispatch_id`/`derive_run_id` over the session and tool-use IDs, and only for a `PreToolUse` on the Agent tool; every other event is answered with no call at all, because `decide` never refuses one and a request this function has no later chance to release would reserve a seat for nothing — applies `decide_or_silent`, and, on a refusal, calls `gate.withdraw` for the dispatch it asked about before returning. That withdrawal is what keeps a spawn relais refuses from being left holding whatever its own request reserved: `admission::AdmissionState::request` never actually reserves anything for a decision that comes back `Refused` (the hard limits are checked before anything is admitted), so the call is usually a no-op today, but the refusal path calls it unconditionally rather than depending on that staying true. `handle` itself is wrapped in `catch_unwind`, the one layer below `decide_or_silent`'s own — this is where a socket and a clock are actually touched, neither of which `decide` reaches.

The first spawn of an ordinary session — one no `relais run` ever started — finds no run on record, because nothing before it ever registers one: `gate.admit` answers `Refused { code: UnknownRun, .. }`. Rather than let that stand as the hook's only possible answer, `ask_coordinator` registers the run itself before giving up: it derives the run id from the session already in hand exactly as `admit` did (never one a caller handed it, which is what keeps a registration from laundering a run relais never derived), calls `gate.register_run` with no budget, agent-cap or depth override at all, and retries `admit` once. What the retry answers — admitted, still unknown, or anything else — is what the hook reports; there is no second retry, because a hook cannot hold a tool call open while it tries again. An unreachable coordinator (`gate.admit` returning `Err`) is not registered against at all: that failure is `CoordinatorAnswer`'s `None`, resolved by `on_coordinator_unreachable` as before, and registering against a daemon that just failed to answer would turn every ordinary tool call on a down machine into a second connection attempt. This is what makes the caps in `[concurrency]` on machine.toml reachable on the hook path at all: before this, every session `relais run` had not already started saw `UnknownRun` on its very first spawn and never anything else, so no cap below it — the per-session agent count, the per-run aggregate limit — had ever had the chance to be the thing that actually refused a spawn.

For the per-session cap to count agents rather than spawns, the seat has to follow the AGENT, not the tool call, and the two end at different moments: an Agent `PostToolUse` fires when the call returns, and a real Claude Code 2.1.282 session showed it returning at LAUNCH — ~100ms after its `PreToolUse`, `duration_ms: 8`, `tool_response.isAsync: true`, `tool_response.status: "async_launched"` — while the agent it launched ran 6–16 seconds more (`tests/fixtures/hooks/0009-PostToolUse.json`, `0010-SubagentStop.json`). A seat given back on `PostToolUse` was given back at launch, and the cap did not bind. So `handle` admits on `PreToolUse`, binds on `PostToolUse`, and releases on `SubagentStop`:

- **Admit on `PreToolUse`**, as above.
- **Bind on `PostToolUse`.** Its `tool_response.agentId` names the agent the call launched — the only place a tool call and the agent it produced appear in one payload — and `event::ToolCallPhase::Post { launched_agent }` is the only phase that carries it, so the type says so. `handle` calls `gate.bind_agent_lease(dispatch_id, agent_id, Provenance::Known)` (wire request `BindAgentLease`), the dispatch id derived from the same `tool_use_id` the spawn carried. Nothing is settled or released here. A `PostToolUse` that names no agent, and a `PostToolUseFailure` (which launched nothing), bind nothing and release nothing.
- **Release on `SubagentStop`.** It names the agent (`agent_id`) and no tool call, so its dispatch id cannot be derived; `handle` calls `gate.settle_by_agent(session_id, agent_id, None)` (wire request `SettleByAgent`), and the coordinator settles and releases whichever dispatch is bound to that agent in that session. `AgentSettleOutcome::NothingBound` is an ordinary answer, not an error: an agent spawned before the hook was wired, or while the coordinator was down, ends like any other.

The order of the last two does not matter. A synchronous spawn's `SubagentStop` arrives BEFORE its own `PostToolUse` (`tests/fixtures/hooks-concurrent`), finding nothing bound; the coordinator remembers that stop (bounded, like finished dispatch ids), and the bind that follows ends the dispatch at once. Settlement is `None` rather than a figure, because a hook payload carries no usage: `None` books the reservation as a lower bound and marks the run uncertain, which is what not knowing looks like when it is recorded honestly, where zero would be a claim that the agent was free.

The failure mode is stated rather than implied away: **an agent whose `SubagentStop` never arrives holds its seat until its lease lapses** — `binding_lease_secs` for a bound agent, `LEASE_GRACE` plus `UNBINDABLE_AFTER` reconciles for a dispatch no `PostToolUse` ever bound — and reconciliation then settles it as unknown. The cap over-counts for that long; it never under-counts because an end was missed. Nothing renews a hook-admitted agent's lease while it runs, so an agent that outlives `binding_lease_secs` is expired by reconciliation before its `SubagentStop` arrives.

Adding the two calls changed the wire protocol: `coordinator::PROTOCOL_VERSION` is 3, and a v2 daemon left running is named as a version skew at the first call (`relais coordinator stop` fixes it).

Two limits of this path are consequences of what a hook payload does and does not carry, and are stated here rather than implied away. **Depth is not enforced**: nothing in a single payload joins the tool call that admitted an agent to the dispatch it descends from, so every hook-admitted spawn is requested at depth 0 and `max_agent_depth` governs only managed dispatch. **No money limit is enforced**: the run the hook registers carries no budget, and `check_hard_limits` refuses on budget only for a run that has one, so `dispatch_reserve_micros` above zero changes what a spawn reserves against an unbounded total and refuses nothing. A hook-admitted session is capped by agent count, not by spend.

Every `DispatchRequest` names a `DispatchSource` — `HookAdmitted` for this path, `ManagedRun` for `relais run`'s own dispatch, `Observed` for a dispatch this coordinator only ever watched — a field the caller that builds the request already knows, never inferred afterward from what the dispatch looks like. `StatusSnapshot` carries live and admitted counts broken down by source, plus how many hook-admitted leases are past `agent_lease_ttl` and not yet reaped, and `admission::enforcement_line` renders the one sentence `relais coordinator status` prints and `relais report`'s `enforcement` key carries as the same structured data: what IS capped for a hook-admitted spawn (agent count per session and per run, the seat held from `PostToolUse` bind until `SubagentStop` or its lease) and what is NOT (depth and spend, for the reasons stated two paragraphs up). `Enforcement` itself gained `Observed` — no hook wired, or the coordinator unreachable with `carry_on` in force — alongside `InProcess` and `Coordinator`, so the sentence never claims a guarantee nothing is holding.

A cancellation on this path holds only while the coordinator still remembers the run. `relais cancel` marks it terminal; the next reconcile — `RECONCILE_EVERY`, 15 seconds — reaps a terminal run with nothing outstanding, and the spawn after that finds no record, registers the run again and is admitted. The cancellation lives in coordinator memory and nothing on disk distinguishes a reaped-cancelled run from a session never seen, so a hook cannot refuse on a cancellation it can no longer read. Cancelling a tab's agents is not a durable stop.

The CLI command wraps `hook::respond::handle` in one more `catch_unwind` and always returns exit 0: a non-zero exit from a hook is reported to the session as a failure of the tool call it was watching, so a relais that cannot answer — an unreadable machine.toml, an unreachable coordinator, a payload that is not JSON, a panic anywhere in between — must be indistinguishable from a relais that had nothing to say. A missing or invalid machine.toml is not surfaced either; `HookAdmissionSettings::default()` applies instead, and `relais doctor` is where a settings problem is reported as a finding.

Every firing is journalled as one JSON line — the raw payload (whatever arrived, including fields this crate does not model, since a payload names a person's transcript path and working directory), the classified event, whatever the coordinator answered, and the decision and its reason if any — appended to `hook_journal.jsonl` under the state directory (`paths::hook_journal_path`), which `hook::respond::append_journal` creates and keeps at mode 0600 so it is never group- or world-readable. Journalling is best effort, like everything else in this path: a write failure there is swallowed rather than turning an answered hook into a failed one.

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

---

Companion repositories: [amont](https://github.com/fredericrous/amont), [aval](https://github.com/fredericrous/aval), [amont-agent](https://github.com/fredericrous/amont-agent).
