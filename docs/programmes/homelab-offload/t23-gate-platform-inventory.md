+++
type = "research"
mode = "afk"
status = "open"
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
- **Platform-sensitive, stays on macOS:** *(withdrawn 2026-11-11, see the
  Correction below)* relais `cargo-test` (183 `cfg` target hits; clonefile
  in `fsclone.rs`, `procs.rs`, the coordinator) and amont `cargo-test` (269
  hits; `amont-slots`, `proctree`, `load`).
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
  relais cycle scripts. ~~Rust `cargo-test` stays local.~~ (withdrawn, see
  the Correction below)
- **Follow-ups, feeding t13 and t15:** add a gate name to `check_timings`;
  collect at least 7 days of it; confirm what happier's `yarn` gate runs.

## Correction 2026-11-11: the Rust rows were wrong

The "stays on macOS" rows counted `cfg` hits and read them as macOS-only.
Most of those hits are `cfg(unix)`, which Linux satisfies. What the code and
CI show:

- **CI proves Linux, macOS and Windows for both repos.**
  - relais `ci.yaml` runs `cargo test` on `ubuntu-latest`, `macos-latest`
    and `windows-latest`. All three legs passed on main at 9fc94a0f
    (2026-11-11), the Linux leg in 5 min against about 14.5 min on the
    other two.
  - amont `ci.yaml` signs an attestation per platform (`x86_64-linux`,
    `aarch64-macos`, `x86_64-windows`) and runs its hooks suite on
    ubuntu and macOS. On a pull request run (release 3.1.0, 2026-11-11)
    `hooks (ubuntu-latest)` and `hooks (macos-latest)` both passed. Its
    main runs skip those legs when a signature already covers the tree.
- **The platform-specific code is thin.** `target_os` appears in three
  relais files (`procs.rs`, `coordinator/mod.rs`, one test) and in five amont
  files (`proctree.rs`, `load.rs`, `snapshot_prep.rs`, `greenharness-slots`,
  one test). relais already clones files copy-on-write on both platforms
  (`clonefile` on macOS, `FICLONE` on Linux) and reads process start times
  from `/proc` or `sysctl`.
- **What Linux cannot prove** is the macOS branches themselves, and
  relais's Unix-only parts mean Windows is not a relais target.

So `cargo-test` and clippy are candidates for `platform=any` on the Linux
verdict, with a macOS attestation as an extra verdict owed before a release
(CI already signs one), not a reason to keep the gate local. Whether each
Rust gate qualifies, and which verdicts a commit, a push and a release owe,
is open again below.

## Look at

- `.github/workflows/ci.yaml` in relais and amont: the platform matrix and
  the attest steps.
- `crates/greengate-runtime/src/hooks/rust_tools.rs` in amont: the built-in
  `cargo-fmt`, `clippy` and `pre-push-cargo-test` argv.
- Each repository's `amont.conf`: the `cargo-test` gate and its scope.

## Answer looks like

One row per Rust gate (`cargo-fmt`, `clippy`, `cargo-test`, per repository):
may it declare `platform=any`; which attestation platforms a commit, a push
and a release each owe; and the wall time of its Linux leg in CI. Then one
line on whether t31's cache is needed for the first Rust slice.
