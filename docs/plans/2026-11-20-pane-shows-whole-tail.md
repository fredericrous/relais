---
status: active
branch: feat/pane-tail-whole
repos: [relais]
adrs: []
plan-id: p-01M7XC4EB97TWNKJMXNE0KGPRC
---
# Pane shows the whole pre-clear tail

## Review panel
👉 **Decide:** none — approve if the engine's own pane scrolling (arrows, pane focused) works on 2.1.291; Phase 1's spike checks it first.
📍 relais · 2 phases · 1 release · next: Phase 1 spike. Panel: backend, lang:typescript, tui, unix, ux-research, game-ux (opus), codex.
**Changed by review:** two more cuts found (240-char lines, the 20-message slice); runs view always keeps the newest tail row; `[ open ]` view opens at its top, hint names Esc.
**Verdicts:** 3 approve, 5 approve-with-changes; 14 carried; words 1190 → 1754 (×1.47, fixes to wrong or unverified claims); remove 0, merge 0.
📄 Full reviews: [2026-11-20-pane-shows-whole-tail.reviews.md](2026-11-20-pane-shows-whole-tail.reviews.md)

## Picture

```
 today                            after this plan
 -----                            ---------------
 tail (<= 2000 ch, whole msgs)    tail (exactly 2000 ch, oldest cut with …)
   |                                |
   v                                v
 each message: first 12 rows      each message: every row
   |                                |
   v                                v
 runs view: rows that fit         runs view: newest rows that fit
 (end of last answer lost)          + "… N earlier rows" + [ open ]
                                    |
                                    v  press [ open ]
                                  tail view: all rows, arrows scroll,
                                    [ back ] to the runs view
```

The tail used to lose rows at three cuts; after this plan it keeps all
2000 characters and the runs view always ends on the last answer's end,
with the rest one press away in a view that scrolls.

## Context

After a `/clear` the relais pane shows a `before /clear · hh:mm [ hide ]`
block, which is meant to hold the last ~2000 characters of the ended
conversation (plan `docs/plans/2026-11-20-pane-shows-programme-and-clear.md`).
The person saw much less than that, and the end of the last answer (the
recap and its `next:` line) was cut off. The cause is in the rendering, not
the capture:

1. `transcriptRows` → `messageRows` (`claude-plugin/hooks/transcript.ts`)
   caps each message at `MAX_MESSAGE_ROWS = 12` rows and keeps the **first**
   12. At ~45 columns that is ~500 characters, so a long last answer loses
   its end.
2. In the runs view, `extraRows` (`hooks/pane.ts`) gives the tail only the
   room the runs and programme rows leave (newest rows), and `extraOf` caps
   it at 60 rows.
3. `trimTail` (`hooks/clear-tail.ts`) stops at a message boundary: when the
   next older message does not fit whole, it is dropped, so the kept tail
   can be well under 2000 characters.

The person asked: the whole tail, and when the pane is not tall enough,
a way to expand or scroll it.

## Goal

After a `/clear`, the pane shows the last 2000 characters of the ended
conversation, with the end of the last answer always visible. When the pane
is too short for all of it, one press opens the tail in its own view, which
the person scrolls with the arrow keys.

## Non-goals

- No change to what is captured (text only, no tool output) or to when the
  tail is frozen or hidden.
- No change to the agent transcript view or its 12-row cap per message.
- No new key bindings: scrolling is the engine's own (pane focused, arrows).
- The status-line hint (`before /clear saved · open the relais pane`) stays.

## Code review
- relais: lang:typescript, tui (backend left out: no new store key, and 0.73.0 draws `kind: 'tail'` as the runs view, `pane.ts:614-629`; unix left out: no CLI change)

## Behaviour

- **Capture fills the budget.** `trimTail` keeps whole messages from the
  newest back; the first older message that does not fit whole is kept cut
  from its front with `…`, so the tail holds exactly 2000 characters when
  the conversation has that many. `tailOf` passes all eligible text messages from the read to `trimTail`, without a `LAST_MESSAGES` cutoff; the character budget bounds retained text. Replace the existing newest-20-messages test with an older 1800-character answer followed by twenty 10-character text messages: all 2000 characters survive. Keep the tool-turn filtering regression.
- **Tail rows are uncapped per message.** The tail is laid out by a new
  `tailRows(messages, columns)` in `transcript.ts`: the same lead (`PERSON`
  / `AGENT`) and wrap as `messageRows`, with no 12-row cap (2000
  characters bound it), and lines split with `cleanLine`, not `cutLine`: `linesOf` cuts each line at `MAX_LINE = 240` (timeline.ts:11), a fourth cut. Escape sequences and control characters still go (`cleanLine`), and each row still passes `cutBytes`. The agent view keeps `transcriptRows` as is.
