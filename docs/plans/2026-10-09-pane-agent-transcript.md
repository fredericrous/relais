---
status: active
branch: feat/pane-agent-transcript
repos: [relais]
adrs: []
---
# relais pane: an agent row opens its transcript

## Review panel

👉 **Decide:** approve if this is the trade you want: clicking an agent row shows that agent's output inside the relais pane (messages cut to 12 rows, tool results to 3); Claude Code gives a plugin no way to open its own agent view, so a hint points to `↓ to manage` for the full view; a run's fourth agent and beyond stay unclickable. Backend's last three edits are left to implementation (no third round): the `rows` state variant carries an optional deny/slow `note`, a `slow` drawn while an older read is pending is not stored, the byte test asserts ≤ 140 000. Recommended: apply them as written.
📍 relais · plan reviewed (two rounds, a prompt-engineer pass you asked for, a backend delta) · next: P1–P3 as one PR on `feat/pane-agent-transcript`. Panel: backend, lang:rust, lang:typescript, tui, unix.
**Changed by review:** the transcript read is a guarded, generation-stamped task outside the flush with a 5 s bound and a flag on the real call; rows laid out at the pane's width and sanitised through `cutLine` before they are stored; digit hotkeys and `◂ in view` dropped as not asked for; one byte limit, one header definition; a deny after rows keeps the rows.
**Verdicts:** round 1: 5 approve-with-changes; round 2 (backend, twice): approve-with-changes, resolved; delta after your review (backend, twice): approve-with-changes, three small leftovers in the Decide line.
📄 Full reviews: [2026-10-09-pane-agent-transcript.reviews.md](2026-10-09-pane-agent-transcript.reviews.md)

## Context

The person runs three relais tasks and the pane (`claude-plugin/hooks/pane.ts`) shows three stacked timelines: phases, `agents ● worker a247…86da running`, cost. It shows none of what the agents do. Their words: "I don't see shit, just that there are relais process running. I want to see their outputs, if possible by clicking and navigating to the output page like in claude"; then "all I needed was to click at the bottom of my screen on the actual agent? maybe all that is missing is an affordance"; then "can our click in the pane behave like a click on the agent name at the bottom of the screen? affordance still needed but maybe different".

