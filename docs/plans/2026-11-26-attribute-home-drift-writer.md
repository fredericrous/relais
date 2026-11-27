---
status: active
branch: fix/f33-home-drift-attribution
repos: [relais]
adrs: []
plan-id: p-01M8CD27X6X2E4CB1HWFPEG8SP
---
# Attribute each home drift to its writer

## Review panel

👉 **Decide:** none — approve if write-target evidence and writer bookkeeping parts turn the 49-row mix into countable causes.
📍 relais · 2 phases · 1 release · next: Phase 1. Panel: backend, lang:rust, tui, unix (opus), codex (gpt-6-astra).
**Changed by review:** worker calls checked before writers; only real writes count (no mentions, no read-only git); writer needs unchanged mode.
**Verdicts:** all approve or approve-with-changes; 15 carried into implementation; words 1457 → 1958 (×1.34, growth from high findings); remove 0, merge 0.
📄 Full reviews: [2026-11-26-attribute-home-drift-writer.reviews.md](2026-11-26-attribute-home-drift-writer.reviews.md)

## Picture

```
 today                          after this plan
 -----                          ---------------
 drift --> "changed" line   drift --> worker writes it? --yes--> "by Write"
                                        | no
                                        v
                             writer part only? --yes--> count line
                                        | no
                                        v
                             "unexplained", listed first
```

A drift used to print the same line whatever wrote it; after this plan it
is checked against the worker's calls first, then known writers, and only the
unexplained ones lead the warning.

## Context

Ticket f33 (relais `docs/programmes/follow-ups`). A `home_drift` row holds
only a path and a change, so every warning at the end of a run looks the
same. Most drifts come from ordinary tool upkeep; the net exists for the 1
to 2% the LLM makes on its own. f08 (due 2026-11-28) counts unexplained
against explained drifts and needs this attribution to do so.

Measured on 2026-11-26 over all 49 `home_drift` rows in the ledger:

| Path | Rows |
| --- | --- |
| `~/.claude/plugins/known_marketplaces.json` | 15 |
| `<repo>/.git/config` (relais 14, greenbelay 3, greenrack 2, happier 1) | 20 |
| `~/.claude/settings.json` | 9 |
| `~/.claude/plugins/installed_plugins.json` | 5 |

- **0 of the 49 are named in any of the run's own worker tool calls.**
  24 rows have a worker transcript; none of its `tool_use` inputs names the
  path. 25 rows come from runs with no worker dispatch at all, or with no
  transcript. So attributing by the worker's tool calls alone would
  explain nothing. These drifts come from other sessions running at the
  same time, and from Claude Code's own upkeep.
- Plugin JSON: each marketplace's `lastUpdated` falls within 15 to 306 s
  of a Claude Code `SessionStart` in the hook journal. 17 of the 20
  plugin-JSON drifts happened while a session was starting. Claude Code
  refreshes marketplaces when a session starts.
