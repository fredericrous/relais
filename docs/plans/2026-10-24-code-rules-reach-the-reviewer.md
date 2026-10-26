---
canonical: decisions:docs/plans/2026-10-24-code-rules-reach-the-reviewer.md
phases: [4, 7]
status: active
branch: feat/adr-lint-pilot
---
# Code rules reach the reviewer (pointer)

Canonical plan: [decisions/docs/plans/2026-10-24-code-rules-reach-the-reviewer.md](https://github.com/fredericrous/decisions/blob/main/docs/plans/2026-10-24-code-rules-reach-the-reviewer.md) (ADR-0044).

This repository carries relais's half of Phase 4 (behaviour 3):
- the reviewer prompt lists adr-lint's candidates for the candidate commit (`adr-lint candidates --base <base> --head <candidate> --json`), as a new `GuidanceSource`;
- a missing or failing adr-lint is a receipt gap, `candidates missing (…)`, and never a silent drop;
- the receipt counts the dispositions (violation, fine, undisposed);
- each ruling is written as an `adr-lint.ruling/1` record with `tool: relais`.

This repository also carries the Phase 7 pilot (behaviour 8, step 1), Rust only, because `claude-plugin` has no Node toolchain:
- vendor adr-lint's `amont.pack` row (`amont add github:fredericrous/adr-lint`), at `warn`;
- `adr-lint sync`: clippy `allow_attributes` and `allow_attributes_without_reason` at `deny` in `[workspace.lints.clippy]`, with `lints.workspace = true` in the member;
- the nine reasonless `#[allow]`: eight become `#[expect(…, reason = "…")]`, and one, `type_complexity` on a type alias, is removed, because clippy does not raise that lint on an alias;
- `adr-lint sync --check` runs in CI.

## Verification

Input → expected → actual:
- `make lint`, `make test` → pass → pass (2240 tests passed, 0 failed); the commit gate → 30 checks pass → pass, after the fake adr-lint was primed (`test_support::prime_script`). Unprimed, a freshly written script lost its first exec to the 5 s guidance timeout under load once.
- The captured adr-lint 0.3.0 document, a wrong `format` token, an uppercase id and a 63-hex id → parsed / Unparseable gap ×3 → as expected (context/candidates.rs tests).
- A fake adr-lint that exits 3, and a missing binary → `candidates missing (` in the receipt note, the reviewer still dispatched, `CandidatesRecord::Unavailable` → as expected.
- A two-part review → each candidate listed in exactly one part → as expected. Three listed candidates with 2 ruled → `undisposed: 1` → as expected. `rulings.jsonl` read back → the field shapes of `adr-lint.ruling/1` → as expected.
- The real adr-lint 0.4.0, run from the primary checkout (on `main`, a different working tree) with `--base a60e1701 --head be5b8d99` → the 4 ids amont-agent's brief listed with the commit checked out → the same 4 ids, at the same paths and lines. So the repository directory sees the candidate commit, and text and lines come from `--head`, in every workspace mode.

## Implementation review

- approve-with-changes (1 blocking, 1 medium, 1 low), then approve on the Delta; 70k + 80k tokens, 117 s + 27 s.
- Fixed:
  - the `stopped_run(.., bool)` flag argument is split in two. adr-lint listed this as a candidate, and the reviewer ruled it a violation: the first real catch;
  - a ruling's `repo` is the main checkout's name;
  - SPEC §7 says what `repo` and `tree` hold.
- The reviewer ruled the other 3 candidates `fine`; their ids stayed the same across both rounds.
- After that approval, relais#307's Windows test job failed to compile: the fake adr-lint is a shell script, written with Unix-only APIs. The fix gates only what needs the script with `#[cfg(unix)]`: the exit-3 test and the argv test. The missing-binary test and the spawning resolver run on every platform; the reviewer asked for that split. Both re-bindings: approve-with-changes, then approve; 84k + 90k tokens.
- The relais runs: `run-65ea960a85370-d257` was approved after its parts review, then applied by hand (an approval does not apply the candidate), with its one citation fix made by hand.

## Verification (phase 7, pilot)

Input → expected → actual:
- `adr-lint sync --check` on origin/main → exit 1, with three gaps → exit 1: both keys and the member's `lints.workspace`.
- `adr-lint sync`, then `cargo clippy --all-targets -- -D warnings` → 16 errors from the 8 compiled `#[allow]` → 16 errors (8 `#[allow] attribute found`, 8 without a reason). After the `#[expect]` conversions → exit 0.
- **Probe 1.** In a scratch copy of HEAD, an added `#[allow(dead_code)] fn adr_lint_probe` → clippy refuses it → exit 101, `#[allow] attribute found` and `allow attribute without specifying a reason`.
- **Probe 2.** The same copy, with `allow_attributes_without_reason = "deny"` deleted → `sync --check` fails → exit 1, `./Cargo.toml: clippy.allow_attributes_without_reason is missing`.
- The commit gate → passes → 32 checks on 9bde16fb (with `adr-lint-check`, after `amont trust` of the new pack block, which the person asked me to run), 31 on bbda83dd.
- The CI step: the 0.8.0 tarball is pinned by its SHA-256, and `tar tzf` shows `adr-lint-0.8.0-x86_64-unknown-linux-gnu/adr-lint`.
- A known limit: on a 32-bit target whose `fsblkcnt_t` is u64 (musl, FreeBSD), procs.rs's `cfg_attr` leaves `useless_conversion` unexpected. No CI job lints such a target.

## Implementation review (phase 7, pilot)

- **Verdicts.** approve-with-changes, then approve on the Delta; 59k + 64k tokens, 161 s + 21 s.
- **Fixed.**
  - The relais worker gave six `too_many_arguments` sites a false reason, "recorded fields". Each now reads "an existing arity, kept until these parameters are grouped into a struct".
  - The procs.rs reason names `f_frsize`, and `f_bavail` on Linux.
  - The CI comment says why the step runs only in CI.
  - The probes above.
- **Deliberate.** The `holds-until:` marker the review suggested is not added, because an arity has no ceiling under load (change.a-deferral-names-its-ceiling). These six suppressions have their cause inside the repository; parameter structs are not scheduled.
- **Relais run.** `run-65ebd1f5fcd99-baff` was accepted and later corrected: one false reason was fixed by hand and recorded with `feedback`.

