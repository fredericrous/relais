---
status: done
branch: feat/pane-programme-and-clear-tail
repos: [relais]
plan-id: p-01M7V1BEK3NQ7D6VKEGB7ABVNV
---
# The relais pane shows the session's programme and, after /clear, what came before

## Review panel

👉 **Decide:** a mid-ticket `/clear` with no handoff shows no programme (relais deletes `work` claims at SessionEnd). Keep that (recommended), or show the old view for a turn?
📍 relais · 4 phases · 1 release · next: worktree, then Phase 1 (`relais programme session --json`). Panel: architect, backend, lang:rust, po, tui, unix (opus), codex.
**Changed by review:** only a `next --claim` handoff survives `/clear`, said everywhere and proved live; the map is found by `--dir`/repo key, not the display path; reads and tail bound to session + seq.
**Verdicts:** round 2: 5 approve-with-changes, rust approve; Codex 2 highs applied; 20 carried; words 1539 → 2294 (×1.49, blocking/high/wrong-fact exempt); remove 0, merge 0.
📄 Full reviews: [2026-11-20-pane-shows-programme-and-clear.reviews.md](2026-11-20-pane-shows-programme-and-clear.reviews.md)

## Picture

```
 relais state: claims/<repo-key>/<slug>/<id>.toml  (session, kind, repo)
        |
        v
 relais programme session --session S [--previous P] --json --no-fetch
   1. a live claim of S (work, else handoff)
   2. else a live handoff claim of P (the session /clear just ended)
   3. else {"programme": null}
        |  {repo, slug, ticket, kind, holder, counts, tickets[]}
        v
 plugin hooks                               right pane "relais"
   programme.ts  read on start, clear,      +---------------------------+
                 and after a Bash           | <runs, as today>          |
                 `relais programme          | <quiet runs, as today>    |
                 claim|release|next`        | programme sess.-boundaries|
   map.ts        the window: closed count,  | v 20 closed               |
                 current, its dependents,   | > t23 cd variables  < you |
                 the ready list             | +-o t25 relay names       |
   clear-tail.ts each main turn: last       | `-. t13 census (needs t12)|
                 messages, text, <= 2000 ch | ready: t12 (due 11-24) .. |
                 session.end{clear}: freeze | before /clear    [ hide ] |
                                            | person | hand off now     |
                                            | agent  | safe to /clear...|
                                            +---------------------------+
