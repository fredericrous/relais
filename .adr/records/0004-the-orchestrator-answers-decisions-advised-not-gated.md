---
id: ADR-0004
status: accepted
decisions:
  - key: decisions.who-answers
    choice: the orchestrating session may answer any decision, approve included, through the plugin's decide tool; every answer carries a typed actor (orchestrator, person, person-via-orchestrator) and how it was recorded (plugin, cli, legacy); an orchestrator never signs off a human-sign-off criterion, and only a TTY `person` answer or a plugin-recorded `person-via-orchestrator` answer counts as the person's judgment
    first: true
    reason: The orchestrator already settled most decisions but nothing recorded them, so 29 runs sat open and seven actor spellings made "who decided" unreadable; letting it answer is the person's decision (2026-10-11), and a receipt that claims a human signed off what a model decided is worse than a run that stays blocked
  - key: decisions.advisor
    choice: a decision advisor is an optional external command in machine.toml, absent by default, called once when a decision opens, its advice stored on the decision row and shown with the outcome; it advises and never gates, and failure means no advice, never a refusal
    first: true
    reason: The person wants Laya to inform approvals without deciding them (2026-10-11); SPEC §16 forbids an optional experiment from becoming a runtime requirement, so relais links no model, and a call on a read path would break the 1 s outcome freshness check
---
# 0004 — the orchestrator answers decisions, advised by a model, never gated by one

## Context

A run that cannot settle itself stops `needs_decision`, `needs_review` or
`interrupted` and opens a row in `decisions`; only `relais decide` closes
it. On 2026-10-11 the ledger held 29 open rows, the oldest from
2026-09-30, while 49 of 158 recorded answers came from the orchestrating
Claude Code session under seven different actor spellings. The plugin had
no decide tool and the outcome message never named the step, so recording
depended on a session remembering a shell command
(plan `docs/plans/2026-10-11-orchestrator-decides.md`).

The person decided that the orchestrator approves, and that Laya — the
decision model the learning loop trains on the homelab — advises an
approval and never gates it.

## Decision

**Who answers.** The orchestrator answers any decision through
`mcp__relais__decide`, `approve` included. relais's own refusals stand: an
approval with open verification gaps is refused whoever asks. Every answer
records a typed actor:

- `orchestrator` — the session decided;
- `person` — the person typed it, from a terminal (stdin is a TTY);
- `person-via-orchestrator` — the person said it, the orchestrator recorded
  it, with the person's words as `person_quote`.

Each answer also records `recorded_via`: `plugin` when the call carries the
session's nonce (sent on stdin, stored only as a hash, per session), `cli`
otherwise, `legacy` for rows written before this record. Only a TTY
`person` answer or a plugin-recorded `person-via-orchestrator` answer
counts as the person's judgment, for a human-sign-off criterion and for
any training label. This is tamper-evidence, not prevention: a model with
a shell can still evade it deliberately, and its row then says `cli`.

**The advisor.** An advisor is an external command named in
`machine.toml` `[decision.advisor]`, absent by default. relais writes one
JSON request on its stdin (objective, reason, detail, changed paths, diff
stats, verification summary; no file contents) and reads one JSON answer
with `p_approve`. It is called once, when a decision opens, outside the
write transaction, within a 10 s deadline and its own process group; the
result is stored on the decision row. No read path calls it. Its advice is
shown with the outcome before anyone answers; nothing branches on it.

This is consistent with SPEC §16 (relais links no model and needs none to
run) rather than an amendment of it.

**Relation to ADR-0002** (`learning.classifier-capture`). On the local
path nothing leaves the machine. On the homelab path the decision request
leaves the Mac, only to the person's own homelab over mTLS, and only when
the person configures that advisor command; this refines, and does not
replace, "nothing leaves except by an explicit export".

## Consequences

- `relais status` stops listing runs nobody is waiting on, once the
  stale rows are answered.
- The advisor trains only on answers that count as the person's, never on
  the orchestrator's, so it does not learn from its own consumer.
- A rollback of the schema step means restoring the backup the migration
  wrote; answers recorded since are replayed from the plan's sweep table.
