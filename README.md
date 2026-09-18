# relais

Relais is a coding-agent execution companion: it chooses a model and reasoning
effort for a bounded coding task, supplies authoritative context, verifies the
result, and escalates when necessary. Its objective is to reduce total cost
per accepted change while preserving an explicit quality bar.

Status: in development against the target specification in `docs/SPEC.md`. The
commands, schemas and integrations there are proposed interfaces, not shipped
features. Relais is a working name; package availability has not been checked.

## Build

```sh
make check   # toolchain lint test msrv
make build
```

## Layout

One crate, `crates/relais`, one binary. Modules follow SPEC §13:

| module | owns |
|---|---|
| `contract` | task contracts: validated, frozen, hashed; work plans (§4, §19) |
| `policy` | `relais.toml`, machine settings, the effective authority (§5) |
| `route` | deterministic routing, risk floors, the learned predictor hook (§6) |
| `context` | context manifests and aval resolution (§7) |
| `workspace` | owned worktrees, candidate snapshots, scope checks (§8) |
| `adapter` | the `Backend` trait, the Claude Code adapter, a mock (§20) |
| `runner` | the interpreter: observes, records, performs one effect per step (§9, §10) |
| `runner::machine` | the §9 observation table as a pure function, one test per row |
| `runner::scheduler` | bounded decomposition and integration (§19) |
| `verify` | verification profiles, amont gaps, receipts (§10) |
| `admission` | the pure admission state machine: caps, budgets, leases (§23) |
| `coordinator` | the per-user daemon, election, socket protocol, client (§23) |
| `ledger` | the SQLite ledger with additive migrations (§12) |
| `report`, `doctor`, `install` | reporting, diagnostics, the Claude integration |
| `learn` | features, dataset, learner, predict, evaluate, registry (§16, §17) |

`crates/relais/tests/release_scenarios.rs` runs the §14 release scenarios
through the real binary against a fake `claude`; the §23 concurrency
scenarios are unit tests over the admission state machine.

## Reading the code

Start at `runner::machine::decide`: it is the spec's §9 table with each
row a `match` arm and a unit test. `runner::RunEngine::run_inner` is the
interpreter around it; everything it touches — git, SQLite, processes —
is behind a `Result`, and a failure of the runner's own machinery ends a
run as `interrupted` with reason `runner_failure`, never as a panic.
`admission::AdmissionState` is the same shape for §23: every method takes
`now`, liveness is a callback, and the concurrency scenarios run in
milliseconds.

## Known limits

- The decomposition scheduler executes packages sequentially in
  topological order; waves of independent packages are computed and
  recorded but not yet run concurrently.
- Native Claude Code subagents are observed, not admitted: only managed
  dispatch through `relais run` is capped by the coordinator.
- The `msrv` target proves the declared floor only when that toolchain is
  installed (`rustup toolchain install 1.88.0`); otherwise it says so.
