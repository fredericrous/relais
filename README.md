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
make check   # toolchain lint test msrv audit
make build
```

CI (`.github/workflows/ci.yaml`) runs the same targets on every push and
pull request: lint (fmt, clippy `-D warnings`, the module-cycle gate) on
**Ubuntu**; the test suite on **Ubuntu, macOS and Windows** (the
coordinator's endpoint and process handling are a different
implementation on Windows, and the product ships to a macOS workstation)
— including the release scenarios in
`crates/relais/tests/portable_scenarios.rs`, which drive the real binary
on all three; the ones in `release_scenarios.rs` need a `sh` fake worker
and stay unix-only;
the msrv build on Ubuntu, against the `rust-version` it reads from
`Cargo.toml`; and `make audit` on Ubuntu, also weekly on a schedule and
deliberately non-blocking. The release workflow's own audit job *does*
block, and runs `cargo test` on the tagged commit before publishing
anything.

A green check on a PR means what a green `make check` means on the
workstation. Locally, `msrv` fails rather than skipping when the pinned
toolchain is absent (`MSRV_SKIP_OK=1` opts out), and `audit` installs
`cargo-audit` if it is missing (`AUDIT_SKIP_OK=1` skips instead) — so a
green `make check` means all four gates really ran.

## Exit codes

One table, in `crates/relais/src/main.rs`, matched exhaustively. Codes are
split wherever a caller has to act differently.

| code | meaning |
|---|---|
| 0 | the command did what was asked |
| 1 | relais's own machinery failed (ledger, registry, a file it owns) — nothing is implied about the task |
| 2 | the invocation, or a file it named, is invalid |
| 3 | policy, trust, admission or the environment refuses; no model ran |
| 4 | the task executed and produced no acceptable candidate |
| 5 | the spending ceiling was reached; patch and evidence preserved |
| 6 | the run was interrupted; `relais resume` reconciles it |
| 7 | cancelled by request |
| 8 | needs a human decision |
| 9 | needs a human review |
| 10 | the run or artifact named is not on record |
| 11 | `resume` refused: a worker may still be running |
| 12 | `resume` reconciled the run; nothing was replayed |
| 13 | `--write` could not carry out part of its plan (the files are named) |
| 14 | nothing to train on yet |
| 15 | not enough records for one tier |
| 16 | the learner did not converge |

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
relais plan --task task.json        # prints the grant block to paste; blocked until trusted
```

Every command finds `relais.toml` upward from the cwd to the repository
root, so a subdirectory (or a task worktree, which carries its own copy)
works; a directory outside any repository is refused by name.

Machine-owned settings live in `~/.config/relais/machine.toml` and are
never written by a run. A trust grant is keyed by the PAIR of the
repository's authority hash and the repository itself (its canonical root
and, when git reports one, its `origin` URL), so editing `relais.toml`
voids the grant and the same declaration in another repository needs its
own review. `relais plan` prints the exact block to paste. A print-mode
worker cannot ask for permission, so the tools it may use are an explicit
machine-owned allowlist — nothing is granted implicitly, and no
permission-mode flag is ever passed. `disallowed_tools` here only ADDS to
the shipped deny floor (commit, merge, push, rebase, reset, tag and the
wrappers around them); it cannot shorten it:

```toml
schema_version = 1

[spending]
per_run_micros = 3000000            # $3 per run, best effort (SPEC §11)

[permissions]
allowed_tools = ["Edit", "Write", "Bash(cargo test:*)", "Bash(make test:*)"]

[trust."<grant key from relais plan>"]
granted_at = "2026-09-20T00:00:00Z"   # RFC3339, or a plain 2026-09-20
reviewed_by = "you"                   # required
repo = "git@github.com:me/the-repo.git"   # what plan filled in, for readers
```

Then `relais run --task task.json`. A worker refused a tool ends the run
`blocked (permission_denied)` naming the tool; nothing is escalated.

A verification worktree is a checkout of one revision and nothing else.
In a repository whose dependencies live in the tree (npm, pnpm, yarn,
bun, uv, poetry, bundler, composer), the profile's commands find no
`node_modules` or virtualenv there until a setup step puts one in place,
and a profile that declares none ends `blocked (baseline_unrunnable)`
with the block to add named. Go and Rust need no step. The setup is
declared per profile and, like the commands, is executable authority —
adding it changes the policy hash, so `relais plan` asks for a new
grant:

```toml
[[verification.profiles.default.setup]]
argv = ["npm", "ci"]
timeout_seconds = 600

[[verification.profiles.default.commands]]
argv = ["npm", "test"]
```

It runs first, in every worktree the commands run in — the base, the
task worktree, each candidate's copy: a three-attempt run is five
`npm ci`, and `cache_baseline = true` drops the base's. `relais doctor`
and `relais plan` warn when a lockfile is at the root and a profile
declares no setup:

```
 ! setup        package-lock.json present, and profile `default` declares no setup step: …
```

`docs/AUDIT-2026-09-20.md` is the audit this behaviour came out of, with
the findings still open. `docs/REVIEW-2026-09-21.md` is the crate-wide
review v0.2.0 answers: every finding with the pull request that fixed it
or the reason it was kept, and a scorecard re-measured afterwards.

## Known limits

- Claude Code 2.1.x has no turn ceiling flag; attempts and wall time
  are enforced by the runner, turns are not.
- On Windows the coordinator's endpoint is a loopback TCP port plus a
  nonce in the socket file rather than a Unix socket; the file's ACL is
  the permission restriction. Worker trees are killed with `taskkill`.

- The decomposition scheduler executes packages sequentially in
  topological order; waves of independent packages are computed and
  recorded but not yet run concurrently.
- Native Claude Code subagents are admitted but never tracked as
  individuals: with the hook wired (`relais install --claude --hooks`),
  a spawn is asked about and can be refused, and the end of its tool
  call gives the seat back — but relais never learns which agent ran
  under which spawn, so depth is not enforced on this path and a subtree
  cannot be cancelled. Without the hook, or with the coordinator
  unreachable, native subagents are observed only: managed dispatch
  through `relais run` is the sole path the coordinator caps.
- The local `msrv` target proves the declared floor only when that
  toolchain is installed (`rustup toolchain install 1.88.0`); without it
  `make msrv` FAILS and says so, rather than passing on a skip. Set
  `MSRV_SKIP_OK=1` to skip deliberately. CI always installs it.
- `install.sh` and `install.ps1` refuse to install a download they cannot
  verify (no `SHA256SUMS`, no sha256 tool). `RELAIS_SKIP_CHECKSUM=1` is
  the explicit way to accept an unverified binary.
- A hook-admitted spawn is governed only by the machine's own
  `[concurrency]` limits — the same caps the coordinator applies to any
  other run — and there is no separate machine setting for it. **No
  money limit is enforced on this path at all**: the run the hook
  registers carries no budget, and the coordinator refuses on budget
  only for a run that has one, so raising `dispatch_reserve_micros`
  changes what a spawn reserves against an unbounded total and still
  refuses nothing. A hook-admitted session is capped by agent count, not
  by spend.
- `relais cancel` on a session's own derived run holds for as long as
  the coordinator still remembers it. A cancelled run with nothing
  outstanding is reaped at the next reconcile — 15 seconds — after which
  the next spawn in that session finds no record, registers the run
  again and is admitted: the cancellation lives in coordinator memory,
  and nothing on disk lets a hook tell a reaped-cancelled run from a
  session it has never seen. Cancelling a tab's agents is not a
  durable stop; closing the tab is.