```

The picture shows relais answering which claim the session (or the session
it replaced) holds, and the plugin drawing that answer and the frozen
pre-clear tail as two new sections under the existing runs in the one pane;
the pane itself uses the real glyphs (check, triangle, box lines).

## Context

The person asked (2026-11-19) for the relais Claude Code plugin's pane to show
which programme the session is running, where it stands in the programme map
as an ASCII map, and, after a `/clear`, the last ~2000 characters / last
messages of the conversation before the clear. Programmes now carry per-session
claims (relais v0.71.0, #497: `<state>/claims/<repo-key>/<slug>/<id>.toml` with
`session`, `kind` work|handoff, `from`, `repo`), so "the session's programme"
is a fact relais can answer. The plugin already has one pane (`relais`,
`hooks/pane.ts` `layoutPane`, rendered by `renderPane` in `register.ts`), a
module store that lives across `/clear` (register.ts: "/clear and /resume go on
in this module under a new session id"), `session.end` with `reason: 'clear'`,
and `classic.SessionStart` with `source: 'clear'`.

The person's choices (AskUserQuestion, 2026-11-19):
- map layout: **a window around the current ticket** — closed tickets folded
  into a count, the last closed blocker, the current ticket marked `◀ you`, its
  dependents with their glyphs, then the ready list, then the counts;
- the pre-clear tail stays **until dismissed** (`[ hide ]`), replaced by the
  next `/clear`.

## Goal

1. When this session holds a programme claim (or, right after `/clear`, the
   session it replaced left a live `handoff` claim; its `work` claims are
   deleted by the SessionEnd hook, so a mid-ticket `/clear` with no handoff
   shows no programme), the relais pane shows the programme section with the
   window map, with or without a relais run.
2. After a `/clear`, the pane shows "before /clear": the last messages of the
   ended conversation, text only, at most 2000 characters, until `[ hide ]`.
3. Nothing changes for a session with no claim and no clear (no rows spent).

## Non-goals

- No new pane; no change to how runs or quiet runs are drawn.
- The plugin never reads claim files or maps itself: relais owns the data.
- No polling: the programme is read on events only.
- No full dependency tree, no tool output in the tail.
- No programme section for a session that works a programme without a claim, or after a /clear with no handoff claim (`SessionEnd` deletes work claims); the pane shows nothing then, by design.

## Phases

### Phase 1 — `relais programme session --json` (crates/relais)

- `crates/relais/src/cli/programme.rs`: new subcommand `session` with
  `--session <id>` (default `RELAIS_SESSION_ID`, else `CLAUDE_CODE_SESSION_ID`,
  as `claim` does), `--previous <id>` (optional), `--json`, `--no-fetch`.
- Resolution lives in `programme::claim` as a new
  `Store::claims_of(session, now)` (the private `keys()` stays private), so the
  CLI only formats; `now` is a parameter, so expiry tests do not sleep. Order:
  live claims of `--session` (a `work` claim wins over a `handoff`, newest
  `refreshed_at` first); else a live `handoff` claim of `--previous` (its
  `work` claims are never read: `end_session_once`, store.rs:310, deletes
  them at the clear, and one an `ended/<session>` tombstone still leaves on
  disk is ignored, with a test that fails without the rule); else
  `{"programme": null, "reason": "none"|"expired"}`. "Live" uses the existing
  hold rules (`claim::rules::held`, `WORK_HOLD_SECS` / `HANDOFF_HOLD_SECS`).
- Exit codes: 0 answered (null, a programme, or a programme with `error`);
  1 the claim store unreadable (stderr only, empty stdout); 2 usage (no
  session id, bad flag). Notes go to stderr; stdout carries exactly one JSON
  document.
- Locating the map: the claim's `repo` is a display path (rules.rs:58) and
  may name a removed task worktree. In order: `--dir` (the plugin passes the
  session cwd) when its repo key equals the claim's `repo_key`; else the
  claim's `repo`; else that path's main checkout through the git common dir
  when it still exists; else `{"programme": {repo, slug, ticket, kind,
  error}}`, which the plugin shows as one dim line. Loaded the way
  `show_command` does (`resolve_dir`, `load`, `standings`), honouring
  `--no-fetch`. Follow-up ticket: new claims record the common dir.
- Output, from `#[derive(Serialize)]` types (a snake_case `status` enum
  generated from `Standing`): `{programme: {repo, slug, ticket, kind,
  holder: "self"|"previous", counts, tickets: [{id, name, status:
  closed|frontier|blocked|in_flight|out_of_scope, blocked_by, open_blockers,
  dependents, due, held: bool}], ready: [ids in frontier order, as
  `order_frontier`]}}`, map order kept; the plugin derives no map logic. A
  golden-JSON file is shared with the TS tests.
- A plain `claim` then `/clear` shows no programme: only a `handoff` written
  by `relais programme next --claim` (the Handoff step) survives the clear.
- Text output (without `--json`): one line, `<slug> <ticket> (<kind>)` or
  `no programme claim`, for a person at a shell. A gone repo or a failed map
  prints `relais: <repo> <slug>: <error>` and `next: relais programme show <dir>`;
  no session id prints `relais: no session id` and `next: pass --session <id>`.
- Tests: unit tests on resolution (own work > own handoff > previous handoff >
  none; expired claim ignored) with a temp `Store`; one CLI test on a fixture
  repo for the JSON shape. CLI help pinned by cli-probe as the other
  subcommands are.

### Phase 2 — the programme section (claude-plugin)

