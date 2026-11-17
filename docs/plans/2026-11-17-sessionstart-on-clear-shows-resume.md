---
status: done
branch: feat/resume-on-clear
repos: [relais, amont-agent, decisions]
adrs: [ADR-0062, ADR-0075]
plan-id: p-01M7M5THKW6CY49NCHMFMHC1JJ
---
# SessionStart on clear shows how to resume

## Review panel

👉 **Decide:** keep po's "Measure after release" section and worst-checkout budget row (not exempt: words ×1.48)? Recommended: keep — they are the only post-release signal.
📍 relais, greenbelay, decisions · 4 phases · 2 releases · next: Phase 1 in relais. Panel: backend, lang:rust, tui, unix, architect, po (opus-5-5), codex.
**Changed by review:** split the format contracts (inbox → greenbelay, Handoff → relais, versioned); a 3 s threaded deadline, no index locks; pointers keep each programme's source.
**Verdicts:** 4 approve-with-changes, 3 approve (final); 15 carried into implementation; words 1490 → 2209 (×1.48); remove 0, merge 0.
📄 Full reviews: [2026-11-17-sessionstart-on-clear-shows-resume.reviews.md](2026-11-17-sessionstart-on-clear-shows-resume.reviews.md)

## Picture

```
 today                            after this plan
 -----                            ---------------
 /clear --> SessionStart          /clear --> SessionStart
              |                                |
              +--> greenbelay:                 +--> greenbelay: plans,
              |    plans line                  |    + inbox orphans,
              |                                |      fresh lines counted
              `--> relais hook:                `--> relais hook: ready
                   (silent)                         tickets, due first,
                                                    + handoff pointer
```

A cleared session used to open knowing only its active plans. After this
plan, relais names the ready programme tickets, with due ones first, and
greenbelay names the inbox lines that nobody filed. Each tool shows only
what it owns.

## Context

This is ticket t05 of the programme session-boundaries
(`decisions/docs/programmes/session-boundaries/t05-resume-on-clear.md`).
After t04, the agent writes a handoff and ends a boundary answer with
``safe to /clear; next: `<prompt>` ``. clear-relay then clears and submits
the prompt. This was proven live on 2026-11-17: printed at 07:11:00Z,
cleared at 07:11:10Z.

From greenbelay, the new session's `SessionStart:clear` got only the
grep-shim notice. Nothing named the programme, the due ticket or the
inbox. The relayed prompt covers the relay path. A plain `startup`, or a
`/clear` typed without a prompt, has nothing. t02 promised that
"SessionStart shows the orphans", and nothing does.

The ticket asked greenbelay for all of it. ADR-0075 (`harness.tool-boundaries`, replacing ADR-0073)
says otherwise: greenlift (relais) owns "a programme's frontier", and no
dependency points up from greenbelay. On 2026-11-17 the person chose to split
the work by owner.

## Goal

On a `startup` or `clear` session start in a repository:
- **relais's `relais hook`**, already installed as a SessionStart command
  hook, prints the frontier of the repository's programmes, due tickets
  first. It computes it with the same code as `relais programme frontier
  --no-fetch`.
- **greenbelay's notice** adds the inbox lines nobody filed. It sits next
  to its board, which is "what is in flight across sessions".

Each one names only itself and works without the other.

## Non-goals

- No `git fetch` at session start.
- `resume`, `compact` and `fork` get neither block. Those sessions keep
  their conversation, or are compacted from it.
- No filing of inbox lines and no write to any repository (index, refs, inbox). The only writes are each tool's own journal line.
- No programme validation at session start. A programme that fails to
  load is skipped, and counted.
- No change to the relais plugin (TypeScript). The command hook is the
  surface.

## Code review

- relais: backend, lang:rust, unix, tui
- amont-agent: backend, lang:rust, unix, tui
- decisions: none — markdown only

## Behaviour

### relais: `relais hook` on SessionStart

The envelope also reads `source` and `cwd`. `HookEvent::SessionStart`
carries both, each optional.

