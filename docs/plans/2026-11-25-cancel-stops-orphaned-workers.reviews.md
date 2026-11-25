# Full reviews: Cancel of an ended run stops its orphaned workers

Plan: [2026-11-25-cancel-stops-orphaned-workers.md](2026-11-25-cancel-stops-orphaned-workers.md).
All panel seats ran on opus-5-5; Codex on gpt-6-astra.

## Round 1 (body 38cb45c91c56)

### backend — approve-with-changes (36k tokens, 38 s)
1. [medium] The guard's `.catch` branch also returns the old text: pick the message in both branches. Accepted.
2. [medium] A plugin reload empties the store, so cancel would stop nothing and say nothing: say so in the reply and the description. Accepted.
3. [medium] Cancel latency and the list-failure and late branches are unstated and untested. Accepted.
4. [low] Rollback unstated. Accepted.

### lang:typescript — approve-with-changes (30k, 22 s)
1. [medium] The 30 s stop bound stacks on the 20 s cancel: state the worst case. Accepted.
2. [medium] No test for the late line or the list failure; the late case needs the scripted clock. Accepted.
3. [medium] A reaped driverless run takes the new path: state and test it. Accepted.
4. [low] Possible import cycle agents.ts ↔ runs.ts. Rejected: agents.ts imports nothing from runs.ts.
5. [low] "fails on origin/main's runs.ts" is wrong for the guard test. Accepted.

### tui — approve-with-changes (31k, 37 s)
1. [high] The deny promises what cancel cannot always do, and never names the run: name it when known, else send to the person. Accepted.
2. [medium] A reply with nothing stopped is indistinguishable from today's: a line for "none known" and "none running". Accepted.
3. [low] "30 s" hard-coded: build from `STOP_CONFIRM_MS`. Accepted.
4. [low] Tests for the late line, no-agent lines and the run id in the deny. Accepted.

### unix — approve-with-changes (24k, 15 s)
1. [medium] Reply delimiting: the CLI line first and byte-identical, agent lines after. Accepted.
2. [medium] Late text from the constant. Accepted.
3. [medium] A failed stop does not make the cancel an error. Accepted.
4. [low] Parallel stops: one bound in all; a two-agent test. Accepted.
5. [low] Repeat cancel idempotence. Rejected: the second-cancel test covers it.

## Round 2 (body cf649e24f561)

- backend — approve (36k, 28 s). All resolved. New [low]: an agent whose child key is still `starting-…` gets the wrong deny case. Carried into implementation (a third text pointing to `mcp__relais__status`).
- lang:typescript — approve (34k, 11 s). All resolved. [low] `.catch` wording for a transient list error: rejected, the `.catch` branch only denies agents the store holds. [low] confirm the reaped literal: carried.
- tui — approve (34k, 10 s). All resolved; two lows with no edit.
- unix — approve-with-changes (39k, 25 s). [medium] "20 s" unsourced: rejected, it is `CANCEL_TIMEOUT_MS` (`runs.ts:48`). [low] exact reaped literal `run R had no live driver; marked cancelled`: carried. [low] the fake clock drives `withinBound`: carried.

## Codex (gpt-6-astra) — approve-with-changes
1. [high] Ownership can change while the agent list is awaited: re-check just before each `TaskStop`, with a test. Applied.
2. [high] "stopped agent" claims more than a `TaskStop` answer shows: say `stop sent to agent <id>`. Applied.
3. [medium] The list read is not bounded: carried into implementation (the read shares the stops' window).

## Backend delta on the Codex edits (body d1c9bfbea897) — approve (41k, 24 s)
Both highs resolved. Carried into implementation: [low] the list read shares the single `STOP_CONFIRM_MS` window; [low] an agent the second check drops gets a line `agent <id> now belongs to another run; not stopped`; [low] two test lines still said "stopped".
