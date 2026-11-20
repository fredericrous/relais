---
status: active
plan-id: p-01M7W0WBH0ABNE7ZWF4JH0S6RN
branch: feat/home-fingerprint
repos: [relais, greenbelay]
adrs: []
---
# Home fingerprint: warn when a run or a test command changes sensitive files

## Review panel

👉 **Decide:** none — approve if a warn-only snapshot of those files is the right net; `~/.config` stays opt-in.
📍 relais, greenbelay · 2 phases · 2 PRs · next: Phase 1 (relais). Panel: backend, rust, unix, tui, architect, po (opus).
**Changed by review:** snapshot moved to run entry/exit; greenbelay piece is an assertion; XDG git config and plugin files added.
**Verdicts:** 6 approve-with-changes, round 2 backend approve-with-changes; 4 carried; words 1186 → 1165 (×0.98); remove 0, merge 0.

📄 Full reviews: 2026-11-20-home-fingerprint-warns.reviews.md

## Picture

```
 today                          after this plan
 -----                          ---------------
 worker/test --writes--> ~      snapshot --> worker/test --> snapshot
          (nobody sees it)                       |
                                     differ? --yes--> warning
                                     (run and command carry on)
```

A worker or test command that wrote to the home directory went unseen before;
after this plan a before and after snapshot of a few files turns that into a warning.

## Context

Two incidents: a leaky test run migrated the real home (11-15), and a migrating
binary's tests touched the live repo (11-11). Both came from test runs in the
session's own shell and from a relais baseline, not from a worker edit. relais has
no OS sandbox (SPEC.md:368-372) and its scope check only sees the worktree diff.
Idea source: claude-code-harness `wtfingerprint` (session e548666f, 11-19).

Decision (user): **warn, never fail or deny.** Phase 1 relais, phase 2 greenbelay.
Both work on macOS, Linux and Windows.

## Watched set (both phases)

Default: `~/.gitconfig`, `~/.claude/settings*.json`, `~/.ssh/**` (minus `known_hosts*`,
which ssh appends to), `~/.netrc`, the repo's `.git/config`; plus, because the 11-15 leak
rewrote them, `$XDG_CONFIG_HOME/git/config` (else `~/.config/git/config`) and
`~/.claude/plugins/{installed_plugins,known_marketplaces}.json`.

- Home is `HOME`, else `USERPROFILE`; `~/.claude` honors `CLAUDE_CONFIG_DIR`; `.netrc`
  also checks `_netrc` on Windows.
- `.git/config`: `git -C <root> rev-parse --path-format=absolute --git-common-dir`
  (stdin null; the bare form prints a relative `.git`), so a linked worktree watches the
  shared config. No git or no repo: recorded `unreadable`.
- **`~/.config` as a whole is not watched** (caches rewrite). Opt-in `extra_paths` in
  relais `machine.toml`; suggested: `~/.config/{greenpiste,greenbelay,greengate,relais}`.
- Snapshot: sha256 of content + size; mode bits on unix only; symlinks by target, never
  followed; only regular files are opened (`symlink_metadata` first), FIFOs, sockets and
  devices are skipped (open on a FIFO blocks; ssh ControlMaster sockets come and go);
  an absent path is recorded as absent. A directory expands to at most 500 files in
  byte-sorted order, over that a truncated flag. Hashes only, never contents.
- Diff output: `{path, change: added|removed|modified|unreadable|truncated}`, no hashes.
  The last two print `(not checked: …)`. Paths under `HOME` print `~/…` on unix; all
  others, and every Windows path, print absolute so they can be pasted.

## Phases

### Phase 1: relais (one PR, worktree from origin/main) — done 2026-11-20

1. **New `crates/relais/src/homeprint.rs`**, pure, taking `home`, an env closure and the
   repo root (like `paths.rs` `resolve_home`, so tests never call `set_var`):
   `snapshot()` and `diff()`. Reuse `ids::sha256_hex` (`ids.rs:33`). Read errors go
   through a reader seam and record `unreadable`.