- **Runs view (pane too short).** The tail block stays after the quiet rows and keeps the newest rows. For a nonempty rendered tail, `layoutPane` reserves two rows: its header and its newest content row. Programme layer selection also reserves that content row; runs and quiet rows use the remaining budget. Add a six-row regression with three active runs, a programme and a long tail: the programme current row, tail header with `[ open ] [ hide ]`, and newest tail row remain visible, and the layout stays within six rows. When rows were left out, the header becomes
  `before /clear · hh:mm [ open ] [ hide ]`, two Buttons in one row (a header Row variant with a `buttons` list, drawn as a Box of Text and `label` Buttons; `Press` gains `open`), and
  the first tail row says `… N earlier rows` (dim) when at least 2 tail rows fit; with one row, that row is the newest tail row and `[ open ]` alone marks the cut. With every row shown,
  the header stays `before /clear · hh:mm [ hide ]`. The 60-row cap goes:
  the room is the only limit.
- **Tail view.** `[ open ]` sets the pane's shown view to
  `{ kind: 'tail' }`: a top row `[ back ]` (autoFocus, a Button with `label: 'back'` for 2.1.291, as in the agent
  view) with the dim hint `  ↑↓ scroll (starts at the top) · Esc hands the keys to the prompt`, then the
  title row `before /clear · hh:mm · N rows` and every tail row (at most `ROWS_CAP`, newest kept), not fitted to the
  room. The engine scrolls a tree taller than the body while the pane is
  focused (`Pane.scroll`, Claude Code types). `[ back ]` returns to the
  runs view. `[ hide ]` from the runs view, or a tail hidden by any path
  while the tail view is shown, returns to the runs view: `hideTail` itself resets `shown` to runs, and `viewOf`/`paneTree` fall back to runs when shown is `tail` but the state has no tail. A new `/clear`
  that freezes a new tail while the tail view is open shows the new tail.
- `[ open ]` records an `open` event `{ rows, bodyRows }` beside `freeze` and `hide`.
- The pane state type (`types/index.d.ts`, `RelaisPaneShown`) gains
  `{ kind: 'tail' }`; a state written by 0.73.0 still reads.

## Preview
evidence: a terminal pane, no web page: the Verification rows record a
tmux `capture-pane` of the runs view before and after, and of the tail
view scrolled to its top, at 180 and at 100 columns.

## Phases
- [x] Phase 1 — first a live tmux spike that arrows scroll with `[ back ]` focused (2.1.291 and 2.1.329), and that at 100×30 (inline seat) a 120-row tree keeps the prompt on screen; record the frame height against `scroll.bodyRows`. If the frame grows past the terminal, stop and bring the plan back to review (a cap would end scrolling), then capture and layout: `claude-plugin/hooks/clear-tail.ts`
  (`trimTail` fills the budget), `claude-plugin/hooks/transcript.ts`
  (`tailRows`), `claude-plugin/hooks/pane.ts` (`extraOf`/`extraRows`:
  `[ open ]` and `… N earlier rows` when cut, no 60-row cap; `paneTree`
  draws the tail view and the two-button header), `claude-plugin/hooks/ui.ts`
  (`openTail` bumps `generation` like `back`, `viewOf`, hide/back return to runs), `claude-plugin/hooks/store.ts`
  (shown kind `tail`), `claude-plugin/types/index.d.ts`; tests in
  `claude-plugin/tests/clear-tail.test.ts`,
  `claude-plugin/tests/transcript.test.ts`,
  `claude-plugin/tests/views/pane-layout.test.ts`,
  `claude-plugin/tests/views/relais-pane.test.ts`; `CHANGELOG.md`.
- [ ] Phase 2 — release (tag-release, next minor), reinstall the plugin on
  this machine, live check in tmux: `CHANGELOG.md`.

## Decision log
- 2026-11-20 — expand is a view of its own with `[ back ]` (unbacked by UX research: SAP Fiori and ServiceTitan favour expanding in place, which a pane cannot do), scrolled by
  the engine, because the agent view already works that way and a pane
  cannot grow itself (the engine owns its seat and height).
- 2026-11-20 — the runs view keeps the newest rows when cut, because the
  end of the last answer is what the person resumes from.
