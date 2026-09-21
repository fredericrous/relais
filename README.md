# relais

Relais is a coding-agent execution companion: it chooses a model and reasoning
effort for a bounded coding task, supplies authoritative context, verifies the
result, and escalates when necessary. Its objective is to reduce total cost
per accepted change while preserving an explicit quality bar.

Status: in development against the target specification in `docs/SPEC.md`. The
commands, schemas and integrations there are proposed interfaces, not shipped
features. Relais is a working name; package availability has not been checked.

## Install

    curl -fsSL https://raw.githubusercontent.com/fredericrous/relais/main/install/install.sh | sh

Pin a version or move the destination with `RELAIS_VERSION` and
`RELAIS_BIN_DIR`. Windows:
`irm https://raw.githubusercontent.com/fredericrous/relais/main/install/install.ps1 | iex`.
Prebuilt, checksummed binaries for Linux, macOS and Windows are on the
[releases page](https://github.com/fredericrous/relais/releases); a release
is cut by a `v*` tag that matches `Cargo.toml` and has a `CHANGELOG.md`
section. Or from source: `cargo install --path crates/relais`.

## Build

```sh
make check   # toolchain lint test msrv
make build
```

CI (`.github/workflows/ci.yaml`) runs the same targets on every push and
pull request — lint and msrv on Linux, the test suite on Linux and macOS —
plus a weekly, non-blocking `cargo audit`. A green check on a PR means what
a green `make check` means on the workstation.

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

## Using it on a repository

```sh
cd the-repo && relais init          # writes relais.toml; edit models + profile, commit it
relais plan --task task.json        # prints the authority hash; blocked until trusted
```

Every command finds `relais.toml` upward from the cwd to the repository
root, so a subdirectory (or a task worktree, which carries its own copy)
works; a directory outside any repository is refused by name.

Machine-owned settings live in `~/.config/relais/machine.toml` and are
never written by a run. The trust grant is keyed by the authority hash
`plan` printed; editing `relais.toml` changes the hash and voids the
grant. A print-mode worker cannot ask for permission, so the tools it may
use are an explicit machine-owned allowlist — nothing is granted
implicitly, and no permission-mode flag is ever passed:

```toml
schema_version = 1

[spending]
per_run_micros = 3000000            # $3 per run, best effort (SPEC §11)

[permissions]
allowed_tools = ["Edit", "Write", "Bash(cargo test:*)", "Bash(make test:*)"]

[trust."<authority hash from relais plan>"]
granted_at = "2026-09-20T00:00:00Z"
reviewed_by = "you"
```

Then `relais run --task task.json`. A worker refused a tool ends the run
`blocked (permission_denied)` naming the tool; nothing is escalated.

`docs/AUDIT-2026-09-20.md` is the audit this behaviour came out of, with
the findings still open.

## Known limits

- Claude Code 2.1.x has no turn ceiling flag; attempts and wall time
  are enforced by the runner, turns are not.
- On Windows the coordinator's endpoint is a loopback TCP port plus a
  nonce in the socket file rather than a Unix socket; the file's ACL is
  the permission restriction. Worker trees are killed with `taskkill`.

- The decomposition scheduler executes packages sequentially in
  topological order; waves of independent packages are computed and
  recorded but not yet run concurrently.
- Native Claude Code subagents are observed, not admitted: only managed
  dispatch through `relais run` is capped by the coordinator.
- The local `msrv` target proves the declared floor only when that
  toolchain is installed (`rustup toolchain install 1.88.0`); otherwise it
  says so. CI always installs it.
