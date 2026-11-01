+++
type = "research"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["amont", "relais", "homelab"]
+++
# Which gates in the fleet are platform-independent, and which repos are Linux-primary

## Question

For each repository that declares amont gates:

- Which gates are platform-independent? For relais: fmt, clippy and the cycle
  checks are candidates; `cargo-test` is likely macOS-sensitive under
  ADR-0041.
- Which repositories are Linux-primary? Candidates: homelab, services,
  trade-agents.
- What share of laptop check-seconds do the platform-independent gates
  account for, from the relais#367 per-check timings?

## Resolution

Explore research, 2026-11-01. The inventory and classification are below.
The share of laptop time is not known yet, and is left to t13.

- **Platform-independent by nature:** trade-agents `ruff-check`,
  `ruff-format` and `pyright`; duro `typecheck`, `lint`, `eslint` and
  `registry --check`; relais `module-cycles` and `child-cycles-*`; the fleet
  boilerplate `adr`, `traits` and `adr-lint-*`; the homelab `amont-homelab`
  yaml checks.
- **Platform-sensitive, stays on macOS:** relais `cargo-test` (183 `cfg`
  target hits; clonefile in `fsclone.rs`, `procs.rs`, the coordinator) and
  amont `cargo-test` (269 hits; `amont-slots`, `proctree`, `load`).
- **Unknown, to classify before declaring:** relais `plugin-test`, homelab
  `pytest-services` and `litellm-bootstrap`, stealth-fetch `pytest`,
  `run-tests-js` (application-landscape, kb-vision), `duro-doctor`, and the
  happier `yarn` gate.
- **Primary platform:** relais is macOS (AGENTS.md, ADR-0041). homelab,
  trade-agents and sre-agent run their CI on Linux, but none states a
  primary platform. No gate declares platform independence yet.
- **Share of load:** the relais#367 `check_timings` table holds one day (27
  rows on 2026-11-01). Of 3,100 gate-seconds that day, an unconfirmed happier
  `yarn` gate took about 65% and relais `cargo-test` (through amont) about
  27%. The labels are `<tool>@<hash>`, not gate names. Slot waits dominate
  (3,225 s for one yarn setup).
- **Candidate first slice:** the high-volume platform-independent gates,
  that is the JS/TS gates (happier and duro) if happier's gate is confirmed
  platform-independent, the trade-agents Python lint and types, and the
  relais cycle scripts. Rust `cargo-test` stays local.
- **Follow-ups, feeding t13 and t15:** add a gate name to `check_timings`;
  collect at least 7 days of it; confirm what happier's `yarn` gate runs.