- `hooks/programme.ts` (effects through `fx`, like `transcript.ts`): spawns
  `relais programme session --json --no-fetch --session <id> --dir <cwd> [--previous <old>]`
  outside the 100 ms flush, bounded at 5 s (on timeout the child gets
  SIGTERM and the last view is kept; a reply older than the newest spawn is
  dropped); parses; stores
  `store.programme = { view, readAt, seq } | { error, seq }` and marks dirty;
  each read takes a rising `seq`, and an answer older than the stored one is
  dropped.
  Every read and cached view is bound to the current session id: a session
  transition drops the old view and any pending read before the next read,
  and a timeout keeps a view only within the same session. Triggers:
  `session.start`; `classic.SessionStart` source `clear`/`resume`/`fork`;
  `--previous` is passed only for the successor of a `reason: 'clear'` end,
  never on resume or fork (tests: clear, resume, fork with a delayed old
  reply and a timed-out new read); a main-chain
  Bash `tool.call` whose command contains `relais programme claim|release|next`
  (after `next(e)` returns). The spawn is skipped when the session id is empty. An older relais (exit 2
  and stderr naming the unrecognized subcommand `session`) is noted once and
  never retried in that module life, shown as one dim pane row `programme
  view needs a newer relais · relais install`; any other exit 2 is a one-line note and
  the next trigger retries.
- `hooks/map.ts` (pure): `programmeRows(view, width): Row[]` —
  1. head `programme <slug> · <repo basename>`;
  2. `✓ N closed` (all closed tickets folded);
  3. the current ticket's closed blockers, at most 1, as `✓ <id> <name>` + `│`;
  4. `▶ <id> <name> ◀ you` (`◀ next` when the holder is the previous session's
     handoff);
  5. tickets whose `blocked_by` names the current one, `├─`/`└─` with `○` ready,
     `·` blocked (`(needs …)` naming the open blockers), `▶ … (in flight)` in flight, at most 4
     then `└─ +n more`;
  6. `ready: <id> (due MM-DD) · …` wrapped to 2 rows;
  7. dim counts line.
  Widths through `cellsOf` / `cutLineTo` (transcript.ts, timeline.ts) so CJK
  and emoji are measured in cells; names cut with `…`.
- `hooks/pane.ts`: `layoutPane(runs, room, quiet, extra)` takes the programme
  and tail rows as a third block after the quiet rows. Budget: when runs need
  the rows, the programme section shrinks first to rows 1, 4 and 7, then to
  row 4 alone; the tail shrinks to its header row (with `[ hide ]`, always kept) plus its newest rows; runs keep their current
  priority. No claim → zero rows.
- Opening: the pane opens unasked (`$.ui.open`, focus left out) when a
  programme is first read for the session or a tail is frozen, as quiet runs
  do; below the engine's width threshold the status line carries it instead.
- Status line: the one string `▶ <slug> <ticket>`, appended when no run is
  live and used as the narrow-width fallback above.

### Phase 3 — the pre-clear tail (claude-plugin)

- `hooks/clear-tail.ts`: on every main-chain `turn.complete`
  (`e.agentId === undefined`), detached, read `$.session.messages({ last: 20 })`,
  keep `{role, text}` of user and assistant messages with text (no tool uses,
  no tool results, no relais `task-notification`s), join newest-last and trim
  from the front to 2000 characters at a message boundary where possible
  (a single longer message is cut from its front with `…`). Kept in module
  memory as `store.liveTail = { session, seq, messages }`: only the newest
  read of the current session updates it, a late or timed-out read never
  does, and every session transition resets it. The frozen tail is
  immutable; hide and the `$.store` write are serialised so a delayed write
  cannot bring a hidden tail back (tests: reordered reads, a read settling
  after its timeout, two clears with no turn between, hide then reload).
- `session.end` with `reason: 'clear'`: one last read attempt (500 ms bound),
  else the last `liveTail`; freeze as `store.clearTail = { endedSession,
  messages, at }` and in `$.store` under `relais.clearTail` (only the newest)
  so a module reload after the clear still shows it. Remember `endedSession` as
  the `--previous` for Phase 2.
- Pane: section `before /clear · <hh:mm>` with a `[ hide ]` Button; rows
  `person │ …` / `agent │ …` laid out with the transcript view's row code
  (`transcript.ts`). `[ hide ]` clears `clearTail` (state and `$.store`).
  A later `/clear` replaces it. Below the pane's width threshold the status
  line carries `before /clear saved · open the relais pane` until `[ hide ]`.

### Phase 4 — docs, release, live proof

- `claude-plugin/README.md` "What it does": a **Programme** and a **Before /clear** bullet; plan
  record kept in `docs/plans/`.
- `CHANGELOG.md`; release through the `tag-release` skill; plugin reinstall
  (`relais install` / marketplace refresh) on this machine.
