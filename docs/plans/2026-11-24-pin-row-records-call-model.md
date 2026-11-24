---
status: active
branch: prog/follow-ups/f19-pin-row-records-call-model
repos: [relais]
adrs: []
plan-id: p-01M87AN8JHS26AV0CKD19GH4HS
---
# A pin row records the model the call ran

## Review panel
📄 Full reviews: [reference](2026-11-24-pin-row-records-call-model.reviews.md).
👉 **Decide:** none — approve if a pin row naming the model the router infers the call ran (not one it observed) is enough.
📍 relais · 2 phases · 1 release · 1 step for you · next: Phase 1. Panel: backend, rust, typescript, tui, unix (opus-5-5), codex.
**Changed by review:** the unknown-preference check is dropped (a typo cannot reach the row); the pilot is isolated; the session-model read is bounded.
**Verdicts:** all approve after the delta; 10 carried (extra cases, live-check wording, tier-version alias, read after `next`); words 1421 → 1590 (×1.12); remove 1, merge 0.

## Picture

```
 today                             after this plan
 -----                             ---------------
 router-state                      router-state
   pins: rev=opus, m=inherit         pins: rev=opus, m=inherit (kept)
                                     pin_sources: rev=preferred,
                                                  m=machine
       |                                 |
       v                                 v
 spawn rev, no model,              spawn rev, no model,
 session on sonnet                 session on sonnet
       |                                 |
       v                                 v
 row: pin, model "opus"            row: pin, model claude-sonnet-5-5
 (the preference)                  (call model, else own, else session)
 row for m: model "inherit"        row for m: the session model
```

The decision row of a pinned spawn stops naming the pin's value and names
the model the call ran; router-state says where each pin came from, beside
the string map old plugins read.

## Context
Follow-up ticket `docs/programmes/follow-ups/f19-pin-row-records-call-model.md`.
Since v0.81.0 a definition's `preferred-model` is a router pin. `decideSpawn`
(`claude-plugin/hooks/router.ts:1396-1402`) writes the pin into
`decision.model`, and `routeSpawn` (`claude-plugin/hooks/routing.ts:404-406`)
passes the call unchanged (the pin is `override: false`). So a
`router_decisions` pin row says `opus` (the unresolved alias) even when the
call ran on something else, and a machine pin row says `inherit`, which is no
model at all. `parse_definition` (`crates/relais/src/router/pins.rs`) folds
`model:` and `preferred-model:` into one value, and `RouterState.pins`
(`crates/relais/src/router/state.rs:119`) is a bare string map, so neither
side can tell an own `model:`, a `preferred-model` and a machine pin apart.

Facts checked:
- **Precedence.** The Agent tool's own schema says its `model` parameter
  "takes precedence over the agent definition's model frontmatter". With no
  call `model`, an own `model:` runs. With no call `model` and no own
  `model:` (a preference, or `inherit`), the subagent runs the parent's
  model. The 20 latest pin rows in the ledger, joined to their subagents'
  `router_usage` step models, all match the pin. None of them separates the
  cases: the sessions ran on the pinned family.
- **Readers of a pin row's `model`.** No Rust code reads
  `router_decisions.model`. The exploration join reads relation, explored,
  applied, propensity and holdout. The holdout window reads session, holdout
  and applied. `relais report`'s routing section reads `router_usage`. The
  learner never reads pin rows. The router pin plan's 11-30 cohort check
  reads `reason` (pin against table), not `model`.
- **A mistyped preference today** (`preferred-model: opsu`) is served as the
  pin `opsu`. The call stays untouched, and the row records `opsu`.
  Refusing it with `PinError` would skip the whole definition
  (`router/mod.rs:160-164`). The agent would then become routable, and the
  router could override a model the person meant to keep. That is the
  failure the comment at `router/mod.rs:135` warns about.

## Approach
- **The row's `model` and `tier` become the model run, resolved.** No new
  row field, so no wire change, no `accepts` entry and no migration. An old
  relais accepts the row as it is. Rule, in the plugin, for a pinned
  spawn:
  1. the call's own `model`, resolved (one that does not resolve falls
     through to the next step);
  2. else, for an `own` pin, the pin's value, resolved;
  3. else, for a `preferred` or `machine` pin, the session model
     (`fx.session.model()`), resolved;
  4. else, when router-state carries no source for the type (a relais
     older than this one), the value written today: the pin, or the
     session model when the pin is `inherit`.

  When nothing resolves (no session model, an unknown id), the row keeps
  the pin's string as today. `tier` is `tierOfModel` of the recorded model,
  else `implementation`, as today. A person-named subagent model does not
  enter: a pin outranks it (SPEC §30).
