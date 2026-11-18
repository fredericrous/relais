---
status: active
branch: feat/resume-outside-a-repository
repos: [relais, amont-agent, decisions]
adrs: [ADR-0075, ADR-0077]
plan-id: p-01M7QPAQ11ZKVEGSFJAXP1Q7RE
---
# A cleared session outside a repository still sees how to resume

## Review panel

👉 **Decide:** none. Approve if you accept the bet that a 2.5 s parallel read of child checkouts (1.7 s warm, measured) fits under the 3 s hook deadline.
📍 relais, greenbelay, decisions · 3 phases · 2 releases · next: Phase 1 in relais. Panel: architect, backend, lang:rust, po, tui, unix (opus-5-5), codex.
**Changed by review:** detached threads with an absolute cutoff and no git after it; map commit time, not mtime; `render` pins the winning Handoff; the journal counts unread.
**Verdicts:** 4 approve, 2 approve-with-changes (final); 9 carried; words 1237 → 1702 (×1.38, mostly exempt high or correcting edits); remove 0, merge 0.
📄 Full reviews: [2026-11-18-resume-outside-a-repository.reviews.md](2026-11-18-resume-outside-a-repository.reviews.md)

## Picture

```
 cwd = ~/Developer/Perso (holds repositories, is not one)

 today                          after this plan
 -----                          ---------------
 SessionStart startup|clear     SessionStart startup|clear
   relais hook: (silent,          relais hook: one line per child
     skipped(error))                checkout with ready tickets, then
   greenbelay: no inbox             the full block of the repository
                                    whose map holds the latest Handoff
                                  greenbelay: the inbox notice of each
                                    child checkout that has one
```

At the Perso root, a session used to start knowing nothing about the
programmes. After this plan, relais lists every child checkout that has
ready tickets and expands the one that was handed off last, and greenbelay
names each child's unfiled inbox lines.

## Context

This is ticket t19 of the session-boundaries programme
(`decisions/docs/programmes/session-boundaries/t19-resume-outside-a-repository.md`).
t05 made `relais hook` and greenbelay print the frontier and the inbox at a
`startup` or `clear`, but both key on the cwd's repository.

Most sessions start at `~/Developer/Perso`, which holds the repositories but
is not one. There, `main_checkout` fails, `resume::gather` returns
`SkipReason::Error`, and greenbelay's `inbox_notice` returns `None` at
`rev-parse`. This session reproduced it on 2026-11-18: no
`relais/programmes:` line. Only the clear-relay prompt pointed to
`decisions`, so a `/clear` right after a handoff is covered already. A
fresh `startup`, or a `/clear` with no relayed prompt, is not.

**Decided by the person on 2026-11-18:** scan the child checkouts. Each tool
finds them on its own, with no shared config key (every Green Harness part
is optional, ADR-0075). Nothing is configured.

Measured warm, one repository at a time: decisions 0.55 s (37 ready),
homelab 0.68 s (12), amont 0.51 s (0), relais 0.40 s (3), social-planner
0.43 s (1), sre-agent 0.42 s (2). That is ~2.9 s in sequence, against
`GATHER_DEADLINE` = 3 s, which is all-or-nothing. Run as six concurrent
processes, the reads took 1.69–1.76 s wall (3 warm runs, 6 physical cores).
So the reads run in parallel, and the render uses whatever arrived by an
inner deadline. A cold probe is in Verification.

## Goal

At a `startup` or `clear` whose cwd is not inside a git repository:
- **relais** prints a summary line for each child main checkout that has
  ready tickets. It then prints the full t05 block (tickets, locators,
  handoff pointers) for one repository: the one whose map holds the latest
  `## Handoff`.
- **greenbelay** prints the inbox notice of each child main checkout that
  has unfiled lines.

Inside a repository, nothing changes for either tool. Outside one, both
hooks exit 0 on every path, stdout carries only the context block, and
diagnostics go to the journal.

