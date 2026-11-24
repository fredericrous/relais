---
status: active
plan-id: p-01M87QDSGVBS561S2HPBZEE3WA
branch: feat/reads-greenspotter
repos: [relais, decisions]
adrs: [ADR-0072]
---
# relais reads greenspotter names

## Review panel
👉 **Decide:** none — approve if swapping one binary name and two format ids, then a release before t09, is the right size for t12.
📍 relais, decisions · 2 phases · 2 PRs · 1 release · next: Phase 1. Panel: backend, lang:rust, platform, tui, unix (opus); codex: not installed.
**Changed by review:** old-format test case; t09 waits on t10's sweep of relais; linux pin, host and exit-3 checks; rollback.
**Verdicts:** 5 approve-with-changes, then 4 approve and tui approve-with-changes; 3 carried; words 455 → 625 (×1.37, findings fixing unverifiable claims); remove 0, merge 0.
📄 Full reviews: [2026-11-24-relais-reads-greenspotter-names.reviews.md](2026-11-24-relais-reads-greenspotter-names.reviews.md)

## Picture

```
 greenspotter --candidates/1--> relais --ruling/1--> rulings.jsonl
 (env, else PATH)             (reviewer prompt)     (old rows stay)
```

relais swaps one binary name and two format ids; nothing else moves.

## Goal
relais spawns `greenspotter` where it spawned `adr-lint`, reads
`greenspotter.candidates/1`, writes `greenspotter.ruling/1`, and CI pins
greenspotter 0.20.0 from `greenharness/greenspotter`. Programme ticket
`topo-spotter-rename/t12-relais-reads-greenspotter`. It must ship and be
installed before t09, which removes `adr-lint` from PATH.

## Non-goals
- No `adr-lint` fallback, no `RELAIS_ADR_LINT` alias (ADR-0072: no aliases;
  t06: readers switch in one release).
- Old `adr-lint.ruling/1` rows in existing ledgers are not rewritten or read;
  greenspotter 0.20.0 `learn` skips them with a count.
- No rename of `adr-lint.toml`, `greengate.conf`'s pack block, or the
  `Cargo.toml` comment: t10's fleet sweep owns them.
- greentopo spawning is t14.

## Code review
- relais: backend, lang:rust, unix (tui left out: only tool names in existing gap/prompt texts change; architect left out: no new contract)
- decisions: none — programme Markdown only

## Behaviour
- The candidate pass runs `RELAIS_GREENSPOTTER`, else `greenspotter` from PATH,
  as `greenspotter candidates --base … --head … --json --max 40`, and accepts
  only format `greenspotter.candidates/1`; the old id is refused as an
  unexpected format, a gap of source `candidates`.
- Gap and prompt texts name greenspotter.
- Each ruling is a `greenspotter.ruling/1` line in `rulings.jsonl`.
- CI's `sync --check` step downloads `v0.20.0` of `greenharness/greenspotter`,
  checked against the release's SHA256SUMS value for the linux x86_64 tarball.
- Rollout: a laptop or pod running this relais without `greenspotter` on PATH
  gets a candidates gap (review still runs). So the release is installed on the
  laptop, and `greenspotter` is on PATH wherever relais reviews, before t09.
  relais's own `greengate.conf` pack block (`adr-lint-check`, severity block)
  still runs `adr-lint`, so t09 also waits on t10's sweep of relais.
- `RELAIS_ADR_LINT` is ignored: the release notes name its removal, and before
  t09 a grep of dotfiles and homelab manifests for it finds nothing.
- Rollback: revert Phase 1 and reinstall the previous release; this works only
  until t09. `greenspotter.ruling/1` rows written meanwhile stay in the ledger.

## Phases
- [ ] Phase 1 — add `adr-lint.candidates/1` to the bad-format test, re-capture
  `CANDIDATES_JSON` from real 0.20.0 output, run `cargo fmt`, clippy and fix
  "an greenspotter" wording from the sed; rename in `crates/relais/src/context/candidates.rs`,
  `crates/relais/src/context/mod.rs`, `crates/relais/src/runner/review/candidates.rs`,
  their tests, `docs/SPEC.md`, `.github/workflows/ci.yaml`; one commit
- [ ] Phase 2 — release, install on the laptop; check `command -v greenspotter`
  on every host and pod image that runs relais reviews (a miss marks t09 blocked); in the decisions programme write
  t12's Resolution in `docs/programmes/topo-spotter-rename/t12-relais-reads-greenspotter.md`
  and the Handoff in `docs/programmes/topo-spotter-rename/map.md`; one decisions commit

## Decision log
- 2026-11-24 — the old environment variable is dropped, not aliased.
- 2026-11-24 — the pilot of CI's pinned binary: `greenspotter sync --check`
  on the x86_64-apple-darwin 0.20.0 asset exits 0 in this tree. The linux x86_64 asset's checksum equals ci.yaml's pin,
  its layout (`<name>/greenspotter`) is checked before push, and
  `greenharness/greenspotter` readability from relais CI is confirmed on the PR.

## Verification
- `cargo test -p relais --lib candidates` green (484 s): format constant,
  old format `adr-lint.candidates/1` refused (new case in the bad-format test), fake `greenspotter` script spawned with the run's base and
  candidate, missing binary gap says "greenspotter could not be launched".
- Full `amont land` gates at push; CI `sync --check` green on the 0.20.0 pin.
- Phase 2: `relais --version` prints the release on the laptop; a relais review
  with the real greenspotter 0.20.0 on PATH lists candidates and records no gap; the real 0.20.0 run in a repository with no
  decisions corpus exits 3 and the receipt says `unavailable`.

## Outcome

<!-- panel: repos=relais,decisions adds= reviewers=backend,lang:rust,platform,tui,unix body-sha=859861b13e13 -->