- `.git/config`: a scratch repository shows that `git worktree add -b`
  writes a `[branch "x"]` section, `git push -u` writes another, and
  `git branch -D` removes one. `--detach` and `--no-track` write none.
  Other sessions do this all day; this is git keeping track of branches,
  not a run reaching outside its worktree. The greengate trust keys are
  already ignored (#612, v0.89.0, not installed yet: brew has 0.88.0).
- The ticket's third known writer, "settings writes through
  `SharedState`", is wrong. `SharedState::write_settings` writes
  relais's `machine.toml`, which is not watched, and never touches
  `~/.claude/settings.json`. That file's likely writers are Claude Code
  (on a plugin install or enable), `relais install --hooks`, and edits the
  person asked for. A 30-minute watch of the four files on 2026-11-26 saw
  no change at all, so no key is confirmed as Claude Code's.

Decided with the person: only known-writer upkeep collapses into a count.
A drift that the run's own worker named in a tool call keeps its own
line, because a worker writing a home file outside its worktree is the
thing the net is for.

## Goal

Each new drift row says why it happened: by a known writer (and which
one), by the run's own worker (which tool and which dispatch), or
unexplained. The warning at the end of `relais run`, and `status` and
`explain` too, list unexplained drifts first, then the worker-named ones,
then one count line per known writer.

## Non-goals

- Silencing a writer's change. A known-writer drift is still recorded and
  still counted, never dropped from the snapshot the way #612 drops the
  trust keys.
- Attributing drifts to the person's own session, or to other sessions.
  Their transcripts are not the run's, and reading them is out of scope.
- Re-attributing old rows. A row without the new field prints as it does
  today.
- greenbelay's own fingerprint hook. f08 counts that one separately.
- Case on Windows: a tool's `c:/users/…` against an OS-joined
  `C:\Users\…` target does not match, so that write shows unexplained.
- A per-machine allowlist.

## Code review

- relais: backend, lang:rust, unix, architect (tui left out: plain output lines, no new interaction)

## Behaviour

**Writer parts (homeprint).** For two kinds of watched file, the
snapshot keeps a second hash next to the full one: a hash of the file
with that writer's own part taken out.

| File | Writer | Part taken out |
| --- | --- | --- |
| `installed_plugins.json`, `known_marketplaces.json` | `claude code plugin refresh` | per entry (`plugins.<name>[i]` in installed_plugins.json, top-level values in known_marketplaces.json; the top-level `version` stays): `lastUpdated`, `installedAt`, `gitCommitSha`, `version`, `installPath` (every other field stays in, so a new marketplace, a new plugin or a changed `source` is not explained) |
| repo `.git/config` | `git branch tracking` | the `[branch "…"]` sections, taken from the bytes `without_tool_keys` already returns |

`~/.claude/settings.json` gets no writer part: the 30-minute watch saw no
change to it, so no key is known to be Claude Code's own (Decision log).
The plugin files are matched by their exact paths in `targets`, so the
writer is chosen where `snapshot` already special-cases the repo config,
not inside the directory walk.

If no worker call writes the path, both sides are regular hashed files
with the same mode, both reduced hashes exist and are equal, and the
full hash changed, the drift is `writer: <name>`. A file that cannot be
parsed as JSON (or as config sections) gets no reduced hash. An added,
removed, large, linked, unreadable or truncated entry, or a mode change,
is never explained by a writer (test: a `lastUpdated` bump plus a chmod
stays `Unexplained`).

**Worker calls (runner).** For each drift, before any writer is asked
(a worker that writes only a writer's part is still the run's own write), the
engine reads the transcript of every worker dispatch in the run tree
(`Ledger::agent_dispatches_of_tree` and `Backend::transcript_of`, as
`runner/holdouts.rs` does), once per agent. A `tool_use` explains the
drift only when it writes the path, by its absolute or `~/` form:

- an `Edit`, `Write`, `MultiEdit` or `NotebookEdit` call's `file_path` is
  the drifted path;
- a `Bash` command names the path as a write target: after `>` or `>>`,
  as an argument of `tee`, `sed -i`, `rm`, `chmod`, `touch`, or as the
  last argument of `cp`, `mv`, `ln`, `install`.

A mere mention is not a write: `Read`, `Grep`, `cat`, a quoted prompt or
another file's replacement text naming the path stay `Unexplained`
(negative fixtures for each). The walk is shared with holdout exposure;
the write test is separate from its read-exposure test.

For a repo `.git/config`, a `Bash` command that runs
`git … (config <key> <value>|config --unset|config --add|branch <name>|branch -d|branch -D|branch -m|branch -u|branch --set-upstream-to|worktree add -b|push -u|push --set-upstream|checkout -b|switch -c|remote add|remote rm|remote rename|remote set-url)` (read-only forms such as `git branch`, `git branch --show-current`, `git config --get`, `git remote -v` never count)
from inside that repository also counts. "Inside" means the command's
directory (the transcript record's `cwd`, or the target of a leading
`git -C <dir>` or `cd <dir> &&`) has the run repository's git common
dir, so a worker's own worktree counts. The subcommand tokens are matched
by hand, with no `regex` dependency. The match is recorded as
`call: <tool> by <agent>`. It names the agent, not a dispatch: a
continued attempt reuses its agent and `transcript_of` returns the whole
history, so a transcript cannot tell which dispatch made a call. The scan reuses `scan_transcript`'s
`tool_use` walk in `runner/holdout_exposure.rs`, moved into a small
shared helper.

**Record.** `Drift` gains
`#[serde(default, skip_serializing_if = "Option::is_none")] cause: Option<Cause>`.
`Cause` (`#[serde(tag = "kind", rename_all = "snake_case")]`) is one of
`Writer { name }`, `Call { tool, agent }`,
`Unexplained { calls_read: bool }`. `calls_read: false` means the run had
no worker dispatch, or at least one dispatch's transcript could not be
read (a backend with no transcript answers `None`), so "unexplained" is
weaker. Tests move `TranscriptBackend` from `runner/tests/holdouts.rs`
into the shared test module.

**Report** (`homeprint::report`), for example:

```
relais run: warning: home files changed while run-x was active:
  ~/.gitconfig (modified)
    unexplained
  ~/.netrc (modified)
    by Write in agent a30abb7e…
  1 by known writer: claude code plugin refresh
  1 by known writer: git branch tracking
check these files; if you did not change them, the run, its tests or
another session did:
  rerun them with HOME set to a temporary directory
```

The follow-up lines print only when an unexplained or worker-named line
does. When every drift came from a known writer, the warning is the
heading plus the count line. Rows without a cause (written by an older
relais) print as they do today, in the unexplained group, with no
suffix. `Unexplained { calls_read: false }` prints its cause line as
`unexplained (no worker transcript read)`. A test asserts every line
`report` returns is at most 80 columns, using a 17-character agent ID.

## Phases

- [x] Phase 1 — writer parts and `Cause` in
  `crates/relais/src/homeprint.rs`; worker-call attribution in
  `crates/relais/src/runner/home_drift.rs`, with the shared `tool_use`
  walk taken out of `runner/holdout_exposure.rs`; report order and the
  count line; tests in `homeprint.rs` and `runner/tests/home_drift.rs`;
  SPEC §8 text; `CHANGELOG.md` Unreleased.
- [ ] Phase 2 — release through the `tag-release` skill (`CHANGELOG.md`,
  `Cargo.toml`, `Cargo.lock`), so that f08's count (due 2026-11-28) runs
  against attributed rows; then `brew upgrade relais`.

### 🧑 Your steps

None.

## Verification

Observed on 2026-11-26 (input → expected → actual):

- Replay of the 49 rows' path mix,
  `the_49_rows_path_mix_is_told_apart_by_cause`: plugin JSONs differing
  in `lastUpdated`/`version`, `.git/config` with a new `[branch "t"]`,
  settings.json changed in `enabledPlugins`, `.gitconfig` changed by
  nobody → both plugin files and `.git/config` `Writer`, settings.json
  and `.gitconfig` `Unexplained { calls_read: true }` → as expected.
- A new marketplace entry beside a refresh,
  `a_new_marketplace_entry_is_unexplained_even_beside_a_refresh` →
  `Unexplained` → as expected.
- A worker `Write` to `~/.netrc`,
  `a_worker_write_to_a_watched_file_is_named_with_its_tool_and_agent` →
  `Call { tool: "Write", agent }` on its own line → as expected.
- `Bash` `git config branch.t.pushRemote x` against `git branch
  --show-current` alone,
  `a_worker_git_config_write_is_a_call_and_a_read_only_git_form_leaves_the_writer`
  → `Call { tool: "Bash" }`, then `Writer { git branch tracking }` → as
  expected.
- `cat`, `Read` and a `Write` dated before the first snapshot,
  `a_mention_or_an_old_write_is_not_a_worker_write` → `Unexplained` → as
  expected. A relative `git -C sub` or `cd sub` with no record `cwd`
  (transcript_calls tests) → no write → as expected.
- A run with no worker agent,
  `a_run_with_no_worker_agent_says_its_calls_were_not_read` →
  `Unexplained { calls_read: false }` → as expected.
- Report order, `status_lists_unexplained_then_worker_named_then_the_count_line`
  (`--bin relais`) and width,
  `every_report_line_fits_80_columns_with_a_17_character_agent` →
  unexplained, worker-named, count lines; every line ≤ 80 columns → as
  expected. Lib run: 127 passed; bin run: 7 passed; clippy `-D warnings`
  clean.
- Re-scoring the real transcripts: a throwaway ignored test (not
  committed) fed `transcript_calls::writes` the 19 drift rows whose
  worker transcripts still exist (5 of the 24 have since been removed),
  each with its run's repository and start time → 0 `Call` causes → 0
  (19 rows, 0 writes, the implementing run's own drift included).
- An old row (`{"path":…,"change":"modified"}`), `relais status
  run-660c47b928e3f-424` with the branch build against a copy of the
  state directory, next to installed 0.88.0 → the same lines → the same
  lines, except the follow-up's first line, which now names "another
  session" (Decision log).

Phase 2 checks (after the install; a branch build cannot run through the
plugin):

- Pilot: a scratch task in a scratch clone, with `git worktree add -b`
  in that clone during the run → a count line naming `git branch
  tracking`, no unexplained or worker-named line, no follow-up;
  `jq -r .cause.kind` over its `home-drift-*.json` (settings.json left
  out) prints only `writer`.
- `relais status` and `relais explain` on that run → the warning's
  order.

## Decision log

- 2026-11-26 — worker-named drifts keep their own line and only
  known-writer drifts collapse, because the person chose it: a worker
  writing a home file is what the net is for.
- 2026-11-26 — no `SharedState` writer, because it writes `machine.toml`,
  which is not watched; the ticket's premise is corrected here.
- 2026-11-26 — settings.json gets no writer part, because a 30-minute
  watch of it, both plugin files and relais's `.git/config` saw no change,
  so no settings key is known to be Claude Code's own. Its drifts stay
  unexplained unless a worker call names it.
- 2026-11-26 — Phase 1 written by relais (run-6613411a6a63c-17b1e,
  candidate 1). Its two verify runs never got a host slot
  (`admission_wait_exhausted` three times, the line held by other
  sessions' push gates), so the candidate was applied with `git apply`
  and checked here, and the commit gate runs the full suite.
- 2026-11-26 — carried review findings taken in Phase 1: `$HOME/` and
  `${HOME}/` spellings, `>|`, `&>` and `n>` redirections, git 2.46
  `config set/unset/rename-section/remove-section`, calls dated before
  the first snapshot ignored, the full agent ID printed, a quote-aware
  word split with no new dependency, `calls_read` per agent. The worker
  also recurses into `sh -c` (depth 2) and keeps a call whose record has
  no timestamp, since it cannot be shown to be older.
- 2026-11-26 — the follow-up's new wording ("the run, its tests or
  another session") also prints for old rows, because it is one shared
  constant and the old wording blamed the run for what the data shows
  are other sessions' writes. Every other line of an old row prints
  unchanged.
- 2026-11-26 — the pilot runs after Phase 2's install, not before,
  because `relais run` starts only through the plugin, which runs the
  installed binary; a branch build cannot be piloted through it.
- 2026-11-27 — PR #637's `test (windows-latest)` failed 6 new tests on
  path shapes. One was a real gap: a target joined with `\` never
  matched a tool or shell spelling with `/`, so `WriteTarget` now
  compares with `\` unified to `/` on Windows. The rest were fixtures:
  the report-order test now passes no home, the runner fixtures take
  git's own config spelling (`homeprint::repo_config`) instead of
  `canonicalize` (`\\?\`, 8.3 names), and the worker-write line is built
  with `json!`. A first try gated the 6 tests to unix; review refused it
  (`general.no-disabled-safety`), and the gates are gone.

## Implementation review

- implementation-review approve-with-changes (103k, 80 s; Delta 47k, 48 s): 4 blockers and 4 plan-record findings fixed; Delta blocker `Step::writes(bool)` deliberate: overruled by the person, the private constructor only stores the value and picks no path.
- backend approve-with-changes (83k, 103 s; Delta approve 32k, 21 s): input trimming, unreadable-agent line, `holds-until:` fixed; `--git-dir` not followed deliberate: such writes still print as Bash calls.
- lang:rust approve-with-changes (75k, 118 s; Delta 32k, 19 s): relative `-C`/`cd`, `TargetKind`, cache once, extra compare fixed; per-call timestamp parse, writer-name constants, nested `Option` in `Step`, one clone, tool list twice deliberate: low, no behaviour change.
- architect approve (57k, 39 s): cache once fixed; `WorkerCall` beside `Cause::Call` deliberate: the port's return shape. unix approve (55k, 49 s): no findings.
- Windows round (after the red CI): unix gates refused as blocking, replaced by the separator fix; all five approve on tree 52e06cd8 (rust's `Cow` instead of a `String` copy deliberate: low).

<!-- panel: repos=relais adds= reviewers=backend,lang:rust,tui,unix,codex body-sha=a20afd906fa8 -->