Facts the design rests on (Claude Code 2.1.295 plugin API, the plugin's own code):

- relais's workers are native agents (`agents.ts:handleSpawn` → `fx.agent.spawn`). Claude Code's tasks footer lists them as `relais:relais-worker-sonnet-medium …`, and `↓ to manage` opens one's transcript beside the conversation. The pane names the same agent `worker a247…86da`: nothing ties the two.
- No `$.ui` method switches the conversation view to an agent. `$.session.messages({ agentId })` returns the agent's rows (`SessionMessage`: `role: 'user' | 'assistant'`, `text`, `toolUses[{ tool, input, text?, isError? }]`), the newest 4096, or `{ deny: string }`; nothing bounds how long the call takes.
- A pane body is an engine-owned window over the tree the render hook draws: a taller tree scrolls, and the engine follows the end on the terminal. A `Button` is pressed by click or by Enter under the focus ring (Tab walks the pane's elements after a click or ctrl+x tab). `$.state` writes are refused while a render hook draws and allowed from an `onPress`. The body is 72–100 columns docked; every `Text` row is cut at the end.
- The plugin keeps one `$.state` value, `relais.pane`, written by the 100 ms pump (`register.ts` → `runs.ts:pump` → `ui.ts:flush`); `flush` returns early unless the models are dirty or a run is live. `Fx` (`fx.ts`) is the one place engine calls are spelled. Tests drive the real module through `tests/support.ts:scriptedEngine`, mount the pane with `$.ui.mount`, and press a Button with the mounted drawing's `press({ key })`.
- `AgentRow` (`timeline.ts`) has `kind`, `model`, `effort`, `status`, and `agentId` once `agents.ts:markAgent` learns it (also called by `handleContinue`, which has no type). The spawn line's `subagent_type` is not kept today. The runs view draws at most 3 agents per run (`MAX_AGENT_ROWS`) and a `+n more agents` row. Every tool call of an agent marks the models dirty, so a busy worker makes the flush write up to 10 times a second.

## Design

### What the person sees

**Runs view.** Today's layout; each known agent row is a `Button` (plain, keyed `agent:<agentId>`), and under it a dim row names the agent as the tasks footer does. Before the agent's id is known the row is plain text, with no name row. Under the last run's agents, once at least one row is pressable, one dim hint:

```
agents  ● worker a247…86da   running
          relais:relais-worker-sonnet-medium
        ● review 9b0c…11e2   completed
          relais:relais-review-opus-high
Click or Enter an agent: its transcript here · ↓ to manage: full view
```

Every row here and below is at most 70 cells before the width cut, so a 72-column body shows it whole. A run's fourth agent and beyond sit under `+n more agents` and are not pressable (`holds-until:` a run whose fourth agent matters; the `+n` row then becomes a list).

**Agent view.** Pressing an agent row replaces the pane's content at once:

```
[ back ]  Esc hands the keys back · ↓ to manage: full view
run 65d2…429e · worker a247…86da · running · sonnet@medium · 14:02
──────────────────────────────────────────────────────────
person │ Migrate application-landscape from npm to pnpm 10 so
       │ node_modules is hard-linked from the shared pnpm store
       │ … 9 more rows
agent  │ I'll start by reading package.json and the workflows.
▸ Read package.json
  │ {"name":"application-landscape", "version": "0.64.0",
  │ … 41 more rows
▸ Bash pnpm install --frozen-lockfile … running
```

- Row 1: `[ back ]`, a `Button` keyed `back` with `autoFocus` (Enter under the ring, or a click, returns to the runs view), then the keys hint. Row 2, the header: `run <short> · <kind> <agent short> · <status> · <model>@<effort> · <elapsed of the run>`; `<model>@<effort>` from `AgentRow.model`/`effort`, omitted when absent. Row 3: a rule as wide as the body.
- Then the transcript, newest at the end, one terminal row each, laid out at the body's width before it is stored: a user message's text as `person │` rows, an assistant's as `agent │` rows (continuation rows carry the bar alone), each message wrapped and cut at 12 rows with a dim `… n more rows`; a `▸ <tool> <what it touches>` row per tool use (the one-line text of `agents.ts:activityText`, cut at the end with `…`), then its result's first 3 rows dim and `… n more rows`. A tool use whose `text` is still undefined is in flight: its `▸` row ends `… running`. With `isError`, the first result row starts `FAIL ` in the error colour. A message with no text and no tool use is skipped. Every stored row passes through `timeline.ts:cutLine` (escape sequences, control characters, tabs) and is then cut at `columns × 3` bytes, so zero-width combining marks cannot push a row past the byte bound; the deny reason included.
- The tree is capped at 400 rows from the end (`holds-until:` a transcript the person reads whole: that one is the full view). The engine's window scrolls it and follows the end while the agent runs.
- Under the rule, instead of rows: `loading` until the first read lands; `transcript unavailable · <deny text>` on a deny; `transcript slow · ↓ to manage: full view` when a read passed its 5 s bound. Once rows have landed, a later deny or slow keeps the rows and adds that line, dim, under the rule (an agent that just finished often has no saved transcript). Any stored result, rows, deny or slow, counts as landed. A deny or a slow is also noted once on the run as a stderr line (`agents.ts:note`), so it shows in the status reply.
- Refresh: on each of the agent's tool calls and every second while it runs; once more when it finishes; then never. Runs that end or start meanwhile do not change the view; the status line keeps the run count.
- Escape hands the keys back to the prompt and leaves the view up (no `closeOnEscape`). The width used for the layout is the one at the press; a pane resized afterwards keeps it until the next read (`holds-until:` a person who resizes mid-view minds it).

### Where it lives

- **State.** `RelaisPaneState` (`types/index.d.ts`) adds `shown`, a discriminated union: `{ kind: 'runs' } | { kind: 'agent'; agentId; run; transcript }` with `transcript` one of `loading`, `rows` (the laid-out `{ text, dim?, color? }` rows), `deny` (reason) or `slow`. The render derives everything from the state; the engine's `props.view` is not read. A write is bounded by 400 rows × 100 cells × 3 bytes = 120 KB worst case (a 1-cell character is at most 3 bytes), a few KB typical.
- **Store** (`store.ts`): `shown` (the same union plus `generation`, `columns`, `refreshedAt`), `transcriptDirty`, `isReadingTranscript` (set around the whole attempt), `pendingMessages` (set while the real `session.messages` promise is unsettled), `notedReasons` (per generation). The store starts dirty, so a reload (module memory fresh, `$.state` kept) writes the runs view on its first tick even with no live run. `AgentRow` gains `type: string | undefined`, set by `markAgent` only when the caller has one.
- **Reading.** New `claude-plugin/hooks/transcript.ts`: `transcriptRows(messages, columns, cap)` is pure; `readTranscript(fx, agentId)` races `fx.session.messages({ agentId })` against `fx.clock.after(5000)` → `slow`, maps `{ deny }` and every throw to `deny`. `Fx.session` gains `messages`.
- **Refreshing**, outside the flush: `pump` calls `refreshTranscript(fx, store)` before `flush`. It does nothing unless `shown` is an agent, neither flag is set, and (`transcriptDirty`, or no read landed yet, or the row is running and a second passed since `refreshedAt`). It sets `refreshedAt`, clears `transcriptDirty`, sets both flags, and detaches the read: `isReadingTranscript` clears in a `finally` on the race, `pendingMessages` in a `finally` on the real promise; a result whose generation is stale is dropped, else stored and the store marked dirty. A view opened while `pendingMessages` is set draws `slow` until that call settles. `onAgentTool` and `showStatus` set `transcriptDirty` for the shown agent.
- **Flush gates** (`ui.ts:flush`) extend to "or an agent is shown", so a read after the run's outcome lands. A finished run's elapsed time is frozen at its duration, as today; a busy agent's tool calls make the shown view redraw up to 10 times a second, as the runs view does.
- **Pressing.** `layoutPane` stays pure; its rows are text rows or `{ button: { key, label, autoFocus?, press } }` with `press` either `{ kind: 'agent'; agentId; run }` or `{ kind: 'back' }`. `paneTree(fx, e, state, actions)` turns them into `Button`s whose `onPress` calls `actions.openAgent(agentId, run, columns)` (closing over the drawing's `bodyColumns`) or `actions.back()`. Both set `store.shown` (generation + 1; `openAgent` with `loading` and `transcriptDirty`) and call `flush` at once, which `onPress` may. The agent view is laid out by `layoutAgentView(shown, room)` outside `layoutPane`'s row budget.

### Non-goals

- A check-output page (the baseline's and verification's logs). No event names a check's log file today; adding `log_path` to `check_started` is the next plan. Until then the pane keeps its 40-line tail per check.
- A runs list, hotkeys, or marking the agent the person has open beside (`props.view.agentId`): not asked for.
- Switching Claude Code's own view to the agent: no API. The hint is the bridge.
- `/relais-status` taking the keyboard (`focus: true`): a click focuses the pane.

## Packages

One PR on `relais`, branch `feat/pane-agent-transcript`, three commits:

- **P1 — the affordance**: `AgentRow.type` from the spawn and the dim name row under each known agent.
- **P2 — the agent view**: everything under Where it lives, the hint line, the drawings for loading, deny and slow.
- **P3 — docs**: `claude-plugin/README.md` (the pane section), `CHANGELOG.md` (Unreleased), `amont agents-md` output if it changes.

## Verification

- `make check` exits 0; its `plugin` target runs `claude plugin validate claude-plugin` and `cd claude-plugin && claude plugin test` with every test passing.
- `tests/transcript.test.ts`, `transcriptRows` at 72 columns on a fixture of user, assistant and tool messages: rows in order; no row over 72 cells; a 30-line message → 12 rows then `… 18 more rows`; a 10-line result → 3 rows then `… 7 more rows`; a long tool row ends in `…`; `isError` → first result row starts `FAIL ` with `color: 'error'`; `text` undefined → `▸ … running`; a fixture holding `\u001b[31m`, `\r` and a tab → no byte under 0x20 in any row; a 4096-message fixture → the newest 400 rows; the same fixture of 1-cell 3-byte characters at 100 columns, and one of letters each followed by three combining marks → `Buffer.byteLength(JSON.stringify(state), 'utf8')` ≤ 130 000 for both.
- `tests/views/agent-view.test.ts`, through `scriptedEngine` and `$.ui.mount` (support gains a `session.messages` script with a settable delay, a never-settle mode and a call count): a spawned agent → a Button keyed `agent:<id>` and the dim `relais:relais-worker-…` row; an agent after `handleContinue` alone → no name row; an agent not yet known → plain text, no Button; a fourth agent of a run → under `+n more agents`, no Button; the hint appears once, only when a Button exists, at most 70 cells; `press({ key: 'agent:<id>' })` then a mount → `loading` with no tick; after a tick → header `run … · worker … · running · sonnet@medium · <elapsed>` and the scripted rows; scripted deny → `transcript unavailable · <reason>`, 1 call and exactly one stderr note over 10 ticks; rows landed, then the agent reported `completed` with the script now denying → the rows stay and one dim `transcript unavailable …` row is added; a read slower than 5 s → `transcript slow …` and one note; a read held across 3 ticks → 1 call; a never-settling read over 30 ticks → 1 call, then `press({ key: 'back' })` and reopen → `transcript slow …`, still 1 call; `back` pressed while a read is pending → the runs view stays after it lands, reopen → the second read lands; a tool call of the agent → the next tick re-reads (+1 call); 9 ticks with the clock advanced 900 ms and no tool call → no extra read; the agent reported `completed` → exactly one more read, none over 30 further ticks; a second run's outcome while an agent is shown → the view unchanged, the status line's run count updated; the view opened after the run's outcome → the read lands and the header's elapsed time stays at the run's duration; shown on a live run with the clock advanced 1 s → the header's elapsed time advances; stale `shown: agent` in `$.state` before a reload → the first tick writes the runs view; the texts `running`, `completed`, `FAIL ` and `… running` each present without reading colours.
- `tests/views/relais-pane.test.ts`: existing tests unchanged; at `bodyColumns: 72` every row of the agent view's first three rows fits whole.
- Live, guided preview on the person's terminal (an interface): `claude --plugin-dir claude-plugin` in a repository with a relais contract, docked at 72 and at 100 columns; start a run and `/relais-status`; click the worker row → its transcript, growing as the worker calls tools, the end followed; wheel up, then back; Tab to `[ back ]`, Enter → the runs view; Escape → the keys return to the prompt, the view stays; open the same worker with `↓ to manage` and compare; after the run, open a finished agent (the real deny case). Measured and recorded in the PR body with the screenshots: `session.messages` calls per minute with one running agent and with one denied finished agent (expected about 60 and 1), the largest state write in bytes and the writes per second with a busy agent shown, the read's latency on the longest transcript at hand, the layout time of the 4096-message fixture.

## Decision log

- 2026-10-09 — the person: the click in the pane should do what the footer's agent row does; an affordance is still wanted. No API switches the conversation view, so the transcript is drawn in the pane and the hint names the footer.
- 2026-10-09 — check-output page deferred: needs `check_started.log_path` in the protocol first.
- 2026-10-09 — the person asked for a prompt-engineer review as the user: digit hotkeys, the `b` key and `◂ in view` dropped (not asked for); one byte limit; the header defined; mock-ups made to fit 72 columns; the slow-read guard written once, in the Design.

<!-- panel: repos=relais adds=lang:typescript reviewers=backend,lang:rust,lang:typescript,tui,unix body-sha=c47feb01d562 -->
