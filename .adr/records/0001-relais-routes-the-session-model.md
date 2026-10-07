---
id: ADR-0001
status: accepted
decisions:
  - key: routing.session-model
    choice: relais routes the model of the person's own Claude Code session, advisory and fail-open, through its plugin and inside an envelope the person grants once; it never re-routes its own workers
    first: true
    reason: The person asked relais to pick the cheapest capable model for the work in front of it and to recover from a weak pick on its own; switching the session's model is the only lever that reaches that work, and an envelope with independent gates is what SPEC §17 already requires of automatic activation
---
# 0001 — relais routes the session's model

## Context

Until now relais chose a model only for supervised runs: a task contract,
a worktree, verification (SPEC §6). Everything else the person does in a
Claude Code session ran on whatever `/model` said, and SPEC said so on
purpose: no transparent model proxy (§2), native delegation is the parent
model's choice (§3), and no classifier for every trivial request (§3).

The person asked for more (plan `docs/plans/2026-10-07-session-router.md`):
choose the cheapest *capable* model for the task at hand, recover from a
weak choice without being asked to "try again", and spend less per
completed task — measured, not assumed. The S0 spike showed the mechanism
works on the installed Claude Code: a `turn.step` override changes the
model mid-turn (full ids only), `agent.spawn` overrides a subagent's
model, `tool.call` exposes a failed check, and a classifier call at low
effort answers in about a second.

Every session runs with `--dangerously-skip-permissions`, so no
permission prompt protects anything here.

## Decision

The relais plugin routes the session's own model, and relais serves and
records it (SPEC §30):

- **Advisory.** The router picks a tier from a capability table
  (difficulty, scope, uncertainty, verifiability) and moves up a tier on
  evidence (a failed check after an edit, repeated failed repairs, a
  correction). It never guarantees a model, a dollar limit or acceptance
  for the session's work; SPEC §3's "advisory" label still holds.
- **An envelope, granted once.** Routing is `on` only when the person
  granted the envelope through the plugin's own question (`$.ui.ask`,
  written by `relais native router-envelope`) **and** an R3 evaluation
  passed and was recorded through the plugin's `/relais-r3`. Until both,
  it runs in `shadow`: it decides and records, and switches nothing. This
  is SPEC §17's "explicitly authorized envelope with independent
  evaluation gates"; §25's promotion rules are unchanged.
- **Fail-open.** Any error, timeout or unparsable answer leaves the
  request as it was (`next(e)`), or keeps the task's current tier — it
  never downgrades on missing information (SPEC §23).
- **relais's own workers are untouched.** A subagent is routed only when
  the router's own spawn decision created its task state; relais's
  supervised workers keep their rung's model (SPEC §6).
- **One classifier call per prompt, and its cost is reported.** The
  person's typed prompts only, one haiku call each at low effort; its
  tokens are recorded and priced in `relais report`'s session-routing
  section, on its own line, never hidden in another figure.
- **Spend is measured per completed task**, routed sessions against a
  randomized 10% hold-out, as an API-equivalent estimate with a cluster
  bootstrap interval; an unpriced model is unknown, never zero (SPEC §11).

## The stated limit

With bypass permissions the model can do anything the person's shell can.
The plugin honours the envelope and the R3 pass only when its own store
holds the consent records written after the person's answers; an envelope
or R3 row that appears without them leaves the mode at `shadow`, and the
plugin's `tool.check` guards refuse the model's writes to machine.toml,
`router-envelope`, `r3 --record` and the store file. That stops an
accidental or "helpful" self-grant. It does not stop a **deliberate** edit
of the plugin store's backing file from the shell: that is the accepted
limit, the same one SPEC §5 states for the trust grant. The CLI records
every envelope and R3 write with its source (`plugin-ask`, or
`cli (unattributed)`), so a write the person did not make is visible.

## Consequences

- SPEC §2 and §3 are amended to name this one exception to "no
  transparent model proxy" and "no classifier for every request", and
  §30 describes the router.
- `[session_routing]` joins machine.toml; every key is optional, so an
  older file parses unchanged — but an older relais refuses the new
  table (`deny_unknown_fields`), so relais is upgraded before the table
  is added.
- The ledger gains the router's tables (step v20). They hold tokens, never
  costs, and are never added to `orchestration_usage` totals.