For `source` = `startup` or `clear` with a `cwd` inside a git repository,
`respond::handle_inner` gathers the frontier (I/O, like `ask_coordinator`) and passes it to `decide`, which stays pure and returns a new `HookAnswer::Context(text)`; the journal records `decision: "context"`. It is printed as
`{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":…}}`:
`stdout_payload` names the event of the answer, no longer a fixed
`PreToolUse`, with a unit test, and the journal's outcome set gains
`context`. SPEC §23's "no variant that only advises" sentence is
rewritten to name this SessionStart exception.
Any other source, a missing cwd, or no ready ticket means `Silent`, as
today.

**Programmes.** These are `programme::load::programmes_of(<main checkout of
cwd>)`, which reads the checkouts and origin's last-fetched view (no
fetch). On the hook path every git child runs with `GIT_OPTIONAL_LOCKS=0`, so the per-checkout `git status` never takes `index.lock`. Each is loaded, its `standings()` taken, and its frontier tickets
kept. The ticket set and the due order are exactly what `frontier_command`
computes, through a shared helper extracted from it rather than a copy. The helper returns rows and prints nothing. The hook's stdout carries only its one JSON line, and `Listed.notes` are dropped:
- due tickets first, soonest first, across all programmes;
- then each programme's undated tickets in id order, programmes by slug;
- `[due D]` / `[overdue D]`, where today counts as due.

**Text.** The header is `relais/programmes: N ready in <main checkout>:`.
Preserve each programme's `Found.checkout` and `Found.source` through gathering
and rendering. Then:
- one line per ticket, `- <slug> <id>: <mark><name>`, cut at 200
  characters with a trailing `…`;
- a cap of 8 ticket lines, then `- … and N more ready (M programmes)`;
- one source locator per programme with a shown ticket: its selected absolute
  programme directory for `WorkingTree`; for `Ref`, the repository, ref and
  a shell-quoted `git -C <repo> show <ref>:docs/programmes/<slug>/<id>.md`
  read command. Do not imply ref-backed files exist in the checkout;
- at most 3 handoff pointers, for programmes with a shown ticket whose selected
  map contains `## Handoff` outside a fence. Inspect and locate the map through
  the same source as its tickets, using `map.md` in the locator above;
- integration tests cover conflicting main/linked-worktree copies and a
  programme present only in origin, asserting that ticket and handoff locators
  retrieve the selected content;
- `(K programmes could not be read; run `relais programme check <dir>` for why)` added to the header when a load
  fails.

**Budget.** The hook's 10 s timeout must never be the limit. Measured on
decisions (9 programmes): `frontier --no-fetch` takes 0.02 s each. That figure is for one programme. The hook runs `programmes_of`, which runs one `git status -uall` per checkout, so it is measured whole on the checkout with the most worktrees before release. The gather runs on a `std::thread` and the hook waits with
`recv_timeout(3 s)`; on timeout it journals `skipped(deadline)` and returns
`Silent` (each git call alone may block up to `GIT_TIMEOUT`, 30 s; the
leftover child is harmless, it takes no locks). The gatherer is a
parameter, so a unit test injects a slow one. A repository with no
`docs/programmes` costs the main-checkout lookup, a `stat` per checkout and
at most one `git cat-file` (`may_hold_programmes`), timed in Phase 1.

**Failures.** Every error is `Silent`. The hook never fails a session
start. The hook record gains `context` and `skipped(<why>)` outcomes with
elapsed ms.

### greenbelay: the inbox block

`on_session_start` (`src/hook.rs`) passes `session.source` to a new
`greenbelay_process::inbox::notice(cwd, source, stance)` (imported in
`src/main.rs`), journals in `hook.rs`, and pushes its result after
`plans::notice`.

- **Sources:** only `startup` and `clear`.
- **Stance:** the block follows the `clear-at-boundary` rule's stance.
  - `observe` journals `session start <source>: <o> orphans, <f> fresh`
    and says nothing.
  - `advise` and `deny` speak, and journal the same line as `advised`.
  - The journal line is written when any scanned inbox or take has a line,
    including when `inbox.md` is missing or empty.