- Live proof (acceptance, not unit tests): in a session that runs
  `relais programme claim <slug> <id>`, the pane shows the window with
  `◀ you`; then `relais programme next --claim` (the handoff) and `/clear` — the pane shows `before /clear` with the last
  messages (time `relais programme session --json --no-fetch` on this machine's
  claim store: expect < 300 ms against the 5 s bound) and the programme line `◀ next` from the previous session's handoff
  claim; `[ hide ]` removes the tail; a `/clear` with no handoff claim shows the tail and no programme section. Checked in the terminal at ≥ 144 columns
  and once below, where the status line carries it.
  Also: a session with no claim and no /clear shows no new rows; an older
  relais on PATH gives one dim note, no retry.

## After release

- Count over 7 days from the reinstall: sessions holding a claim whose pane showed the programme section on their first turn (target: all); /clears that froze a non-empty tail (target >= 95%).
- Roll back the tail to opt-in if more than 50% of tails are hidden within 60 s of the /clear.

## Code review

- relais: backend, lang:rust, unix, tui, architect, lang:typescript

lang:typescript is added because TS is most of the change: the plugin's hooks.

## Verification

- `relais programme session --session <none> --json` → `{"programme":null}`, exit 0; with a fixture work + handoff claim → `holder:"self"`, the work ticket.
- pane-layout test: no claim, no tail → 0 extra rows; 6 rows of room with 3 runs → programme reduced to row 4.
- `cargo test -p relais programme::` and the new CLI test; `cargo clippy`.
- `cd claude-plugin && claude plugin validate . && claude plugin test .`
  (new `tests/map.test.ts`, `tests/clear-tail.test.ts`, extended
  `tests/views/pane-layout.test.ts` for the row budget and the zero-row case,
  `tests/views/relais-pane.test.ts` for the sections and `[ hide ]`).
- `tsc -p claude-plugin` against the generated types of the pinned Claude Code.
- The project's local gates (`greengate run`) before the push.
- Live proof as in Phase 4.

## Decision log

- Data source: a relais subcommand, not the plugin reading TOML — relais owns
  the claim store and the map parser.
- After `/clear` the new session holds no claim yet; the previous session's
  `handoff` claim names the next ticket, so the pane shows it as `◀ next`. A
  `work` claim of the ended session is deleted by the SessionEnd hook
  (hook/respond.rs:424), so a session that never ran `next` shows nothing
  after `/clear`.
- The tail ships in the same PR and release as the programme section (one
  PR per repo), not as a release of its own.
- The tail is captured on each turn, not only at `session.end`, because the
  engine does not promise the old messages are still readable at that moment.
- 2000 characters, text only, message boundaries preferred — the person's
  "last 2000 characters or last messages".
- Phases 2–3 also keep a capped event list in the plugin store
  (`hooks/events.ts`: one entry per programme read, freeze and hide), counted
  on `/relais-status`: the After-release measure reads it.