- **`pin_sources` beside `pins`.** `RouterState` gains
  `pin_sources: BTreeMap<String, PinSource>`, and `PinSource` is `own`,
  `preferred` or `machine`, serialized lowercase. `build` writes `machine`
  for `[session_routing] pinned_agents`, then each definition's source over
  it, as `pins` already does. `pins` stays the string map, so an older
  plugin reads router-state unchanged. Retiring `pins` waits until the
  plugin floor reads `pin_sources`; this plan does not do that. The plugin
  parses `pin_sources` and drops any value it does not know.
- **`parse_definition` returns the source,** as a `DefinitionPin { agent,
  model, source }`. An own `model:` other than `inherit` gives `own`, else
  a `preferred-model:` gives `preferred`. Both keep the rule that the order
  of lines does not matter.
- **A mistyped preference stays as it is today.** `preferred-model: opsu`
  is still served as the pin `opsu`, with source `preferred`. The row then
  records the session model (rule 3), so the typo cannot reach the row, and
  the agent stays pinned (its call untouched). So no validation, no warning
  and no alias list in Rust: the ticket's question is answered "not
  validated", and the `opsu` test below pins that.
- **f20 overlap.** f20 (`relais-wt-f20`, plan only so far) edits
  `routeSpawn`'s start and adds `RouterMemory` fields. This plan's plugin
  hunks are `decideSpawn`'s pin branch and one call in `routeSpawn`. Phase 1
  touches no f20 file. Phase 2 starts from a rebase on `origin/main`, and
  whichever branch lands second rebases on the other.

## Phases
- [x] Phase 1 — the pin carries its source. `crates/relais/src/router/pins.rs`
  (`PinSource`, `DefinitionPin`, the doc
  comments, the tests under Verification); `crates/relais/src/router/mod.rs`
  (`definition_pins` returns `Vec<DefinitionPin>`);
  `crates/relais/src/router/state.rs` (`Inputs.definition_pins`,
  `RouterState.pin_sources`, `build`, the tests: pins and sources join, and the serialized `pin_sources` values are
  `"preferred"` / `"machine"`, the
  document-keys test lists `pin_sources`); `crates/relais/src/install/mod.rs`
  (the template-pin test expects source `preferred`). Done through relais:
  one `kind: change`, acceptance `cargo test -p relais --lib router::` and
  `cargo test -p relais --lib install::`.