- **Read:** `<git-common-dir>/greenharness/inbox.md`, and every
  `inbox.*.filing` beside it whose mtime is more than 24 h old, the same
  test as t04's `find -mtime +0`. A line has the form
  `<YYYY-MM-DD> <session8> <text>`.
- **Orphan:** a line dated two or more days before today (t04's reading of
  t02; stricter than t02's "session unseen for 24 h", noted in t05's
  Resolution), or any line in an abandoned take. The grammar includes
  t02's optional `[due=YYYY-MM-DD]`.
  - Shown in full, cut at 200 chars (`chars()`, never a byte slice) with
    a trailing `…`, at most 5, then `… and N more`. Dates are local calendar days, as the skill's `date +%F`
    writes them; relais's due marks keep its
    `chrono::Local` day, as `frontier` does.
- **Fresh:** every other line, counted, with its distinct session ids.
  - `N lines under 2 days old, of sessions ab12cd34, ef567890`, then
    `a live session files its own at its boundary; after a /clear the
    cleared session cannot, so file those the map's Handoff names`.
  - A malformed line counts as fresh, under session `?`.
- **Text:** the header is `greenbelay/inbox: <path> holds N unfiled lines
  (nothing was moved):`. The orphan block ends `any session may file an
  orphan as a task ticket`.
- A take younger than 24 h is counted as `N lines being filed`, not shown.
- Scan `.filing` takes independently of whether `inbox.md` exists or has
  content. Print nothing only when the combined scan has no lines. A missing
  file, including one removed during scanning, contributes no lines; other
  read errors are silence.
- Hook tests cover an abandoned take beside a missing inbox and beside an
  empty inbox: `startup` and `clear` show its orphans under `advise`/`deny`;
  `observe` emits no block but records their count.

## Phases

- [x] Phase 1 — relais frontier at session start:
  `crates/relais/src/hook/event.rs` (source, cwd),
  `crates/relais/src/hook/decide.rs` (`HookAnswer::Context`, payload),
  `crates/relais/src/hook/respond.rs`, `crates/relais/src/hook/mod.rs`,
  a new `crates/relais/src/hook/resume.rs` (gather and render, with unit
  tests), `crates/relais/src/cli/programme.rs` (extract the shared
  frontier helper), `crates/relais/tests/it/programme_cli.rs` (a hook
  run per source on a scratch repository), `docs/SPEC.md` (§23 hook,
  §31 programmes, which documents the map's `## Handoff` heading as relais's
  contract, with a version token), `CHANGELOG.md`.