## Non-goals

- No recursion below the cwd's direct children, and no linked worktrees as
  roots. A worktree is reached through its main checkout, as
  `programmes_of` already does.
- No `git fetch`, and no writes except each tool's journal line.
- greenbelay's plans notice stays scoped to the cwd. Plans are per
  repository, and the board covers that across repositories.
- No change to `respond.rs`'s deadline contract or to the plugin.

## Code review

- relais: backend, lang:rust, unix, tui, architect
- amont-agent: backend, lang:rust, unix, tui, architect
- decisions: none — markdown only

## Behaviour

### relais: `resume::gather` at a non-repository cwd

`gather(cwd)` behaves as before when `main_checkout(cwd)` succeeds. When it
fails:
1. **Find.** It calls `load::repositories(cwd)`, the existing direct-child
   `.git`-directory filter (`programme/load.rs:886`). It keeps those with
   `docs/programmes` on disk, a filesystem test only. That avoids the git
   calls `may_hold_programmes` would make for ~60 repositories. A programme
   that exists only upstream or only in a worktree is missed here, and that
   is the accepted cost, named in a `holds-until` in `docs/SPEC.md` §23.
   Both tools skip a child that is a symlink (`symlink_metadata`).
2. **Read in parallel**, through `collect_within(repos, read, deadline)`
   with the reader injected for tests. Each kept repository gets one
   detached `std::thread::spawn` thread that owns its `PathBuf` (never
   `thread::scope`, which joins past the deadline). The thread runs
   `programmes_of` → `frontier::ready`, reads `has_handoff` once per
   programme, and computes the repository's handoff time: the newest map
   commit time among its programmes with a `## Handoff`, `git log -1
   --format=%ct [<rev>] -- <map>` (git does not keep mtimes, so a pull
   would win). The file mtime is used only for a map with uncommitted
   changes. Results come back on a channel, and a panicked thread counts as
   unread. The cutoff is absolute: an `Instant` taken at `gather` entry,
   before `main_checkout`, plus `OUTSIDE_REPO_DEADLINE` = 2.5 s, so
   discovery and spawning spend the same budget. `collect_within` takes that
   cutoff, then renders what arrived. Unfinished threads are left to finish, as in
   `gather_within`: git runs with `GIT_OPTIONAL_LOCKS=0`, and every git child
   has stdin null and stdout/stderr piped, so none holds the hook's stdout.
3. **Render**, through a pure `render_outside(root, per_repo, today)` with
   unit tests:
   ```
   relais/programmes: 55 ready in 5 repositories under /Users/…/Perso (not a repository):
   - decisions: 37 ready, 5 due (first 2026-11-24); cd /Users/…/decisions
   - homelab: 12 ready; cd /Users/…/homelab
   - …
   (1 repository not read in time: amont; relais programme list --no-fetch --root /Users/…/Perso lists it)
   expanded: decisions (latest ## Handoff among those read)
   <the t05 block of that repository, from render() with the winning map pinned>
   ```
   - Repositories with 0 ready are left out of the summary lines, but stay
     candidates for the expanded block.
   - Order: soonest due first, then most ready, then name.
   - At most `TICKET_LINE_CAP` summary lines; any others are counted as
     `… and N more repositories`.
   - Paths are shell-quoted with the existing `shell_quote`. Control
     characters in a printed name are replaced, and a test covers a newline.
   - The full block is `render(&repo, …)` with a new `pinned: Option<&Found>`
     argument: the winning map, always its first `handoff:` pointer, and the
     `0 ready` header when the repository has no ready rows. Inside a
     repository the caller passes `None`, and a test proves that output
     unchanged byte for byte. It takes a `handoff_in`
     that looks up the flags the threads returned: nothing runs git after
     the deadline. It goes to the read repository with the latest handoff,
     and its first `handoff:` pointer is always the winning map, even when
     that programme has no ticket among the first eight or the repository
     has 0 ready (then it prints `0 ready` and the pointer). Tests cover both.
     Unread repositories are not candidates. If no read repository has a
     Handoff, it goes to the first summary line, labelled `expanded: X (no
     repository has a ## Handoff)`.
   - No repository read in time → `SkipReason::Deadline`. Nothing ready
     anywhere and no Handoff in a repository read in time →
     `SkipReason::Nothing`; a Handoff with 0 ready anywhere prints the
     expanded block alone. No child checkout, or the root cannot
     be read → `SkipReason::Error`, as today.
   - The journal line records `read N, unread M`.

