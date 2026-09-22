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

No silent fallback is allowed. Unavailable models yield `blocked:model_unavailable`, or an explicitly pre-authorized alternative whose identity and cost are recorded. Provider-driven model substitution is detected where observable; an unapproved substitution stops further dispatch and invalidates any claim that the requested route was tested.

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

Successful execution delivers a candidate patch, base revision, retained worktree and verification receipt. Integration into the user's branch is an explicit later action. Relais never force-cleans a worktree containing unexported changes.

## 9. Attempt lifecycle and escalation

States: `prepared`, `running`, `verifying`, `repairing`, `escalating`, `accepted`, `needs_review`, `needs_decision`, `blocked`, `failed`, `budget_exhausted`, `cancelled`, `interrupted`.

Every transition has a reason code, timestamp and evidence references. A worker can propose completion or blockage; only the runner assigns final state.

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

Semantic review is risk-dependent. A separate reviewer gets the contract, candidate, pertinent source and architecture evidence. It reports concrete findings with file/range, violated criterion, evidence and suggested verification; it cannot edit or waive checks. Findings are triaged by evidence. Lack of findings is not mathematical proof, and reviewer disagreement returns `needs_review` when it cannot be resolved within limits.

An accepted receipt means the declared acceptance policy passed on that candidate. It does not authorize merge, certify all correctness, or remain valid after edits. Every changed candidate requires fresh applicable verification.

## 11. Cost and resource accounting

Record requested and effective model, effort, harness version, token categories where provided, provider-reported cost, estimated cost, pricing-version reference, duration, attempt, phase and final outcome. Mark missing usage `unknown`; never replace it with zero.

Costs include preparation performed by a model, research, implementation, repairs, escalation and review. Outer interactive-session overhead is unknown unless correlated from its transcript; reports must label that boundary. Native assistance cannot be compared as if its parent session were free.

Provider-reported totals and child usage are alternative aggregation sources: do not add an inclusive parent total to its children. Deduplicate stable request/event IDs. Persist accounting incrementally and reconcile terminal totals after completion. An interrupted run may have an incomplete lower-bound cost.

Use an integer or decimal money representation, not floating-point accumulation. Distinguish API spend, usage-credit spend, estimated API-equivalent cost and subscription consumption. Do not convert token savings directly into subscription fee savings.

The runner enforces dispatch, attempt, turn and wall-time ceilings. API dollar controls are best effort across in-flight requests and delayed reporting; reserve a margin before dispatch and stop admitting work once exhausted. Report overshoot and unknown usage. Do not advertise an exact financial cap where the provider cannot guarantee it.

Primary outcome metric: all recorded cost, including failed runs, divided by accepted tasks in the same cohort. Also report acceptance rate, review corrections, escalation rate, duration and later user-reported regressions. Compare like task classes and policy versions.

## 12. Persistence and crash recovery

Use a machine-local data directory with a SQLite ledger and an artifact directory per run. Suggested records: tasks, contract revisions, attempts, transitions, evidence, usage events and outcomes. Store payload schemas with explicit versions and support additive migrations.

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

A task is the unit of sampling. An execution profile combines model, effort, harness, context policy and bounded recovery policy. Capture the starting candidate identity and task contract so comparisons can reproduce the same task.

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

Cache verification only when all declared relevant inputs match: candidate content, dependency lockfiles, toolchain, command, configuration and required environment identity. Checks with undeclared external dependencies or nondeterministic behavior are not cacheable by default. Preserve amont/attest ownership and never forge attestations. Cache misses rerun checks. Targeted impact analysis supplements mandatory checks rather than removing them. A profile's setup step is part of its configuration, and the programs the setup runs are part of the toolchain identity; a cached baseline skips the base's setup, not the task worktree's.

## 19. Bounded decomposition and recovery

A bounded planner may propose a dependency graph of work packages when a single task contains separable deliverables. Deterministic validation requires explicit scope, dependencies, per-package acceptance, integration acceptance and aggregate resource limits. Planning overhead is included. Simple tasks remain single-worker runs.

Each package uses an owned worktree at its declared input revision. Independent non-overlapping packages may run concurrently up to a machine-owned limit. Overlapping writes are serialized or reconciled in an explicit integration package. Workers may request child agents through the managed dispatch path. Each descendant inherits the root run budget and a scope no broader than its parent. Native child agents can also be observed, with enforcement capability reported explicitly; untracked recursive spawning is not accepted as supervised execution.

A scheduler assembles completed candidates and verifies the integrated revision. Independent receipts do not constitute final acceptance. Integration failures consume the same aggregate budget and cannot restart an unlimited task graph.

At bounded checkpoints, recovery chooses between retrieving missing context, a focused repair, higher effort, a stronger profile, or an explicit stop. Environment failures are not automatically escalated to expensive models. Strategy changes preserve the contract; scope or intent changes require a new contract revision.

## 20. Execution backends and final outcome feedback

The adapter contract includes launch, events, cancellation, resume/reconciliation, effective profile, permission capability, sandbox capability and usage completeness. The mandatory Claude Code adapter uses supported native authentication and launches explicitly selected models. An optional alternative adapter can run another harness or local model. No adapter may advertise guarantees its backend cannot enforce.

After acceptance, allow the user to record accepted unchanged, corrected, reverted or confirmed regression. Feedback is attributed to the candidate and strategy, with correction magnitude and evidence where available. Absence of feedback is not a positive quality label. Retain both immediate verification and delayed outcomes.

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

### Managed and observed execution

For Relais-managed dispatch, reserve capacity and budget atomically before launch. Return a dispatch ID; retries with the same ID cannot create duplicate agents. The adapter binds the resulting harness agent/session ID to that reservation.

For ordinary native Claude Code subagents, collect available lifecycle and transcript events and track the tree without requiring users to replace every delegation with a manual CLI command. Where the installed harness provides a reliable pre-dispatch interception point, the adapter can enforce admission and select permitted invocation parameters. This capability must be verified by integration tests, including nested starts, resumes, forks and cancellation.

`SubagentStart` is an observation/context event, not a creation veto. A post-start hook cannot enforce an atomic global spawn cap. Claude Code's own session-local concurrency/depth settings are defense in depth, not a cross-tab resource scheduler. Hook/tool names and payloads are version-dependent; probe them through the compatibility matrix.

If a launch/resume path cannot be intercepted, label its enforcement `observed`, include known usage, and do not claim hard aggregate concurrency/model guarantees over it. Strict runs use managed dispatch for such paths. Coordinator outage must not silently turn a strict managed launch into an unmanaged launch; preserve the request and report unavailable admission. Ordinary unwrapped Claude Code sessions remain usable and are not forcibly terminated.

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