- [x] Phase 2 — greenbelay inbox notice (greenharness/greenbelay#284, released in 6.3.0 with #285):
  `crates/greenbelay-process/src/inbox.rs` (new, with unit tests),
  `crates/greenbelay-process/src/lib.rs`, `src/hook.rs`, `src/main.rs`,
  `tests/hook.rs`
  (a SessionStart per source in a scratch repository with its own
  common dir), `docs/index.md` (documents the inbox line grammar and the
  `.filing` take as greenbelay's contract, with a version token, per
  `harness.cross-tool-reads`; the worktree-task skill conforms), `CHANGELOG.md`.
- [x] Phase 3 — releases through the `tag-release` skill, each as soon as
  its phase merges (relais does not wait for greenbelay). Done: relais
  v0.69.0 (#492) and greenbelay 6.3.0 (greenharness/greenbelay#285), both
  installed with brew; the machine now runs relais 0.69.0 and greenbelay 6.3.1:
  - relais: `Cargo.toml`, `Cargo.lock`;
  - greenbelay: `Cargo.toml`, `Cargo.lock`, `greenharness.toml`. This
    waits until session `32290a5c`'s 6.1.0 has landed; re-run `greenbelay
    board` first.
  - Install both with brew.
- [x] Phase 4 — live proof (done 2026-11-17; the live `/clear` missed, see Verification and Outcome), in t05's Resolution, its `repos`, and the map
  (`decisions/docs/programmes/session-boundaries/t05-resume-on-clear.md`,
  `decisions/docs/programmes/session-boundaries/map.md`):
  - the installed binaries on a real SessionStart payload in decisions, once
    per source;
  - then a real `/clear` at this session's boundary. The next session quotes
    its `SessionStart:clear` context as its first act; the handoff line's
    `next:` prompt asks for that quote, since clear-relay submits only
    that prompt.

## Decision log

- 2026-11-17 — The work is split by owner (the person's choice): relais
  prints the frontier and greenbelay the inbox. ADR-0075 gives the frontier
  to greenlift and forbids upward dependencies. The ticket predates that
  reading.
- 2026-11-17 — relais answers from its existing `relais hook` command,
  not from the TypeScript plugin. The command is already installed for
  SessionStart, is Rust, and shares `programme::load`. No plugin in the
  tree returns SessionStart context today.
- 2026-11-17 — Each map's `## Handoff` gets a pointer line, not its text.
  The text runs to about 25 lines, and the relayed prompt already carries
  the next step.
- 2026-11-17 — The inbox block follows the `clear-at-boundary` stance and
  does not get a new rule: it is the other half of the same loop.
- 2026-11-17 — Fresh inbox lines are counted, not shown. A live session
  files them, and showing them invites a double filing.
- 2026-11-17 — If decisions holds no orphan at the live `/clear`, the inbox
  path is proven by the piloted run on a scratch repository with a dated
  line, and the Resolution says so. No line is written into a real inbox to
  stage the proof.

- 2026-11-17 — The plan's figure of 0.02 s per programme came from a call
  that failed on a wrong directory. The real cost was about 0.4 s per
  programme, from git calls made one after another and a 20 ms poll per
  child. A perf commit reads checkouts, source choices and loads on scoped
  threads, with the results in input order, and starts the poll at 1 ms,
  doubling to 20 ms. That took decisions from 2.7 s to 0.53 s.
- 2026-11-17 — The two small budget misses (0.53 s against 0.5 s, and
  0.07 s against 0.05 s) are accepted. The worst-checkout bar is met (0.53 s
  against 1 s), and the deadline is 3 s. What remains is the chain of
  dependent git calls itself.
- 2026-11-17 — `GIT_OPTIONAL_LOCKS=0` and the doubling poll apply to every
  relais git child, not only the hook path. A read-only status never needs
  the optional index refresh, and a long check still wakes once per 20 ms.
  Recorded here at implementation review's request, and documented in
  `git_command` and in the changelog.

## Verification

| Check | Input | Expected | Actual |
| --- | --- | --- | --- |
| relais unit | programmes with closed/open blockers, due past/today/future, a `prog/` branch, a failing load | the set and order are `frontier --no-fetch`'s; marks; cap line; failure count; handoff pointer only outside a fence, at most 3 | pass (greengate pre-commit cargo-test on ed8bbcdd, 30 checks) |
| relais it | `relais hook` fed SessionStart `startup` and `clear`, on a scratch repo with a programme | stdout JSON with `additionalContext` naming the tickets | pass |
| relais it | `startup` on a scratch repo with a linked worktree | `.git/index` mtime unchanged | pass; the `index.lock` assertion was dropped in review, since git removes the lock on exit, so the mtime is the evidence |
| relais it | the same with `resume`, `compact`, `fork`, without cwd, and in a repo with no programmes | empty stdout, exit 0 | pass |
| relais it | conflicting main and linked-worktree copies; a programme only in origin | each locator, run as printed, returns the selected ticket and its handoff | pass |
| greenbelay unit and hook tests | as in the rows above, plus an abandoned take beside a missing or empty inbox, a bad byte, and the stance | as Behaviour | pass (greenbelay#284 CI on macOS, Ubuntu and Windows); piloted: `startup` and `clear` show 2 orphans and 1 fresh line, `resume`/`compact`/`fork` show none |
| gates | both repos' greengate pre-commit (test, clippy, fmt) | pass | pass |
| budget | release build `relais hook`, SessionStart `clear`, in decisions and in a repo without `docs/programmes` | under 0.5 s and under 0.05 s | **missed by a little:** decisions 0.52–0.57 s, the repo without programmes 0.06–0.07 s (load 3.5). Before the perf commit, decisions took 2.7 s and `startup` hit the 3 s deadline and went silent |
| budget (worst) | decisions, the checkout with the most linked worktrees (7 worktrees, 9 programmes) | under 1 s; at or over 3 s blocks the release | 0.53 s: met. relais (5 worktrees) 0.37 s |
| output | the same hook, sequential build against the parallel one | byte-identical context | identical |
| CI on Windows | relais#490, first push | the test matrix passes | 4 render tests failed: they expected `/`-joined directories. `Found::dir` joins one relative `docs/programmes/<slug>`, so on Windows only that join is a backslash (`/work/repo\docs/programmes/alpha`), and `shell_quote` then quotes it. The first fix guessed backslashes throughout and failed again on the second push. The test helper now joins exactly as `Found::dir` does |
| piloted | the installed binaries, a SessionStart JSON per source, cwd decisions | the context quoted in t05 | pass: `startup` and `clear` printed `relais/programmes: 14 ready`, due tickets first, in 0.48–0.49 s; `resume`, `compact` and `fork` printed nothing. A replay after t13–t18 were filed printed `21 ready`, led by t12 `[due 2026-11-24]`, with no `greenbelay/inbox:` line (the inbox is empty) |
| live | the `/clear` at this session's boundary | the next session's first message quotes `relais/programmes: N ready`, due tickets first (t12 `[due 2026-11-24]` if ready), t11 listed, and the greenbelay inbox block, or no `greenbelay/inbox:` line when the inbox is empty | **missed:** session 8f33d4fe was cleared at `~/Developer/Perso`, which is not a repository. relais printed nothing, and greenbelay printed only its grep-shim (no plans or inbox line); both are silent outside a repository by design. clear-relay did submit the handoff's `next:` prompt. Filed as decisions session-boundaries t19 |

## Measure after release

- Over 7 days of relais hook records for SessionStart `startup`/`clear` in
  repos with `docs/programmes`: the share `context` vs `skipped(*)`, and
  p95 elapsed ms.
- Roll back (stop the hook speaking) if any `skipped(deadline)` appears or
  p95 exceeds 1,000 ms.
- greenbelay: orphans named at a start and still unfiled a day later;
  target falling.

## Implementation review

- implementation-review: approve-with-changes, then approve-with-changes on the delta. The blocking `_ =>` over `HookEvent` and the `value` names are fixed; the plan ticks, the observed results and the budget misses are recorded here. 111k + 130k.
- backend: approve-with-changes, then approve. Filed as inbox follow-ups: `skipped(error)` drops the error text, and the scoped threads are not panic-silenced. 80k + 104k.
- lang:rust: approve-with-changes, then approve. Deliberate: the thread fan-out stays uncapped, with `holds-until` comments, and `GIT_OPTIONAL_LOCKS` stays global and documented. 80k + 103k.
- unix: approve in round 1, carried to this tree (no rework). tui: approve-with-changes on both rounds. Filed as an inbox follow-up: the overflow command lists only the first programme. 48k + 70k.

## Outcome

Shipped and installed: relais v0.69.0 names the ready programme tickets, with due ones first, at a `startup` or `clear` SessionStart, and greenbelay 6.3.0 names unfiled inbox lines. This is proven in decisions by a replay of the installed binaries for each source. The live `/clear` at the boundary missed, because that session started at the non-repository Perso root and neither hook reads anything outside a repository. Sessions often start there, so resuming from SessionStart alone does not yet hold in the common case: decisions session-boundaries t19. The 7-day measure above runs to 2026-11-24.

<!-- panel: repos=relais,amont-agent,decisions adds=contract reviewers=backend,lang:rust,tui,unix,architect,po,codex body-sha=7bac5f528f09 -->
