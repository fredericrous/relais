---
status: active
branch: feat/quiet-in-relais-pane
repos: [relais, amont, amont-agent]
adrs: [ADR-0022]
---

# A quiet run is live in relais's pane

## Review panel

👉 **Decide:** none. Approve if the 12 carried findings below are fine to fold in during implementation rather than in another review round.
📍 relais (plan, watcher, pane) + amont + amont-agent (active-run entry) · reviewed and bound · next: amont and amont-agent entries. Panel: backend, rust, typescript, react, ui-design, ux-research, game-ux (opus-5-5).
**Changed by review:** watcher has its own spawn path and read loop and no `--session`; ended entries are kept until emitted; pane opens after 1 s or on a failure; relais-first budget.
📄 Full reviews: [2026-10-11-quiet-run-live-in-pane.reviews.md](2026-10-11-quiet-run-live-in-pane.reviews.md)
**Verdicts:** round 1: 7× approve-with-changes. Delta and final binds: game-ux approve, the rest approve-with-changes. 12 findings carried into implementation.

## Context

Since amont 1.50.0 and amont-agent 2.35.0 (plan `2026-10-11-test-output-keeps-its-status`), a check or command an agent runs is quiet: the tool's output goes to a `.log.tmp` file and the agent's result is one line or the failure excerpt. A person at a terminal still sees the stream (`amont.quiet auto` is quiet only when stderr is not a terminal), but in the Claude Code console a long agent-run check now shows nothing while it runs — only, after 5 s, `running <check>, log <path>`.

The person chose (2026-10-11) to show that stream live in relais's existing pane rather than a new pane or a hand `tail -F`.

Facts that shape the design (relais `claude-plugin/`, explored 2026-10-11):

