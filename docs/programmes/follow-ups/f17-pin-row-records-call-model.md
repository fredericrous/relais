+++
type = "task"
mode = "afk"
status = "open"
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
struct with a source enum (`Own | Preferred | Machine`) changes the
`router-state` wire, so a plugin that reads a string must be gated
through `accepts`. And should an unknown `preferred-model` value be
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
end to end behind an `accepts` entry, or a reasoned no-change; and whether
an unknown preference value is validated, with the test that pins it.