2. **Config:** `[fingerprint]` in `machine.toml` (`enabled` default true, `extra_paths`);
   not `relais.toml`, so the authority-hash pin test (`policy/mod.rs:4004`) is untouched,
   which the phase confirms by running it. `MachineSettings` is `deny_unknown_fields`
   (`policy/mod.rs:1073`): write the section only once every installed relais is at or
   above this release; the CHANGELOG says so.
3. **Hook points** in `crates/relais/src/runner/engine.rs`: snapshot at `run_inner` entry
   (`:665`) and after it in `run_to_outcome` (`:524`) on every Ok and Err exit. That covers
   the baseline (where the repo's tests run), setup failures, `verify_applied`,
   `attempt_loop` and decomposed runs. Best-effort: a snapshot error is logged, the run
   continues.
4. **Recording, no outcome change:** one `record_artifact` (`runner/evidence.rs:132`) per
   drift, new `EvidenceKind::HomeDrift`; `evidence.kind` is TEXT, no migration (an older
   relais reads an unknown kind as Corrupt, `ledger/evidence.rs:344`: accepted downgrade
   cost). No new `Reason`, no `NeedsDecision`.
5. **Where it shows:** stderr at the end of `relais run`; `relais status <run>` after the
   state line (`cli/status.rs:131`); `relais explain <run>`; the list form adds a
   `(home changed)` mark. One form, one line per path:
   `relais run: warning: changed while run <id> was active: ~/.gitconfig (modified)`, then
   `check these files; if the run caused it, see SPEC.md §sandbox`. It says "while the run
   was active" because a concurrent edit by the person shows too.
6. **Docs:** SPEC.md sandbox paragraph (~:370), CHANGELOG.
7. **Tests** (`runner/tests/`, `Fixture` + `MockBackend`): no change no warning;
   modified, added, removed each warn; the run still reaches its normal terminal state;
   a baseline-only write and a Superseded run warn; read error injected through the seam
   (not chmod, which lies as root and on Windows); extra_paths dir; sort-stable
   truncation (two snapshots of 600 unchanged files differ by nothing); a FIFO in the dir
   does not hang; symlink not followed (`cfg(unix)`); `USERPROFILE`-only path logic on
   every OS; `enabled=false`.
8. **Verification:** `cargo test -p relais` passes; a mock worker writing a fake-`HOME`
   file gives one line in `relais status <run>`; with no write, none.

### Phase 2: greenbelay (separate PR; starts once Phase 1 is merged, no release wait)

1. **Assertion `home-fingerprint-drift`** in `crates/greenbelay-guard/src/assertions/`,
   listed in `ASSERTIONS` and `AFTER_FAILURE` (`assertions/mod.rs:135`, `:143`),
   `default_stance = Advise`. An assertion cannot refuse (`mod.rs:38-44`), so "no deny"
   holds by construction. Not a `rules/` rule: those read the command before it runs.
2. **Trigger:** test/build commands via `test_output_trimmed::runner_words`/`is_check_run`
   and `heavy_build_unqueued::is_heavy`, plus any command running a binary from
   `target/debug/` or `target/release/` (the 11-11 hand run); `--help/--version/--list/
   --no-run` stay exempt.
3. **Pairing:** pre snapshot written beside `push_record::write_before`
   (`greenbelay-process/src/preview.rs:2038`), keyed by `tool_use_id`, under the journal
   dir `previews/home/<id>`, private. Reject ids holding `/`, `\`, `:` or `..`. Pre prunes
   records older than 24 h (an interrupted command gets no Post; pattern at
   `push_record.rs:118`). Post and `PostBashFailure` compare.
4. **Snapshot code** is greenbelay's own (`sha256.rs`, a short `std::fs` walk); no code is
   shared with relais, so each tool works alone. Both document the same watched set.
5. **Output:** a `Finding` with `reason` (changed paths one per line, the command, and the
   note that a simultaneous edit by the person also shows) and `remedy` (inspect the
   files; if the command caused it, rerun its tests with an isolated `HOME`).
6. **Budget:** ≤ 20 ms p95 over 100 hook runs with the real `~/.ssh`; else prefilter by
   size+mtime and confirm by hash.
7. **Checklist:** inline tests, `corpus` cases with expected negatives, `docs/rules.md`
   row, CHANGELOG `## Unreleased`, a `tests/` integration test; CI already has windows.
8. **Verification:** `cargo test --workspace` and the corpus check pass; in a scratch repo,
   a `cargo test` whose test runs `git config --local x.y 1` yields advice naming
   `.git/config (modified)`; the same test without the write yields none; a replay of the
   11-15 rewrite of `~/.config/git/config` under a temp `HOME` set for the hook warns once.

## Phase 1 record (2026-11-20)

Behaviour as built (supersedes items 4-5 above, per the review fix list in the
sidecar): drift is recorded with `record_immutable_artifact` (`EvidenceKind::HomeDrift`);
`relais run` prints to stderr a heading `relais run: warning: home files changed while
<run> was active:`, one indented `<path> (<change>)` line per file and a two-line
follow-up; `status <run>` and `explain <run>` print the same block to stdout under
`home files changed while the run was active:`; a path unreadable on both sides prints
`not watched (unreadable)`, a directory over the cap on both sides `not fully checked`.
No `(home changed)` mark in the list form (deliberate). `drifts_of_run` lives in
`runner/home_drift.rs` (the module-cycle gate refused it in `homeprint`).

Verification (input → expected → actual):
- `cargo test --workspace` on the rebased tree → 0 failed → 2530+56+349+16+16+1 passed, 0 failed.
- `cargo clippy --all-targets -- -D warnings` → exit 0 → exit 0; commit gate 32 checks passed.
- mock worker writes a watched file under a scratch HOME → one HomeDrift row, run accepted → as expected (`a_worker_writing_a_watched_home_file_…`).
- baseline check writes `.gitconfig`, candidate red → one drift row → as expected.
- run reaped mid-flight → one drift row → as expected.
- status report → heading, path line, two follow-up lines, all ≤ 80 columns → as expected.
- `~/` in `extra_paths` expanded; a relative entry refused at load naming the key → as expected.
- authority-hash pin test → unchanged, passes → passes.
- CI windows-latest on the first push → pass → 5 path tests FAILED (`/x` not absolute on Windows, `~/` expected where Windows prints absolute, 80-column check on a path line, `/` in a suffix check); fixed in `fix: home fingerprint paths on Windows`, `~\` accepted as a tilde prefix on Windows; macOS home tests 29 passed, clippy exit 0; Windows proof is the next CI run.
- Not done: a piloted `relais status` through the installed binary; the host was saturated (load 15–100 on 6 cores) and the fixture tests drive the real engine. First real run after release is the live check.

## Implementation review
- implementation-review: approve-with-changes after 3 rounds; 14 fixed; `_` arm over `Result` in `expand_extra_paths` deliberate: not a domain enum (its own round-2 ruling). 154k tok.
- Windows fix (delta): implementation-review, tui, architect approve; `is_valid_extra_path` doc to name `~\` carried (low).
- backend approve; lang:rust approve; unix approve (run-end problem lines unprefixed: carried, low).
- tui approve-with-changes: no-home line ~130 cols deliberate: shown only when HOME is unset.
- architect approve-with-changes: run-end heading literal deliberate: one place, a different string by design; HomeWatch injection, absolute stored path, streaming hash carried to follow-ups.

## Process

Each phase: worktree from `origin/main` (`worktree-task`), plan committed first,
`amont agents-md` staged, one PR, merge on green. Separate work, not here: isolating
`HOME`/`XDG_*` in the involved test suites; this plan is the detection net.

## Non-goals

- No blocking, no `needs_decision`, no deny stance, no graduation.
- No sandbox, no prevention, no restoring changed files.
- No code shared between relais and greenbelay.

## Measure after release

7 days after each phase installs: sort warnings into tool-caused and user-edit. Roll
back (`enabled=false` default; the assertion removed) if over 1 in 3 are the person's own
edits, or the hook's p95 exceeds 50 ms.

## Code review
- relais: backend, lang:rust, unix, tui, architect
- greenbelay: backend, lang:rust, unix, tui

## Open points for the person

- `~/.config`: opt-in via `extra_paths`; the four green tool dirs are suggested.
- Add `.git/hooks` to the default set? The user listed `.git/config` only.
<!-- panel: repos=relais,greenbelay adds=contract reviewers=backend,lang:rust,unix,tui,architect,po body-sha=e5368d400ea9 -->
