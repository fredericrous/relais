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
 drift --> "changed" line   drift --> writer part only? --yes--> count line
                                        | no
                                        v
                             worker call names it? --yes--> "by Write"
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

- [ ] Phase 1 — writer parts and `Cause` in
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

- Replay test (`runner/tests/home_drift.rs`): a scratch home holding the
  49 rows' path mix. Before and after the run, `known_marketplaces.json`
  and `installed_plugins.json` differ only in `lastUpdated` and `version`,
  `.git/config` gains a `[branch "t"]` section, and settings.json changes
  in `enabledPlugins`. `.gitconfig` is also changed, and no call touches
  it. Expected: the two plugin files and `.git/config` get `Writer` causes
  with their writer names; settings.json and `.gitconfig` are
  `Unexplained { calls_read: true }`.
- The same test with a new marketplace entry added to
  `known_marketplaces.json`: expected `Unexplained`, since the writer
  never explains a new source.
- A worker whose transcript holds `Write` with `file_path` `<home>/.netrc`:
  expected `Call { tool: "Write", agent }`, printed on its own line and
  not counted in the writer line.
- A worker whose transcript holds `Bash` `git config branch.t.pushRemote x`,
  with `.git/config` changed only in `[branch "t"]`: expected
  `Call { tool: "Bash", agent }`, not `Writer { git branch tracking }`.
- An old row (`{"path":…,"change":"modified"}`) still reads, and prints
  as it does today.
- A worker whose transcript holds only `Bash` `git branch --show-current`, with `.git/config` changed in `[branch "t"]`: expected `Writer { git branch tracking }`. Re-scoring the 24 existing transcripts with the Bash rule gives 0 `Call` causes.
- `cargo test -p relais --lib homeprint:: runner::tests::home_drift` and the `cli::ctx` warning tests (`--bin relais`) pass, including a new bin test that `status` lists unexplained, then worker-named, then the count line.
- Piloted: a branch build of `relais run` on a scratch task in a scratch
  clone of this repo, with a `git worktree add -b` in that clone during the
  run (branch and worktree removed afterwards). Expected stderr: a count
  line naming `git branch tracking`, no unexplained or worker-named line
  and no follow-up lines (other sessions' plugin refreshes may add to the
  count). `jq -r .cause.kind` over its `home-drift-*.json` prints only
  `writer`.
- `relais status <piloted run>` and `relais explain <piloted run>`:
  expected the same order as the warning (unexplained, worker-named, then
  the count line); on a run recorded by 0.88.0, expected the lines
  unchanged from today.

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

<!-- panel: repos=relais adds= reviewers=backend,lang:rust,tui,unix,codex body-sha=a20afd906fa8 -->
