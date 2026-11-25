---
status: done
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
- [x] Phase 1 — code and tests in `crates/relais/src/{tooling,context/mod,context/guidance,verify/mod,doctor/mod,runner/preflight,repo/detect}.rs`, `docs/SPEC.md`, `CHANGELOG.md`; one commit
- [x] Phase 2 — release and hand-off: tag via `tag-release`, then in the decisions repo write t14's Resolution in `docs/programmes/topo-spotter-rename/t14-relais-spawns-greentopo.md` and file the fallback removal in `docs/programmes/topo-spotter-rename/t10-fleet-sweep.md`

## Decision log
- 2026-11-25 — fallback to `aval` by PATH probe rather than a flag: hosts
  not yet switched (t09) keep working with one release, and no new config.
- 2026-11-25 — "resolved once per run" (Behaviour, last line) became "chosen
  once by each reader of a run": the decision resolver (`AvalCli`, one
  `OnceLock`, passed to `aval_resolve_in` as `default_program`) and
  preflight's gaps and version (`RunEngine::aval_program()`, two lookups in
  one preflight). One stored value would cross `cli/run.rs`, which builds the
  resolver before the engine, and push `assemble_context` past clippy's
  argument limit; the readers agree while PATH does not change mid-run.
  CHANGELOG and SPEC say this. When t10 removes the fallback,
  `RunEngine::aval_program` and `AvalCli::default_program` go with it.
- 2026-11-25 — the doctor row label stays `aval` (Non-goals); its detail names
  the chosen binary, and with none installed reads "`greentopo` (or `aval`)".
- 2026-11-25 — one existence test, `tooling::exists_in_any_form`: a dangling
  or non-executable `greentopo` is chosen and fails loudly, never skipped.
- 2026-11-25 — plain `relais init` writes `greentopo` too, not only `--detect`.

## Verification
- Unit tests through a pure `default_bin_in(name, dirs, extensions)` (no `set_var`; the existing `default_bin("aval") == "aval"` test at `tooling.rs:285` moves to it): only `greentopo` → spawned program is
  `greentopo`; only `aval` → `aval`; both → `greentopo`; declared `bin` wins;
  `aval_resolve_in` absence check follows the same choice.
- `relais init --detect` output contains `greentopo =` and parses back.
- After the release, on this host: `relais doctor` → the aval row names `greentopo`; with `aval` taken off PATH, one run → context pack version is `greentopo`'s and the run has no integration gap.

Observed before the push (2026-11-25, tree `9c0adf4e0`):

| input | expected | actual |
|---|---|---|
| `default_bin_in("aval", dirs, ext, exists)` (now four arguments) over temp dirs: greentopo only / aval only / both / neither / dangling greentopo + aval | greentopo / aval / greentopo / aval / greentopo | as expected (`tooling::tests`, 5 tests) |
| `aval_resolve_in` with only a fake `greentopo` (exit 3) | not `NotInstalled`; verdict of exit 3 | `parse_aval_output(3, "")` (`a_greentopo_only_path_is_not_confirmed_absent`) |
| `aval_resolve_in(.., default_program = Some("aval"))`, PATH with `aval` (exit 0) and `greentopo` (exit 3) | aval spawned | exit-0 verdict (`the_callers_default_program_is_the_one_spawned`) |
| `integration_gaps(.., "greentopo")`, nothing installed | gap names `greentopo` | as expected (`verify::tests`) |
| built binary, `relais doctor`, `greentopo = "required"`, only a fake greentopo on PATH | aval row names greentopo | `✓ aval  required — `greentopo` greentopo 9.9.9-fake` |
| built binary, `relais doctor`, dangling `greentopo` link, no aval | blocks, exit non-zero | `✗ aval  required by [integrations] and `greentopo` (or `aval`) is not on PATH`, exit 3 |
| built binary, `relais init --detect --write` in a Cargo repo / `relais init` | `greentopo = "optional"` / `greentopo = "required"`, no `aval =` | as expected |
| `cargo clippy -p relais --all-targets -D warnings`; `cargo test -p relais --lib` | green | green (full lib run 523 s on `8d81a0010`, focused modules 461 s on `be17b615c`, tooling + context on `9c0adf4e0`) |

The after-release row (doctor and one run with `aval` off PATH on this host) is Phase 2's.

After the release (2026-11-25, relais v0.85.0 installed by brew; greentopo
2.0.0's released x86_64-apple-darwin binary on a scratch PATH, `aval` removed
from it):

| input | expected | actual |
|---|---|---|
| `relais doctor` in the relais checkout | aval row names greentopo | `✓ aval  required — greentopo greentopo 2.0.0` |
| `greentopo resolve execution.settle-before-asking --json` | answers like aval | `{"adr":"ADR-0012",…}` |
| one live run with `aval` off PATH | context pack version is greentopo's, no gap | not observable here: a live run inherits the plugin's PATH, where `aval` is installed. Unit tests cover it; decisions t09 checks it live after the machine switch |

## Outcome
Done 2026-11-25. relais#588 (spawn) and relais#591 (release) merged;
**v0.85.0** tagged on `d7b6044be`, Release run 45597824722 `success` on every
job, six platform assets plus `SHA256SUMS`, tap published. Installed on the
laptop. Decisions t14 closed with its Resolution; the `aval` fallback's removal
is in t10, the live context-pack check in t09. The declared-bin launch hint
(review low) is follow-up f29.

### 🧑 Your steps
- `relais install --claude --write` — because the relais Claude Code plugin is still at 0.84.0, and reinstalling it is the person's step.

## Implementation review
- implementation-review: approve-with-changes ×3 rounds; doctor row kept (label is `aval`), pack version and per-resolver choice fixed, tests added; plan edits here.
- backend, tui: approve-with-changes → approve / approve-with-changes; `deliberate: declared-bin launch hint says "on PATH" (low, follow-up filed)`.
- lang:rust, unix, architect: approve-with-changes → approve; `deliberate: two PATH lookups in one preflight; key-to-binary mapping spread until t10`.
- ~1.1M tokens over 15 reviews, about 9 min of review wall time.

<!-- panel: repos=relais adds= reviewers=backend body-sha=8a0a64c8452a -->