- The pane is a `ui.render` hook (`register.ts:498`) that only reads stored state; a 100 ms pump (`register.ts:97`, `runs.ts:21` → `ui.ts:43 flush`) is the single writer (binding in `docs/plans/2026-10-06-all-native-mod.md`, "one writer").
- A run's live output reaches the pane as JSON lines on a child's stdout: `relais run --protocol` spawned through `$.process.spawn`, split by `lines.ts:splitLines`, folded by `timeline.ts:applyEvent`, capped by `appendOutput` (`MAX_OUTPUT_LINES = 40`) and `cutLine` (`MAX_LINE = 240`).
- Plugin code has no file access: no hook imports a Node built-in, and `Fx` (`fx.ts:8`) has no file or watch effect. Every effect is spelled on `$` in `register.ts`.
- Claude Code exports `CLAUDE_CODE_SESSION_ID` to every tool command, so a `git commit` (amont's hooks) and an `amont-agent quiet` run both know which session started them. The plugin knows its own session.
- No relais rule limits the pane to relais runs; `docs/SPEC.md:618` and the README describe it as "this session's runs".

Outcome: while an agent-run quiet check or command is live, relais's pane shows it as a row — tool, check, elapsed — with its last lines updating live, scoped to the session that started it. A passed run drops when it ends; a failed one stays until the next quiet run starts. The agent's context is unchanged (still one line or the excerpt).

## Decisions

- **Reuse the pane's one data path: a child process emitting JSON lines.** A new `relais quiet watch --protocol` (Rust, in the relais binary) watches the active-run directory and tails each log from an offset, emitting events the plugin folds like run events. The plugin gains no file access and no new `Fx` member; it spawns one more child.
- **Contract between the tools and relais: one JSON file per live run.** `${XDG_STATE_HOME:-~/.local/state}/quiet/active/<pid>-<n>.json`, written atomically (temp + rename; file 0600, dir 0700; the dir is `$QUIET_ACTIVE_DIR` when set, used by every test, and `%LOCALAPPDATA%\quiet\active` on Windows) when a capture starts inside a Claude Code session (no entry is written without `CLAUDE_CODE_SESSION_ID`): `{"v":1,"tool":"amont"|"amont-agent","session":"<CLAUDE_CODE_SESSION_ID>","check":"<name>","repo":"<toplevel or cwd>","log":"<abs .log.tmp path>","pid":<n>,"started":<unix ms>}`. When it ends, the tool rewrites the entry with `"exit":<code>` and `"ended":<unix ms>` and leaves it: the watcher emits `ended` (a null `exit` means killed and is drawn as failed), then deletes it, and any tool's sweep removes ended entries older than 60 s, so a run that ends inside one 250 ms poll is still seen with its exit. `<n>` disambiguates amont's concurrent pre-commit checks within one process. Documented once in relais `docs/` and pointed to from both tools.
- **Scoped by session id, in the plugin.** The watcher takes no `--session` flag: it emits each entry's `session`; the plugin keeps entries whose `session` is in `store.sessions` (every id this module served, so /clear and /resume keep working, `register.ts:109`, `:254`); an entry with no session (a run outside Claude Code) is never shown. No process-tree walking.
- **Stale entries are dropped, not trusted.** A killed run leaves its file: the watcher ignores an entry whose `pid` is dead (`libc::kill(pid,0)` on unix with EPERM counted as alive, `windows-sys` OpenProcess on Windows, and the log must still exist with a recent mtime to guard against pid reuse), and removes it only if its owner is dead and it is older than 1 h. An entry carrying `ended` is never stale: the watcher emits it whatever the pid or log state, then deletes it. Each tool's next capture `open` also sweeps its own dead-pid entries, so stale files do not wait for a relais session. The tools never depend on the watcher.
- **No new dependency in relais for watching.** Poll the directory and each log every 250 ms (a handful of files; `notify` is not worth its tree). Reads at an offset, at most the last 64 KiB per poll (seek to max(offset, len-65536)), emitting at most 10 lines per run per poll; a log that shrank or vanished ends that row.
- **Pane budget, stated once.** Order: the relais output cap is computed first, exactly as today (`pane.ts:212`); quiet head rows then count in `fixed`; quiet runs share `min(10, remainder / quietCount)` output rows each. At most 3 quiet head rows, live runs first (oldest first), then kept failed runs, the rest under a dim `+n more quiet runs` (as `pane.ts:146` does for agents). `PaneState` and `layoutPane` gain a `quiet` field; the early return in `layoutPane` (`pane.ts:205`) becomes "no runs AND no quiet runs".
- **Drawn below relais runs, Text only (the person chose this, 2026-10-11: no mockup round).** Quiet runs sit BELOW all relais runs in the pane's existing row style: the head row is one dim Row `amont · clippy · 0:12`, then dim `  │ ` lines (the prefix of `pane.ts:201`); a quiet run with no line yet draws a dim `  │ waiting for output`. Text only, no Buttons, so the agent Buttons' keyboard order is unchanged. The pane change is shown to the person on fixture-fed renders in the verification step before release.
- **No flash.** A quiet run is drawn only once it has run 1 s; a run that ended before 1 s is never drawn, unless it failed.
- **A failed quiet run stays visible (the person decided, 2026-10-11: "failure stays, pass drops").** `ended` carries `exit` (null when the entry vanished without one). A failed quiet run keeps its head row (error colour) and last lines until the next quiet run of the same tool and check starts (concurrent sibling checks never clear it), as a failed relais run does (`pane.ts:220-222`); a passed one drops at once.
- **The pane opens on its own (the person decided, 2026-10-11: "open it, toast if no room").** The first quiet run in a session to pass the 1 s anti-flash (or to end failed) calls `openPane`, never a sub-second pass (no `announceRun`, no "relais run started" toast). If the pane cannot be placed (narrow terminal), one toast per session: `<tool> <check> running: /relais-status`. The status line lists live quiet runs.
- **Only agent-run quiet runs appear.** That follows from the session filter; a person's own terminal runs are not quiet and write no entry.

## Phases

### amont (one PR)

- [x] **1. Active-run entry.** (amont#318; written only inside a session) In `crates/amont-runtime/src/capture.rs`, `Capture::open` first sweeps this tool's dead-pid entries and any ended entry older than 60 s, then writes the entry after the `.tmp` opens (session from `CLAUDE_CODE_SESSION_ID`, repo from the git toplevel); `Capture::finish` and a new `impl Drop for Capture` rewrite it with `"exit"` and `"ended"` (`exit` null on Drop without a code; idempotent via a `done` flag) and leave it for the watcher or a later sweep to remove (an early return or panic must not leave a live-pid entry); a write failure is ignored (the excerpt and log never depend on it). The runtime crate has no dependencies, so the JSON is hand-written with a tested string escaper (test: a path with a quote and a backslash); `<n>` comes from a process-wide `AtomicU32`; on Windows the state dir is `%LOCALAPPDATA%` (or the entry is skipped). Unit tests: entry written with the documented fields, rewritten with `exit`/`ended` on finish and left in place, a second concurrent capture in one process gets its own file, a dead-pid entry is swept on open, unwritable state dir → capture still works.

### amont-agent (one PR)

- [x] **2. Active-run entry.** (amont-agent#100, released v2.37.0) Same in `src/quiet.rs` (`check` = the program name, `tool` = `amont-agent`): sweep on open, write `exit`/`ended` at the end and leave the entry for the watcher or a later sweep. Tests in `tests/quiet.rs`: entry present while a `sleep` child runs; after exit and after SIGINT it carries `exit`/`ended`; a later `quiet` run's sweep removes it once older than 60 s.

### relais (one PR, owns the plan)

- [x] **3. `relais quiet watch --protocol`.** New clap subcommand, no `--session` flag: polls the directory, drops dead-pid entries, tails each log from an offset (a new row starts at the last 16 KB, an incomplete trailing line is held, `\r` splits lines, one last drain happens when the entry disappears). Line cutting is the same as `cutLine` (`timeline.ts:112-120`): CSI sequences stripped before control characters, tabs to two spaces, length counted in UTF-16 units like the TS (240). It emits one JSON line per event — `{"relais":"quiet","event":"started","id","session","tool","check","repo","started"}`, `{"relais":"quiet","event":"output","id","lines":[…]}`, `{"relais":"quiet","event":"ended","id","exit","ended"}` (`exit` null when the entry vanished without one) — plus a heartbeat line every 5 s. It never relies on stdin: it exits when a stdout write fails (the heartbeat makes a dead reader visible) or when its parent pid changes. Tests (Rust): a null-session entry is emitted with `session` null; each `started` carries its session; appended lines arrive in order; a removed entry yields `ended` carrying `exit`; a dead-pid entry is never `started`; a line-cutting fixture (ANSI colour, a tab, wide characters) matching `cutLine`.
- [x] **4. Plugin: spawn, fold, draw.** `register.ts` spawns the watcher through `$.process.spawn` on its own path: no `store.models` entry, no `store.starting`/`childOf`, no `announceRun`; its exit never draws a run. The spawn is idempotent like `ensurePump` (one watcher per plugin module; it outlives /clear and /resume); the host kills it at session end/unload. Restart policy, one rule: exit code 2 (an older relais without `quiet watch`: clap usage error) → noted once, never respawned, nothing drawn; any other exit while the module lives → respawned at most once per 30 s, and meanwhile the pane shows one dim `quiet watcher down` line. The watcher's own read loop takes each stdout chunk from the stream it holds (`step.value`; the module's `process.spawn` hook only reads `childOf` children, `register.ts:337-339`, and `drain` discards chunks, `runs.ts:261-262`), keeps its own partial-line carry, and `runs.ts` folds `"relais":"quiet"` lines into a new `quiet` map in the store (`timeline.ts`-style fold, output capped at 10 lines via `appendOutput`; `{"relais":"quiet","event":"heartbeat"}` is ignored and does not `markDirty`). Every entry is kept; the pane and status line draw only those whose `session` is in `store.sessions`, so a session added after a run's `started` (/resume) still shows it; a test splits one protocol line across two chunks; the first quiet run of a session to pass the 1 s anti-flash, or to end failed, calls `openPane` (toast rule in Decisions); a sub-second pass never opens it. `pane.ts` draws the quiet section per Decisions (below all relais runs, Text only, dim `  │ ` lines, `  │ waiting for output`, 1 s anti-flash, failed run kept) inside the budget stated there; `ui.ts flush` treats a live quiet run as live (1 s refresh). `tests/support.ts` gains a `quiet` branch in its `process.spawn` fake (as `learn`/`resume`) with its own feed, so `calls.spawn` stays runs only. Tests in `tests/views/relais-pane.test.ts` with fed protocol lines: a quiet run draws its head and last lines; a passed run drops on `ended`, a failed one stays; quiet rows are `Text` only, drawn after all relais runs, and the agent Buttons' order is unchanged; a watcher live with no quiet run leaves the empty state unchanged and `hasLiveRun` false; first quiet `started` opens the pane and a second does not toast again; exit 2 → no respawn; crash → respawn after 30 s; a pane test that fails without the session filter (sessions A/B/C); a pane test that fails without the relais-first budget.
- [x] **4b. Status line.** `statusText` (`ui.ts:55`) adds live quiet runs (`amont clippy 0:12`) so an unplaced pane still shows them; test it.
- [x] **4c. Person review.** Before any release, show the person fixture-fed pane renders (live, waiting, failed, 3 heads plus `+n more`, at 8 and 24 rows) and take their call.
- [x] **5. Contract doc.** `docs/quiet-runs.md` (relais): the entry file, its fields (`exit`, `ended`) and lifetime, the session filter in the plugin; README pane section mentions quiet runs.
- Releases: amont, amont-agent, relais via `tag-release`, in that order; brew upgrades; relais plugin reinstall (`relais install --claude --user --write`) is the person's step, as before.

### Live (agent-observed)

- [ ] **6. Observe.** In a fresh session after the reinstall: run `amont run clippy --all-files` on amont-agent and `amont-agent quiet -- sleep 20` through the agent; the pane opened on its own and shows each row with live lines and drops it at the end; a second Claude session's quiet run does not appear in this one's pane.

## Decision log

- 2026-10-11: the relais runs for phases 3 and 4 ended `needs_review` (their reviewers stopped without a result; `make check` passed on both candidates); the session reviewed both against the carried findings and committed them.
- The watcher re-reads a log that shrank from its start instead of ending the row (amont's logs only grow; re-reading is harmless), and its first heartbeat goes out at startup; `docs/quiet-runs.md` states both.
- The ended entry names the renamed `.log` when the tool's rename succeeded (amont and amont-agent agree); the watcher's last drain reads it at the offset it kept from the `.log.tmp`.
- An entry is written only inside a Claude Code session (both tools): a session-less entry is never drawn, and amont's own test suite, run under an agent's gate, had written 27 fixture entries into the real directory under that agent's session.
- Pid-reuse guard: a live pid also needs its log to exist and the log or the run to be younger than an hour; an ended entry that cannot be deleted stays closed instead of being announced every poll.
- Kept deliberate: live vs failed stays an optional `failure` field on `QuietRun`, not a union (implementation review, low): the refactor touches the fold, pane and tests for no behaviour change.
- The watcher tests wait for its first line (up to 60 s) before timing anything: `cargo test` relinks the binary with other features than `cargo build`, and macOS held the first exec in `_dyld_start` past the tests' 6 s windows at load ~250.
- amont-agent's atomic temp is a dotfile ending in `.json`; the watcher skips dotfiles, or the temp's rename would end a run as killed and keep a failed row (found closing amont, 2026-10-12).
- Under load an amont-agent test read that temp just before its rename; the test helper skips dotfiles too (amont-agent#101).
- End to end, branch binaries (2026-10-11): a failing `amont-agent quiet` run gave `started` (its session) → one `output` per line, live → `ended` exit 3 → entry deleted; `amont run clippy --all-files` gave `started` → live `output` → `ended` exit 0.

## Non-goals

- Showing a person's own terminal runs (they are not quiet and stream already).
- A pane for sessions without the relais plugin.
- Replacing the `running <check>, log <path>` line.

## Verification

| Input | Expected | Actual |
|---|---|---|
| amont capture opened then finished (unit) | entry file with documented fields appears, then carries `exit`/`ended` and stays until the watcher or a sweep removes it | `active.rs` unit tests: fields, then `exit`/`ended`, left in place; `quiet_run.rs`: a quiet run leaves an ended entry with its result |
| two concurrent captures in one amont process | two entry files | two files (`<pid>-0`, `<pid>-1`) |
| capture `open` with a dead-pid entry present | the stale entry is swept | swept on unix; off unix every pid counts as alive, so only ended entries go |
| `amont-agent quiet -- sleep 5` | entry present during; after exit and after SIGINT it carries `exit`/`ended` and a later sweep removes it once older than 60 s | `tests/quiet.rs`: present during, `exit`/`ended` after exit and SIGINT, swept past 60 s; a command that cannot start records 127 |
| pane test: `started` for A and B, store.sessions={A}; then /clear adds C, C `started` | A's and C's rows drawn, B's never | pane tests (`quiet-pane.test.ts`): only A and the /resume-added C draw; B never |
| append 3 lines to a watched log | one or more `output` events carrying them in order | `live_entry_streams_started_output_and_ended`: in order, partial line held until its newline |
| remove an entry / dead-pid entry | `ended` / never `started` | `ended` with exit null / `a_dead_pid_without_ended_is_never_started` |
| line-cutting fixture: ANSI colour, a tab, wide characters | same cut as `cutLine` | `lines_are_cut_as_timeline_cut_line_does` (a surrogate pair split by the cut is dropped whole; a Rust string cannot hold half) |
| pane test: fed `ended` with exit 1 | head row in error colour and last lines stay until the next quiet `started` | kept in error colour until a `started` of the same session, tool and check; siblings and other sessions leave it (carried finding 3) |
| pane test: fed `ended` with exit 0 | row drops at once | drops at once |
| pane test: fed `started` + `output` | head row `amont · clippy · 0:0x` and the lines, dim `  │ ` prefix | `amont · clippy · 0:01` + dim `  │ ` lines (person saw the render, 4c) |
| pane test: `started`, no `output` yet | `  │ waiting for output` | `  │ waiting for output` (render 2, 4c) |
| pane test: relais run + quiet run, 8 and 24 rows | relais rows equal the same test without the quiet run (baseline); quiet run gets only the remainder | relais rows equal the baseline at 8 and 24; the quiet head and one line follow (render 5, 4c) |
| pane test: 6 concurrent quiet runs | 3 head rows + `+3 more quiet runs` | 3 heads + `+3 more quiet runs` (render 4, 4c) |
| watcher live, no quiet run | empty state unchanged, `hasLiveRun` false | empty state unchanged, `hasLiveRun` false |
| watcher exits 2 / crashes | no respawn, nothing drawn / respawn after 30 s with `quiet watcher down` meanwhile | not respawned / respawned after 30 s with `quiet watcher down`; a dead watcher's live runs are dropped |
| first quiet `started` in a session; then a second | pane opens; no second toast | pane opens on the pump tick once a run passes 1 s (not on `started`); no second toast (carried finding 1) |
| pre-commit with sub-second checks | none flashes; the pane does not open | pane test: a sub-second pass never draws or opens the pane; a sub-second failure does |
| pane test: `ended` with exit 1 under 1 s after `started` | head row and last lines drawn; the same with exit 0 draws nothing | drawn with its lines; exit 0 draws nothing |
| live: agent runs `amont run clippy --all-files` | row appears with live lines, drops at end | — |
| live: quiet run from another Claude session | not shown | — |
| live: `amont-agent quiet -- sh -c 'for i in 1 2 3 4 5; do date +%s; sleep 2; done'` | each line in the pane within 0.5 s of its timestamp | — |

## Implementation review

- relais: approve on tree 4526ed37, after three deltas (resumed reviewer). Fixed: ended runs are closed once, sweep errors are reported once, the dotfile temp is skipped, and the Windows test build works.
- amont: approve-with-changes (delta on c66d6af). The contract wording above is fixed here; the CHANGELOG session line goes in amont 1.51.0.
- amont-agent: approve on tree 1de7f960; released v2.37.0.
- Next: phase 6, live, after the person reinstalls the plugin.

<!-- panel: repos=relais,amont,amont-agent adds=lang:typescript,ui reviewers=backend,language:rust,language:typescript,react,ui-design,ux-research,game-ux body-sha=6e179a158a84 -->