- [x] Phase 2 — the row records the model run. `claude-plugin/hooks/router.ts`
  (the `pin_sources` type and its parse; `decideSpawn` takes `callModel` and
  `sessionModel` and records the model run in its pin branch);
  `claude-plugin/hooks/routing.ts` (`routeSpawn` passes `e.model`, and
  `fx.session.model()` only when the type is pinned, read as
  `Promise.resolve().then(() => fx.session.model()).catch(() => undefined)`,
  bounded with `within(fx, …, waitFor(remainingMs))` as the spawn
  classifier is (`routing.ts:395`); a timeout means no session model, so
  the read can neither throw nor stall `next(e)`. A `routedSession` test
  with a never-settling `session.model` sees `next(e)` run once with the
  unchanged call and the row keep the pin's string);
  `claude-plugin/tests/router.test.ts`, `claude-plugin/tests/routing.test.ts`,
  `claude-plugin/tests/routing-support.ts` (the cases under Verification);
  `docs/router-protocol.md` (the `pin_sources` field, what a pin row's
  `model` holds), `docs/SPEC.md` (§30's pin sentence), `CHANGELOG.md`
  (Unreleased). Done through relais: one `kind: change`, acceptance
  `make plugin`.

### 🧑 Your steps
- Reinstall the relais plugin with relais's own install command for Claude — the plugin half records the model run only once the installed plugin carries it (installed: 0.72.0).

## Non-goals
- Observing the model actually run: the row records the model the router infers (call model, own model, else the spawning session's model). A spawn made by a subagent on another model, or a mid-session `/model` switch, can differ; `docs/router-protocol.md` says so.
- Retiring `pins` from router-state; it waits for a plugin floor.
- Validating an own `model:` value.
- A per-row `pin_model` field. No reader needs the preference stored per
  row, since the definition holds it.
- Rewriting past rows.

## Code review
- relais: backend, lang:rust, lang:typescript (unix, tui left out: one added JSON field, no command, flag or message change)

## Delivery
One worktree and one PR from `origin/main`, merged on green. Then release
v0.82.0 through the `tag-release` skill and run `brew upgrade relais`:
router-state is served by the binary. The plugin half needs the relais plugin
reinstalled, which the follow-ups handoff already lists as the person's step
(the installed plugin is 0.72.0). The live row check runs in the first
session after that reinstall, and is filed as a follow-up ticket with a due
date.

## Verification
- `cargo test -p relais --lib router::` covers:
  - `model: sonnet` → `own`;
  - `inherit` + `preferred-model: opus` → `preferred`;
  - `preferred-model: opsu` → pin `opsu`, source `preferred`;
  - a machine pin overwritten by a definition takes the definition's source,
    and a machine pin alone is `machine`;
  `cargo test -p relais --lib install::` passes with source `preferred`.
- `make plugin` covers, for a pinned spawn's row:
  - preferred `opus`, no call `model`, session `claude-sonnet-5-5` → model
    `claude-sonnet-5-5`, tier of sonnet;
  - the same with call `model: "opus"` → `claude-opus-5-5`;
  - own `sonnet`, no call `model`, session opus → `claude-sonnet-5-5`;
  - machine `inherit`, session haiku → `claude-haiku-5-5`;
  - a state with no `pin_sources` → today's value (`opus`), so an old
    relais records what it did;
  - no session model and no call `model` → the row keeps the pin string;
  - a `pin_sources` value the plugin does not know is dropped, and that
    agent takes rule 4;
  - `session.model` rejecting → the spawn is passed unchanged and the row
    keeps the pin's string;
  - an unresolvable call `model` (`opsu`) falls through to rule 2 or 3;
  - every case stays `reason: 'pin'`, `applied: false`, and the call is
    passed unchanged.
  The call-model and session-model cases run end to end in
  `routing.test.ts` (`routedSession`, `engine.script.sessionModel` set,
  the row read with `observed()`), not only against `decideSpawn`.
- Piloted, with the built binary: in a scratch directory with
  `.claude/agents/typo.md` (`model: inherit`, `preferred-model: opsu`) and
  `.claude/agents/rev.md` (`preferred-model: opus`), run `(cd <scratch> &&
  CLAUDE_CONFIG_DIR=<scratch>/cfg RELAIS_STATE_DIR=<scratch>/state
  <built relais> native router-state --session pilot-f19)` (router-state
  takes only `--session` and reads the working directory; the two variables
  keep the person's agents and real ledger out). It exits 0, its stdout
  parses as JSON with `pins` holding `typo: opsu` and `rev: opus` and
  `pin_sources` both `preferred`, its stderr is empty, and the real
  ledger's mtime is unchanged.
- Live, after the reinstall (filed follow-up), also count pin rows whose `model` is still the raw pin string (expected 0 for new-plugin rows) and compare nested spawns' rows to their step models;
- Live, after the reinstall (filed follow-up): a `relais-review` launch with
  no `model` in a session on a non-Opus model writes a pin row whose `model`
  equals the subagent's `router_usage` step model.


### Observed

| Check | Input | Expected | Actual |
|---|---|---|---|
| `cargo test -p relais --lib router::`, `install::` | Phase 1 candidate | pass | pass (47.6 s, 2.2 s); clippy `-D warnings` and `cargo fmt --check` clean |
| `make plugin` | Phase 2 | pass | 555 pass, 0 fail (the script's one rerun: an unrelated chunk-matching test timed out at 5 s under load) |
| Red check | the row rewrite disabled | the new cases fail | 8 fail; the 4 whose expected value is today's still pass, as they should |
| Pilot | scratch `typo.md` (`opsu`), `rev.md` (`opus`), isolated `CLAUDE_CONFIG_DIR`/`RELAIS_STATE_DIR` | exit 0, pins and sources as planned, empty stderr | exit 0; `pins` {rev: opus, typo: opsu}; `pin_sources` both `preferred`; stderr 0 lines; the scratch state directory stayed empty |

## Decision log
- 2026-11-24: Phase 1 ran through relais; both its verify runs blocked on `admission_wait_exhausted` (a jammed host, 5 waiters). The kept candidate was applied unchanged and checked in session; the run is recorded `salvaged` at `f96b007e1`.
- 2026-11-24: Phase 2's relais run got no worker lease in 1800 s (`nothing was launched`), so Phase 2 was written by hand in session.
- 2026-11-24: carried findings taken in Phase 2: the session model is read in `afterSpawn` after `next(e)` (backend), a `TIMEOUT` maps to undefined (typescript), aliases expand by `DEFAULT_MODEL_IDS` not the tiers (Codex 2), and the own `opsu`, preferred `opsu` and preferred-source rejecting cases (rust, typescript, backend).

<!-- panel: repos=relais adds= reviewers=backend,lang:rust,lang:typescript,tui,unix,codex body-sha=3298939a5d52 -->