- 2026-11-20 — Phase 1 also touched `claude-plugin/hooks/events.ts` (the
  `open` event, carried from backend's round-2 finding A) and
  `claude-plugin/hooks/limits.ts` (`TAIL_CHARS` moved there, so `pane.ts`
  reads it without an import cycle through `clear-tail.ts` → `ui.ts`).
- 2026-11-20 — Phase 1 was done by hand: two relais runs ended
  `admission_wait_exhausted` (no lease in 30 min each, host load 60+).
- 2026-11-20 — the agent view's `[ back ]` now passes `label` too, as the
  2.1.291 floor needs; its text is unchanged.
- 2026-11-21 — the tail view caps at `MAX_TRANSCRIPT_ROWS` rows plus its
  title (401), not `ROWS_CAP` as the Behaviour says: the cap counts tail
  rows, the title and `[ back ]` row ride above it (implementation review).
- 2026-11-21 — the runs view's `… N earlier rows` always hides at least 2
  rows (the marker takes a row itself); `plural` gives `1 earlier row` only
  in the tail view's cap.

## Verification
| Input | Expected | Actual |
| --- | --- | --- |
| spike: a 120-row tree under an autoFocus `[ back ]`, tmux, 2.1.329 and 2.1.291 | arrows scroll with `[ back ]` focused; inline at 100×30 the prompt stays on screen | pass on both: docked body 33 rows, ↓/Home/End move the window one row a press (ctrl+x tab to focus), Esc keeps the offset; inline at 100×30 the frame stays at `bodyRows` 8 and scrolls, prompt on screen |
| `trimTail` on messages of 1500 and 800 characters | 2000 characters: the 1500 whole, the 800 cut from its front with `…` | pass (`clear-tail.test.ts`) |
| `trimTail` with 0, 1 and 2 characters of room left before an older message | no partial message for 0 and 1; `…` plus 1 character for 2; total ≤ 2000 | pass |
| `tailOf`: an 1800-character answer, 30 tool turns, then twenty 10-character messages | all 2000 characters, the answer whole | pass (the `LAST_MESSAGES` cutoff is gone) |
| `tailRows` on one 2000-character line at 40 columns | ≥ 50 rows, joined back equal to the input, last row ends with the last word | pass (`transcript.test.ts`) |
| `tailRows` on a message holding `ESC[31m` and a BEL | no ESC or BEL in any row | pass |
| pane layout, 3 runs, room 6, programme, tail of 10 rows | header with `[ open ]` then `[ hide ]`, programme row, newest tail row, ≤ 6 rows | pass (`pane-layout.test.ts`) |
| pane layout, no runs, room 12 and room 80, tail of 30 rows | 12: `… 20 earlier rows`, newest last; 80: `[ hide ]` only, all rows | pass |
| header at 38 columns | title cut, both buttons fit | pass |
| tail view tree past `MAX_TRANSCRIPT_ROWS`, a front-cut tail | title `last 2000 characters · 999 rows`, `… 600 earlier rows`, 401 rows | pass |
| press `[ open ]`, then `[ back ]`; hide while the tail view is shown; a new `/clear` with the view open | tail view with hint and every row; runs again; `hideTail` resets to runs; the new tail shows | pass (`clear-tail.test.ts`), `/relais-status` says `1 opened` |
| live: tmux 180×40, `--plugin-dir claude-plugin` (2.1.329), a 150-line answer, `/clear` | the runs view ends on the answer's last line; `[ open ]` opens at the first row; arrows scroll; `[ back ]` returns | pass: `before /clear · 23:51 [ open ] [ hide ]`, `… 121 earlier rows`, ends `line 150`; the view opens on `[ back ]`, hint, `· 153 rows`, `person │ Reply with…`; ↓ moves, End shows `line 150`; Enter on `[ back ]` shows the runs view |
| live: same at 100×30 (inline seat) | scrollable, the prompt on screen | pass: frame rows 16–25 (`bodyRows` 8), prompt on row 28; End reaches `line 150`; Esc keeps the tail view scrolled where it was |
| `make plugin` (`claude plugin validate`, `scripts/plugin-test.sh`) | exit 0, the new tests listed as passed | pass: 481 pass, 0 fail; validate passes with the TypeScript notes main already has |

## Implementation review
- implementation-review (opus): approve-with-changes → approve on the Delta; fixed: exhaustive header-button switch [blocking], exact one-row test, full-tail front-cut check, cap noted in Decision log; candidates 3 fine (store actions). 75k, 56 s.
- lang:typescript (opus): approve-with-changes → approve; fixed: narrowed `RowButton.press`, `Row` button/buttons union, rows laid out once, typed title parts, front-cut check. 69k, 55 s.
- tui (opus): approve-with-changes → approve; fixed: `plural` counts, buttons-only header under 20 columns, front-cut check. 73k, 64 s.

<!-- panel: repos=relais adds=screen reviewers=backend,lang:typescript,tui,unix,ux-research,game-ux,codex body-sha=7fd95644684c -->
