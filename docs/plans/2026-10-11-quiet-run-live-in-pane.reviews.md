# Full reviews: 2026-10-11-quiet-run-live-in-pane

Round 1 (sha 9537cf3f9203; full panel for a new plan: the tool reported `delta` only because this file held the previous, landed plan):
- **backend** (44k, 85 s) approve-with-changes: [high] one `--session` breaks after /clear and /resume; [high] unbounded log reads; [medium] no watcher restart; [medium] shared dir, modes, Windows, test override; [medium] entry left on panic (`Drop`); [medium] no Actual column or latency row. All accepted.
- **lang:rust** (32k, 25 s) approve-with-changes: [high] pid liveness per platform (EPERM alive, mtime guard); [high] hand-written JSON escaper, `AtomicU32`, Windows dir; [medium] start at the last 16 KB, split on `\r`, final drain; [medium] `cutLine` parity; [medium] spawn/stdin semantics unverified; [low] tools sweep their own dead entries; [low] tests that fail without the feature. All accepted.
- **lang:typescript** (54k, 70 s) approve-with-changes: [high] watcher must stay out of the run machinery; [high] /clear and /resume, rejected in favour of backend's plugin-side filter; [high] no stdin reliance; [high] pane-opening is the person's call (decided: open, toast if no room); [medium] version skew exit 2. Others accepted.
- **react** (47k, 59 s) approve-with-changes: [high] pane never opened (decision); [high] empty state swallows a quiet run; [medium] relais-first budget formula; [medium] separate test-fake branch; [low] one dim Row; [low] Text-only rows after relais runs. Accepted.
- **ui-design** (38k, 45 s) approve-with-changes: [high] mockup round (decision: none, existing style below relais runs); [high] empty-state early return and cap order; [medium] cap quiet heads at 3; [medium] `waiting for output` and watcher-failure states; [low] baseline row-count test. Accepted.
- **ux-research** (34k, 50 s) approve-with-changes: [high] a failure must stay visible (decision: failure stays, pass drops); [medium] 1 s anti-flash; [low] unbacked choices, measure in phase 6. Accepted.
- **game-ux** (37k, 38 s) approve-with-changes: [high] open the pane with a toast (decision: yes); [high] show the end state (decided per ux-research instead); [medium] status line; [low] empty-state branch. Accepted.

Person's decisions (2026-10-11): pane opens on the first quiet run (toast once if not placed); a failed run stays until the next quiet run starts, a pass drops at once; no mockup round; no extra clearing of a failed row ("it's a log like any other").

Round 2 (sha 274c4f76a5b0):
- **backend** (35k, 24 s) approve-with-changes: round-1 findings 1–6 resolved. [high] an entry removed right after `exit` is written loses short failures, accepted (entry left; watcher emits then deletes; 60 s sweep); [medium] no upper bound on a failed row, person decided none; [low] double write, `done` flag; [low] Verification row lifecycle, fixed.

Binds:
- sha 10b0b279f9b2, **backend** (28k, 16 s) approve-with-changes: [high] the stale rule would drop ended entries, accepted (an `ended` entry is never stale); [medium] phase 1 test wording; [low] open also sweeps ended entries older than 60 s. Applied verbatim.
- sha 8e788d30a046, **backend** (28k, 12 s) approve-with-changes: all resolved. Carried into implementation:
  1. [low] The watcher checks `ended` before pid liveness: the writer has usually exited by then, so liveness-first would drop exactly the entries this design keeps.

Delta pass (sha 8e788d30a046; the hook wanted every reviewer on the edited text): typescript, react, ui-design, ux-research, game-ux all approve-with-changes. Applied: pane opens after 1 s or on a failure (game-ux, high); a failure is cleared only by the same tool and check (react); person review of renders before release (ui-design); live heads before kept failures (ui-design); a sub-second-failure test (ux-research); the watcher's own read loop and draw-time session filter (typescript); the stale `sleep 5` row.

Final bind (sha 6e179a158a84): game-ux **approve** (29k, 12 s); backend (41k, 39 s), typescript (32k, 22 s), react (30k, 18 s), ui-design (30k, 19 s), ux-research (50k, 64 s) approve-with-changes. All medium or low, so the body was not edited again. Carried into implementation, each to be done as written:
  1. [medium, react/typescript/ui-design/backend] Phase 4's test list and the Verification row "first quiet `started` … pane opens" describe the old trigger. Test instead: the first quiet run past 1 s opens the pane on a pump tick with no further protocol line; a sub-second pass never opens it; a second run does not toast again.
  2. [medium, backend] The 1 s check runs in the pump tick (`now - started >= 1000`, or a failed `ended`), never in the `started` fold.
  3. [medium, backend] A failed row is cleared only by the next quiet run of the same session, tool and check. Sibling checks and other sessions' runs never clear it; passed entries of any session drop on `ended`, so the map stays bounded. Context and the exit-1 Verification row get the same wording.
  4. [medium, backend] An entry the watcher first sees already ended (exit 1, dead pid) yields `started`, then `output` with its log's last lines, then `ended`, in that order. Rust test for it.
  5. [medium, ux-research] Decisions wording: the plugin keeps every entry and draws only those whose `session` is in `store.sessions`, matching phase 4.
  6. [medium, ux-research] `openPane` fires only for a run whose `session` is in `store.sessions`.
  7. [medium, ux-research] The row "pre-commit with sub-second checks" is about passing checks: none flashes and the pane stays closed; a failing one opens it.
  8. [medium, ux-research] The status line (4b) lists kept failures as well as live runs (`amont clippy failed 0:12`), so a failure under `+n more` is still visible.
  9. [low, backend, earlier bind] The watcher checks `ended` before pid liveness.
  10. [low, typescript] The heartbeat is exactly `{"relais":"quiet","event":"heartbeat"}` every 5 s; Rust test: one arrives within 6 s with no entries.
  11. [low, game-ux] Pane test: a failed amont/clippy row stays when amont/test starts and clears when amont/clippy starts again (same session).
  12. [low, ux-research] No age cue on a kept failed row (the person's call): observe in phase 6 whether a stale failure reads as live.
