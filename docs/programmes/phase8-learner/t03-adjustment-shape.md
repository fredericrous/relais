+++
type = "grilling"
mode = "hitl"
status = "open"
blocked_by = ["t01-gate-inputs"]
repos = ["relais"]
+++
# What a learned adjustment is, and how the plugin applies it

## Question

What does `router-state` serve in `adjustments` for an activated stratum:
one tier step down from the stratum's table tier, a named model, or a
probability? Does an activated stratum keep exploring (draw around the new
tier), freeze, or explore up? Does an adjustment on a subagent stratum yield
to the parent's `model` preference or only to the person's pins? The
options go to the person with a recommendation first.

## Look at

- `crates/relais/src/router/state.rs` (`adjustments`, `epsilon`,
  `exploration_stopped`), `claude-plugin/hooks/router.ts` (state parser,
  `exploreAt`, `stepDown`), `docs/router-protocol.md`, session-router plan
  §5 and §6, SPEC §30.

## Answer looks like

A decision recorded in the Resolution and, since it changes the
router-state wire contract, a relais ADR; the ticket for the serving and
plugin plan follows from it.