### greenbelay: `inbox_notice` at a non-repository cwd

When `rev-parse --git-common-dir` fails, it walks the cwd's direct children
whose `.git` is a directory and whose `.git/greenharness/` exists. These are
filesystem tests only (today that is amont and kb-vision). It reads
`local_date` and the stance once. Then, for each child, it calls the
existing `greenbelay_process::inbox::notice`. Each match gets its own
journal line, with that child's repository name. Each notice is preceded by
`in <name>: cd <shell-quoted path>`, because its `docs/programmes/follow-ups/`
advice (inbox.rs:178) is relative to that repository.
- The notices are joined in name order. At most 3 are printed, and the
  rest become one line: `greenbelay/inbox: N more repositories hold
  unfiled lines: a, b, c, …` (at most 3 names).
- The cwd-inside-a-repository path is unchanged. The walk goes into a small
  `fn child_checkouts(root) -> Vec<PathBuf>` in `src/hook.rs` with unit
  tests. greenbelay does not depend on relais.

## Phases

- [x] Phase 1 — relais reads the child checkouts (relais, worktree from
  origin/main; the plan lands first):
  `crates/relais/src/hook/resume.rs` (the non-repository branch of
  `gather`, the parallel collector, `render_outside` and unit tests),
  `crates/relais/src/programme/load.rs` (the per-repository handoff time,
  beside `repositories`), `crates/relais/tests/it/programme_cli.rs` (a hook
  run at a scratch root holding two repositories), `docs/SPEC.md` (§23 and
  §31: the non-repository cwd), `CHANGELOG.md`. PR → merge-when-green →
  `tag-release` (minor) → install.
- [x] Phase 2 — greenbelay reads the child inboxes (amont-agent, worktree
  from origin/main): `src/hook.rs` (`inbox_notice` at a non-repository cwd,
  `child_checkouts`, unit tests), `tests/hook.rs` (a SessionStart at a
  scratch root with two inboxes), `docs/index.md`, `CHANGELOG.md`. PR →
  merge-when-green → greenbelay minor release → install.
- [ ] Phase 3 — record (decisions):
  `docs/programmes/session-boundaries/t19-resume-outside-a-repository.md`
  (Resolution: the decision, the releases, the live proof; `status =
  "closed"` after the Phase 1 proof, without waiting for Phase 2), `docs/programmes/session-boundaries/map.md`
  (the Done line and the Handoff).

## Verification

- Integration tests: a scratch root with two repositories → relais prints
  both summary lines and the block of the one with the newer Handoff. A
  scratch root with two inboxes → greenbelay prints two notices in name
  order, each after its `cd` line, and two journal lines.
- Unit test: `collect_within` with a sleeping fake reader and a 50 ms
  deadline renders a partial result naming the unread one, and a panicking
  reader counts as unread.
- Built-binary probe, before release and after install: pipe
  `{"hook_event_name":"SessionStart","source":"clear","cwd":"/Users/fredericrous/Developer/Perso",...}`
  into `relais hook` and into greenbelay's hook. The output must show
  `ready in N repositories under` and the block of the repository named
  beforehand from the maps' `git log`. greenbelay must show a notice for
  each child whose inbox has unfiled lines just before the probe. Wall time
  under 3 s, journal `context` with `unread 0`. Repeat once at the first
  `startup` after a reboot (cold cache).