- The map lookup keeps the common-dir hop the review asked for (the recorded
  worktree's main checkout when it lacks the map); a removed worktree with no
  `--dir` in its repository still reads "the repository is gone" until claims
  record the common dir (follow-up).
- Two relais runs (Phases 2–3, then the review fixes) stopped `blocked
  (env_missing)` only on plugin tests hitting their 5 s timeout at load 50–70;
  each candidate passed every plugin test (458/458, then 466/466) at load
  10–40 and was committed by hand, recorded `salvaged`.

- Closed with three gaps, each named (2026-11-20): (1) Phase 3's narrow-width
  status hint shows only when the pane is unplaced at open; after a resize the
  plugin does not learn it (live row "narrow"), tracked as follow-up f03;
  (2) `relais programme session` took 1.24 s on this machine's real claim
  store against the 300 ms guessed in Phase 4: the map load reads git, and the
  plugin's bound is 5 s, so the guess is replaced by the measured 1.24 s;
  (3) the Verification's `tsc -p claude-plugin` did not run: the repository
  commits no `tsconfig.json` and the plugin's types are generated only when
  Claude Code loads it, so `claude plugin validate` (per-module type check,
  on 2.1.291 and 2.1.329) stands in.

## Verification record (Phases 1–3, 2026-11-20)

| Check | Input | Expected | Actual |
| --- | --- | --- | --- |
| no claim | `relais programme session --session nobody --json --no-fetch` | `{"programme":null,"reason":"none"}`, exit 0 | as expected, 0.09 s |
| live handoff claim | a real `handoff` claim on decisions `session-boundaries`, `--dir` its repo | the ticket, `holder:"self"`, every ticket | t23, 28 tickets, 6 ready, 1.24 s (not the guessed 300 ms; inside the 5 s bound) |
| previous session | `--session newone --previous <that session>` | `holder:"previous"` | `previous t23…` |
| expired | the same claim past its 2 h hold, text mode | `programme claim expired` | as expected |
| no session id | no flag, no env | `relais: no session id`, `next: pass --session <id>`, exit 2 | as expected |
| Rust tests | `cargo test -p relais --test programme_session` + the claim unit tests | pass | 15/15; resolution order, expiry, the tombstone guard and the gone-repository hint each tested |
| plugin tests | `claude plugin test claude-plugin` | pass | 467/467 at load ~10 (the SessionStart-race test fails with its fix reverted: 466/1) |
| plugin tests on the floor | the same under Claude Code 2.1.291 (the Makefile pin, CI's version) | pass | first CI run red: 12 clear-tail tests, as 2.1.291 refuses `messages({ last })`; with `messages({})` and a local slice, 468/468 on 2.1.291 and on 2.1.329 |
| plugin validate | `claude plugin validate claude-plugin` | passes | passes with warnings of the kinds the base already has (`.ts` import extensions, possibly-undefined indexes); no separate `tsc -p` run |
| gates | the pre-commit gate on each commit (fmt, clippy, cargo-test, plugin-test, cycles, ban-terms) | pass | passed on d6c8369d |
| live, older relais | Claude Code 2.1.329 in tmux, 180×50, `--plugin-dir` this build, installed relais 0.71 | one dim row, no retry | `programme view needs a newer relais · relais install` in the pane; no read after a Bash claim (the plugin's `relais` is the one on its own PATH) |
| live, happy path (Phase 4) | relais 0.72.0 (brew), Claude Code 2.1.329 in tmux 180×50, `--plugin-dir` the merged plugin, a scratch demo repo: `claim demo t03-handoff` | the window, `◀ you`, status `▶ demo t03-handoff` | the pane opened unasked with the window (`✓ 2 closed`, the blocker, `▶ t03-handoff … ◀ you`, two `·` dependents with `(needs …)`, the ready line, the counts); status `▶ demo t03-handoff` |
| live, handoff + `/clear` | `next --from demo:demo --claim` (first parked `next due 2026-11-24`; after a ready ticket was added it reserved t08-count), then `/clear` | `◀ next` from the handoff, the tail | `▶ t08-count Count the handoffs ◀ next`, `before /clear · 07:01 [ hide ]` with the three exchanges, text only; status `▶ demo t08-count` |
| live, narrow | the same session resized to 100 columns, then back to 180 | the pane hides; the status names the saved tail | the pane hid and came back at 180; the status kept `▶ demo t08-count` without `before /clear saved` (the plugin learns placement only when it opens the pane): follow-up f03 |
| live, `[ hide ]` | — | — | not pressed live (tmux sends no click to the pane); covered by the plugin tests |

## Implementation review

- implementation-review: approve after round 1 and 3 Deltas (opus); 2 blocking, 5 medium fixed; 12 candidates ruled; ~280k tok.
- backend, unix: approve on the last Delta; deliberate: `resetLiveTail` after two awaits (a lost first read is re-read), the stale note's cause left to `relais programme session`.
- lang:rust, tui: approve; deliberate: `KindView::as_str`, `Nothing { expired }`, the failed-map row's short hint (the error wins the width).
- architect, lang:typescript: approve after one Delta (opus).
- 2.1.291 delta (read the tail without `last`): implementation-review and lang:typescript approve.
- Phase 4: v0.72.0 released (#506, all six platform assets); brew upgraded to 0.72.0 here; live proof above. Refreshing the installed Claude plugin is the person's step: the relais plugin refuses its install command from the model. Follow-ups f01–f03 in `docs/programmes/follow-ups/`.

<!-- panel: repos=relais adds= reviewers=architect,backend,lang:rust,po,tui,unix,codex body-sha=0e063360eeca -->
