+++
type = "task"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais"]
+++
# A pin row records the model the call ran

## Question

Since v0.81.0 an agent's `preferred-model` is a router pin. `decideSpawn`
writes the pin into `decision.model` (`claude-plugin/hooks/router.ts`,
the `reason: 'pin'` branch) while `routeSpawn` passes the call unchanged
(`routing.ts`, `next(e)` when the decision is not an override). So a
`router_decisions` pin row says the preferred model (`opus`, unresolved)
even when a marked fallback (no `model`) or a parent-written `model` runs
on something else. Should the row record the call's own model? Should a
pin carry its source: today `Pin` (`pins.rs`) is only `(agent, model)`,
`parse_definition` folds `model` and `preferred-model` into one value, and
`RouterState.pins` serves a bare string map, so neither side can tell an
agent's own `model:`, its `preferred-model` and a machine pin
(`MACHINE_PIN = "inherit"`, which definition pins overwrite) apart. A
source enum (`Own | Preferred | Machine`) cannot replace the string
values: `accepts` lists what `router-observe` takes from the plugin, so
it cannot protect an older plugin reading `router-state`. Keep `pins` as
the string map and add a field beside it (say `pin_sources`, the enum in
lowercase) that old plugins ignore, then retire `pins` once the plugin
floor reads the new field; `accepts` is only for the row's new `model`
field, which goes through `router-observe`. And should an unknown `preferred-model` value be
refused, as `model:` pins never were?

## Look at

- `router.ts` `decideSpawn` and `routing.ts` `routeSpawn`/`afterSpawn`:
  where the row's `model` comes from.
- `crates/relais/src/router/pins.rs`: `Pin` and `parse_definition`, where
  the source is dropped today.
- `crates/relais/src/router/state.rs`: `build` (the machine pins, then the
  definition pins over them), `RouterState.pins`, and `accepts`.
- Plan `docs/plans/2026-11-23-router-keeps-preferred-model.md` (Approach,
  "The decision row records the preference, not the model run") and its
  implementation review's three deliberates.

## Answer looks like

A plugin change where a pin row records the model the call ran (the call's
`model`, else the session model, resolved), with the pin source served
as an added `router-state` field (the string map kept), or a reasoned
no-change; and whether
an unknown preference value is validated, with the test that pins it.

## Resolution

Closed 2026-11-24 by the plan
`docs/plans/2026-11-24-pin-row-records-call-model.md`.

- **The row.** A pin row's `model` and `tier` are now the model the router
  infers the call ran. The order is the call's own `model`; else an own
  `model:` pin; else, for a preference or a machine pin, the session's model;
  with no source served (an older relais), the value recorded before. When
  nothing resolves, the pin's string is kept. The field and the wire are
  unchanged, so no `accepts` entry and no migration: no Rust code reads a
  pin row's `model`. Aliases expand by `DEFAULT_MODEL_IDS`, never the tiers'
  configured versions, and a `[1m]` suffix is dropped. The session model is
  asked for before `next(e)` without being awaited, then collected bounded,
  so the spawn never waits on it.
- **The source.** `router-state` serves `pin_sources` (`own`, `preferred`,
  `machine`) beside the unchanged `pins` map. `parse_definition` returns a
  `DefinitionPin` with its source. Retiring `pins` waits for a plugin floor.
- **An unknown preference is not validated.** A preferred pin's row records
  the session model, so a typo such as `opsu` cannot reach the row while the
  session model is known. It stays a pin, so the call is left untouched.
  `a_mistyped_preference_is_served_as_a_preferred_pin` (pins.rs) and the
  plugin case "a mistyped preference never reaches the row" pin this.
- **Still owed:** the live check after the plugin reinstall,
  [f22](f22-pin-row-live-check.md).