- **Proof (the ticket's):** the next live `/clear` or `startup` at the
  Perso root shows `relais/programmes: N ready`. That needs the released
  relais on PATH (`relais hook` is a command hook, so no plugin reinstall
  is needed for this path).

## Decision log

- 2026-11-18, implementation review: a reader that panics, fails or cannot
  start is `unreadable`, apart from `late`. It gets its own line,
  `(N repositor(y|ies) could not be read: <names> (first: <reason>); relais
  programme list --no-fetch --root <root> says why)`. Child checkouts none
  of which holds `docs/programmes` give `Nothing`, not `Error`. The
  partial-result test blocks the slow reader on a channel with a 1 s cutoff
  instead of sleeping with a 50 ms one. Behaviour lines 118 and 158 and
  Verification line 208 read as approved; this entry supersedes them.

## Verification results

- Built-binary probe, SessionStart `clear` at `/Users/fredericrous/Developer/Perso`:
  - expected: `ready in N repositories under`, the decisions block (newest
    `## Handoff` map commit: greenrack-rename in decisions), under 3 s,
    journal `context` with `unread 0`;
  - actual: `63 ready in 5 repositories under …`, `expanded: decisions`,
    greenrack-rename pinned as the first `handoff:` pointer. 1.28 s wall;
    1.29 s after the review fixes, at load 10. Journal: `context`,
    `read 6, unread 0`, `gather_ms` 1240.
- In-repository output unchanged: the byte-for-byte unit test passes.
- Integration (`a-older`/`z-newer`), `collect_within` partial, unreadable
  line and render unit tests: pass in `cargo test` at each commit gate.
  The `hook::` tests: 108 passed.
- Phase 2 (greenbelay): see greenbelay #296; probe `in amont: cd …` + its
  notice, in 1.06 s.
- Cold-cache probe and the live `/clear` proof: after release, see the
  Measure section.

## Implementation review

- implementation-review: approve-with-changes (2 blocking rule hits, fixed) → approve-with-changes → approve on the fix. 101k+53k+30k.
- backend: approve-with-changes ×2 → approve. Fixed: time-independent integration test, blocking-channel test, `Option` map time, the unreadable line's command. Deliberate: the second root scan. 65k+44k+33k.
- lang:rust: approve-with-changes ×2 → approve. Fixed: late/unreadable split, slug keys, `pub(crate)`, `Nothing`. Deliberate: reason kept as a shown `String`. 64k+36k+24k.
- unix, tui: approve-with-changes, then approve-with-changes → approve on the fix. Deliberate: the `expanded:` label, lines over 80 columns, and no repository name in `(first: …)` (it is the first listed). 84k+53k+35k, 55k+49k+37k.
- architect: approve-with-changes → approve. Fixed: slug keys, the documented thread name, the unread split. 54k+40k.

- CI on #495: `test (windows-latest)` failed one new unit test that spelled the pinned map path with `/`. Fixed by building it with `Path::join` as the other tests do (fb74f1ef); `hook::resume` 30 passed.

Next: Phase 3 (decisions) after relais's release.

## Measure after release

- Baseline: 100% of Perso-root `startup`/`clear` entries are
  `skipped(error)` today.
- Over 7 days at the Perso root: `context` ≥ 90% of starts; entries with
  unread > 0 plus `skipped(deadline)` ≤ 5%; p95 elapsed < 2,600 ms. If
  unread is higher, lower the inner deadline or cap the threads.
- Roll back to the previous relais release if p95 elapsed at the root
  exceeds 2,800 ms, or if `skipped(deadline)` inside repositories rises
  above its rate in the 7 days before the release.

<!-- panel: repos=relais,amont-agent,decisions adds=contract reviewers=architect,backend,lang:rust,po,tui,unix,codex body-sha=285c9ef59c4e -->
