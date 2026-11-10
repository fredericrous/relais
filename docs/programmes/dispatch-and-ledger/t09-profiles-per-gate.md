+++
type = "research"
mode = "afk"
status = "closed"
blocked_by = ["t03-attempt-ends-on-amont"]
repos = ["relais", "amont", "amont-agent", "adr-lint", "sre-agent", "grh"]
+++
# Which repositories' relais profiles can split into amont gates

## Question

For each repository with a `relais.toml` and amont installed, which of its
profile commands are covered by an amont gate that `amont run --stamp` can
run, which parts stay plain commands, and does each one's verify environment
resolve the same toolchain binary as its push shell?

## Look at

- `/Users/fredericrous/Developer/Perso/*/relais.toml`: the
  `[verification.profiles.*]` commands.
- Each repository's `amont list --json` and `amont.conf`.
- relais `relais.toml:86-110`, the working example.
- t03's resolution: the gate-token and clean-HEAD conditions.

## Answer looks like

One row per repository: its current commands, the `amont run --stamp
<gate>` lines that replace them, what stays plain, and whether the token
condition holds (same resolved binary). Ordered by t02's check-hours, so
the build starts with the repository that pays most.

## Resolution

2026-11-10, Explore agent (`amont list --json` per repository; Makefiles
read for the first group only).

| Repository | Today | `amont run --stamp` | Stays plain | Toolchain |
|---|---|---|---|---|
| amont | `make check` | cargo-fmt, clippy, cargo-test | `test_bump_tap.py`, `--features test-hooks` tests, `check-no-deps.sh`, `attest check-inputs` | pinned (1.94.1) |
| amont-agent | `make check` | cargo-fmt, clippy, cargo-test (partial: `--workspace --locked`) | test-hooks clippy and tests, `test_bump_tap.py` | pinned |
| adr-lint | `make check` | cargo-fmt, clippy, cargo-test | toolchain, ast-grep rules, bump-tap, npm-test | pinned |
| grh | `make check` | cargo-fmt, clippy, cargo-test | nothing | pinned |
| aval, fleet-lsp | toolchain, deps, lint, test | cargo-fmt, clippy, cargo-test | toolchain, deps setup | pinned |
| sre-agent | `uv run pytest -q` with an env override | pytest | the env override (move it into `env`) | uv.lock; `uv` unpinned |
| homelab | pytest-services; manifests; bootstrap | pytest-services; go-test (maybe) | manifest validation, the Go wrapper | unknown |
| happier | `yarn verify` | none | everything | unpinned |
| Go operators, trade-agents, pnpm/npm repos | vet, test, typecheck, lint | go-test, pytest, run-tests-js (unverified match) | vet, typecheck, lint, pyright | packageManager only; Node unpinned |

**The Rust repositories split cleanly**, and they are where most check-hours
go (t02). The rest gain little: happier has no matching gate, and the JS,
Go and Python matches are unverified.

**Open for the build:**
- Whether a builtin gate's command line equals the profile's command, so
  the tokens match.
- Whether `amont run --stamp` takes a builtin gate's short name.
- Whether a brew `cargo` shadows the rustup shim in either environment
  (`which -a cargo`).

**Build order:** amont, amont-agent, adr-lint, grh, then aval and fleet-lsp;
sre-agent and homelab after.
