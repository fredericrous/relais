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
| 17 | `recipe promote` refused: the comparison does not clear every gate; nothing was written |
| 18 | `trust grant` refused: the key is not this policy's; `relais trust show` prints the current one |
| 19 | `init --detect` found no verification command and none was typed; nothing was written |

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

From Claude Code with the plugin, there is nothing to set up first:
`/relais <task>` in any repository. The first time, the plugin asks two
questions in Claude Code's own dialog:

1. **Use these checks?** It shows the commands `relais init --detect` found
   in the repository: a Makefile `check`/`test` target, `cargo test`,
   `go test`, a `package.json` test script, pytest, with the lockfile's
   install as setup. On yes, it writes `relais.toml` and commits that file
   alone. When nothing is found, you type the command.
2. **Let relais run these commands, outside Claude Code's permission
   prompts?** It shows every command the grant covers. On yes, it records
   the grant in `machine.toml`.

Change `relais.toml` later and only the second question comes back, with
new or changed commands marked `+` or `~`. Answer *Not now* and nothing is
written. The plugin asks; the model can neither word the question nor
answer it, and the plugin refuses the model's own edits to `machine.toml`.

By hand, the same steps are:

```sh
cd the-repo && relais init --detect --write   # or plain `relais init` for the template; review, commit
relais trust show                             # the grant key and every command it would allow
relais trust grant --key <key> --reviewed-by <you>
```

Every command finds `relais.toml` upward from the cwd to the repository
root, so a subdirectory (or a task worktree, which carries its own copy)
works; a directory outside any repository is refused by name.

Machine-owned settings live in `~/.config/relais/machine.toml` and are
never written by a run. A trust grant is keyed by the PAIR of the
repository's authority hash and the repository itself (its canonical root
and, when git reports one, its `origin` URL), so editing `relais.toml`
voids the grant and the same declaration in another repository needs its
own review. `relais trust grant` writes it (atomically, under a lock,
keeping your comments and the file's mode); `relais plan` still prints the
block if you prefer to paste it. A worker is a
native agent: it runs with the tools its agent definition names and the
session's permissions, and no permission-mode flag is ever passed.
`disallowed_tools` here only ADDS to
the shipped deny floor (commit, merge, push, rebase, reset, tag and the
wrappers around them, plus `Agent`/`Task`: a worker never spawns subagents,
since model choice belongs to the route); it cannot shorten it:

```toml
schema_version = 1

[spending]
per_run_micros = 3000000            # $3 per run, best effort (SPEC §11)

[trust."<grant key from relais trust show>"]
granted_at = "2026-09-20T00:00:00Z"   # RFC3339, or a plain 2026-09-20
reviewed_by = "you"                   # required
repo = "git@github.com:me/the-repo.git"   # what plan filled in, for readers
```

Runs start from Claude Code (`/relais`, or the `mcp__relais__run` tool):
`relais run` is refused anywhere else. A worker refused a tool ends the run
`blocked (permission_denied)` naming the tool; nothing is escalated. A
`[permissions] allowed_tools` left in `machine.toml` is no longer read, and
`relais doctor` says so.

relais keeps no OS sandbox of its own: every dispatch is a native agent of
your Claude Code session and runs under that session's sandbox and
permissions. A `[sandbox]` section left in `machine.toml` is no longer
read, and `relais doctor` says so. SPEC §8 has the details.

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
- The local `msrv` target proves the declared floor only when that
  toolchain is installed (`rustup toolchain install 1.88.0`); without it
  `make msrv` FAILS and says so, rather than passing on a skip. Set
  `MSRV_SKIP_OK=1` to skip deliberately. CI always installs it.
- `install.sh` and `install.ps1` refuse to install a download they cannot
  verify (no `SHA256SUMS`, no sha256 tool). `RELAIS_SKIP_CHECKSUM=1` is
  the explicit way to accept an unverified binary.

`relais install --claude` writes the `/relais-verified-push` and
`/relais-architecture-conflict` skills and the three advisory agents
(`relais-research`, `relais-implementation`, `relais-review`), and installs the
relais plugin, `relais@relais-local`, which carries the mod, the agent
definitions (`relais-worker-<model>-<effort>` and the reviewer and planner ones)
and the `/relais` skill. The plugin is embedded in the `relais` binary, so a
relais installed from a release needs nothing else: install writes a directory
marketplace under relais's state directory (`claude-marketplace/`) and runs
`claude plugin marketplace add` then `claude plugin install` (once it is
installed, `marketplace update` then `plugin update`). The plugin carries
relais's version, so each release refreshes Claude Code's copy; `relais doctor`
(`plugin`) warns, naming `relais install --claude --write`, when it is absent,
disabled or at another version. `relais uninstall --claude` runs `claude plugin
uninstall` and `claude plugin marketplace remove` and removes the directory. A
`claude` call that fails is reported with its stderr and the command exits
non-zero. The per-model worker agent files and the `/relais` skill an earlier
relais wrote under `.claude/` are removed by the next install when you have not
edited them, and kept and reported when you have. `relais run` starts only
from Claude Code with the relais plugin, which runs it with `--protocol` and
`RELAIS_HOST=claude-code-mod`, so the worker shows as Claude Code's own agent;
from a plain terminal it is refused (exit 2), as it is on a Claude Code outside
the plugin's range (`>= 2.1.291, < 2.2.0`). `--native` is gone.
`--hooks` wires only the admission caps on your own `Agent`/`Task` spawns
(`Agent|Task`, no `WorktreeCreate`, no `SendMessage`): Claude Code makes its own
worktrees, and the plugin starts, continues and stops relais's agents. Installing
over a settings file from the hook-side native path (#171) removes relais's
`SendMessage` matcher and `WorktreeCreate` entry and keeps every entry that is not
relais's.

The `relais install --claude --hooks` integration — what it wires, its
handler timeouts, how to remove it, what `relais doctor` reports on it,
and the four limits specific to that path (subagents admitted but never
tracked as individuals, no ledger row for a coordinator restart to
adopt, no money cap on that path, and a cancellation that lasts only
until the next reconcile) — has its own document:
[`docs/INTEGRATIONS.md`](docs/INTEGRATIONS.md).
