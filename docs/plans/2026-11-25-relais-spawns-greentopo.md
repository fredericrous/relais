---
status: active
branch: feat/spawn-greentopo
repos: [relais]
adrs: [ADR-0072]
plan-id: p-01M88CJWZ8R0EE977781BX6DPR
---
# relais spawns greentopo

## Review panel
👉 **Decide:** none — approve if a PATH probe with `aval` fallback, resolved once per run, is the right size for t14.
📍 relais · 2 phases · 1 PR · 1 release · next: Phase 1. Panel: backend (opus).
**Changed by review:** tests via pure `default_bin_in`; resolve once per run; no fallback on a failing greentopo; v0.82.0 reader floor; real-host check.
**Verdicts:** 1 approve-with-changes, then approve; 1 carried (picture lacks the resolve-once step); words 381 → 464 (×1.22); remove 0, merge 0.
📄 Full reviews: [2026-11-25-relais-spawns-greentopo.reviews.md](2026-11-25-relais-spawns-greentopo.reviews.md)

## Picture

```
 default_bin("aval")                      callers (context, verify, doctor,
 -------------------                      preflight, tooling probe)
   greentopo on PATH?  --yes--> "greentopo"  --+
        |no                                      +--> spawn / probe / label
        +--------------------> "aval" ---------+
 declared `bin = ...` always wins
```

The default binary becomes a PATH probe, and every caller reads it from one place.

## Goal
relais spawns `greentopo` where it spawns `aval` today, and falls back to
`aval` while only `aval` is on PATH, so the machine switch (decisions t09)
can remove `aval`. Programme ticket `topo-spotter-rename/t14-relais-spawns-greentopo`.

## Non-goals
- No change to the `[integrations]` keys read (t11 shipped both); no
  `RELAIS_GREENTOPO` env override (no other tool has one: a repository
  declares `bin = "…"`).
- No rename of the internal `Integrations.aval` field, the `aval` doctor/gap
  label, or the `AvalVerdict` types; the label stays `aval` (SPEC).
- The `aval` fallback is removed when decisions t10 closes (filed there).

## Code review
- relais: backend, lang:rust, unix, tui, architect

## Behaviour
- `tooling::default_bin("aval")` returns `"greentopo"` when `greentopo` is
  on PATH, else `"aval"`. A declared `bin` still wins. Every place that
  hard-codes `"aval"` as the program (`context/mod.rs` constraint rules,
  `relevant`, `decisions_for`, `aval_resolve` incl. its PATH-absence check,
  `verify::integration_gaps`, `doctor` component default) asks `default_bin`
  instead; `aval_resolve_in` resolves against the path it is given.
- `relais init` (`repo/detect.rs`) writes `greentopo = …` instead of `aval = …`; relais older than v0.82.0 refuses that key (`deny_unknown_fields`), so the CHANGELOG states v0.82.0 as the reader floor.
- The missing-integration blocker names the binary that was looked for.
- An installed `greentopo` that fails keeps each caller's handling (ADR-0073); it never falls back to `aval`.
- The binary is resolved once per run and carried as the context's `bin`; the tool version recorded in the context pack (`integration_version("aval")`) is that same binary's.

## Phases
- [ ] Phase 1 — code and tests in `crates/relais/src/{tooling,context/mod,context/guidance,verify/mod,doctor/mod,runner/preflight,repo/detect}.rs`, `docs/SPEC.md`, `CHANGELOG.md`; one commit
- [ ] Phase 2 — release and hand-off: tag via `tag-release`, then in the decisions repo write t14's Resolution in `docs/programmes/topo-spotter-rename/t14-relais-spawns-greentopo.md` and file the fallback removal in `docs/programmes/topo-spotter-rename/t10-fleet-sweep.md`

## Decision log
- 2026-11-25 — fallback to `aval` by PATH probe rather than a flag: hosts
  not yet switched (t09) keep working with one release, and no new config.

## Verification
- Unit tests through a pure `default_bin_in(name, dirs, extensions)` (no `set_var`; the existing `default_bin("aval") == "aval"` test at `tooling.rs:285` moves to it): only `greentopo` → spawned program is
  `greentopo`; only `aval` → `aval`; both → `greentopo`; declared `bin` wins;
  `aval_resolve_in` absence check follows the same choice.
- `relais init --detect` output contains `greentopo =` and parses back.
- After the release, on this host: `relais doctor` → the aval row names `greentopo`; with `aval` taken off PATH, one run → context pack version is `greentopo`'s and the run has no integration gap.

<!-- panel: repos=relais adds= reviewers=backend body-sha=8a0a64c8452a -->
