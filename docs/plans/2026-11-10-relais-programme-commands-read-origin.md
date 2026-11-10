---
status: active
branch: feat/programme-sources
repos: [relais]
adrs: []
---
# relais programme commands read origin, local work, and a fresh fetch

## Review panel

👉 **Decide:** none — approve if one "local work wins, else origin" rule for all four commands, plus a fetch by default, is the shape you want.
📍 relais · plan reviewed, nothing built (PR #430 merged, unreleased) · next: worktree + Phase 1. Panel: backend, lang:rust, tui, unix (opus); ⚠ codex: usage limit until 11-11.
**Changed by review:** squash-merged branches are no longer counted as local work (tree-history test); fetch stays in relais's process group with a SIGTERM backstop; `check` keeps GNU `file:line:` and tags `(<ref>)`.
📄 Full reviews: [2026-11-10-relais-programme-commands-read-origin.reviews.md](2026-11-10-relais-programme-commands-read-origin.reviews.md)
**Verdicts:** round 1: 4 approve-with-changes (18 findings, all applied); round 2: 4 approve-with-changes, every round-1 finding resolved; 10 medium/low carried into implementation.

## Picture

```
 today                               after this plan
 -----                               ---------------
 list ----> origin/main (stale)      fetch --prune origin
 frontier/show/check --> files          |
                                        v
                                     each checkout: local work on slug?
                                       | yes                  | no
                                       v                      v
                                     newest local copy      origin/main
                                     (primary or worktree)
                                       \______ same rule ______/
                                        list frontier show check
```

A programme query used to read origin in `list` but files elsewhere, from
a stale fetch; after this plan all four commands fetch, then read the
newest local copy with work on it, else origin.

## Context

PR #430 (merged, unreleased) made `relais programme list` read each map from
origin's default branch. It left three gaps, which the person asked to close:

1. `frontier`, `show` and `check` still read the working tree. `list` can
   print a path for an origin-only programme that `frontier <dir>` then
   cannot read, and on a stale checkout `list`'s counts disagree with
   `frontier`'s.
2. Unpushed programmes disappear. A programme that exists only in the
   primary checkout, or one being charted on a PR branch in a linked
   worktree, is not listed for a repository that has an origin.
3. Nothing fetches, so "origin" means the last fetch, which can be days old.

The person decided (2026-11-10):

- **Fetch by default:** `git fetch --prune origin` in each repository, in
  parallel, about 10 s each; on failure, one stderr warning and a read of the
  last fetch; `--no-fetch` skips it.
- **Unpushed programmes:** origin, the primary checkout and each linked
  worktree are all candidates. A local copy counts only when it has local
  work on that programme. Each slug is listed once: local work wins over
  origin, and the most recently changed copy wins among worktrees.

Repo: `relais` (single repository).

## The resolution rule (one function, used by all four commands)

For a checkout `C` (the primary checkout or a linked worktree) and a slug
`S`, let `R` be origin's default ref (`origin_default`, from #430: an
`origin/HEAD` that resolves, else `origin/main`).

- **`C` has local work on `S`** when either holds in `C`:
  - **uncommitted:** `git status --porcelain -z -uall --
    docs/programmes/S` lists an entry (`-uall` names each file of an
    untracked programme; `-z` keeps odd names unquoted);
  - **committed, not yet on origin:** HEAD's tree for the directory
    (`git rev-parse HEAD:docs/programmes/S`) is not a tree that path ever
    had on `R`. The candidates are the trees of `git rev-list R --
    docs/programmes/S`, read in one `git cat-file --batch-check` pass over
    `<commit>:docs/programmes/S`. A branch already squash- or
    rebase-merged into `R` has commits outside `R` but a tree `R` held, so
    it is not local work even after origin closes more tickets. A merge
    whose upstream also changed the directory concurrently yields a tree
    `R` never held, and still counts: that residue is documented.

  With no `R`, every checkout that has the files counts (this is today's
  behaviour).
- **Its recency** is the newest of the mtime of each file that `git status
  -z -uall` lists under the directory (deleted entries skipped) and the
  committer date of the newest `R..HEAD` commit touching it. The mtime is
  a heuristic, since a checkout or rebase resets it; SPEC §31 says so.
- **Source for a given `<dir>`** (`frontier`/`show`/`check`): `C` is the
  checkout that holds `<dir>`.
  - `C` has local work → read `C`'s files.
  - Otherwise, `R` holds `S` → read `R`.
  - Otherwise, the files exist → read `C`'s files.
  - Otherwise → error, as today.
- **Source for `list`:** per repository, the candidates are every checkout
  (primary + `git worktree list --porcelain`, skipping missing and
  prunable entries) that has local work on `S`.
  - Pick the most recent candidate (ties go to the primary checkout, then
    to worktree path order).
  - If there is no candidate, read `R`, printed under the primary
    checkout's path.
  - Whatever is chosen, the printed path is that checkout's
    `docs/programmes/S/map.md`. `frontier`/`show`/`check` on it resolve to
    the same source, so the commands agree.
- **A `<dir>` that does not exist locally** (an origin-only programme):
  derive the repository from the path's shape
  (`<repo>/docs/programmes/<slug>`) after `std::path::absolute`, without
  `canonicalize`, provided `<repo>` is a git checkout.

`check` prints each problem and each warning as `<dir>/<file>:<line>:
<message>` when it reads files. When it reads `R`, it prints `<dir>/<file>:<line>:
(<R>) <message>`. The `file:line:` prefix stays parseable (GNU Coding
Standards §4.4), and the ref in the message says the line number is `R`'s.
`--json` gains a top-level `"source"` field (the ref or `working-tree`), and
its `path` fields keep the same `<dir>/<file>` form.

## Fetch

- **What runs:** `git fetch --prune -q origin`, once per repository (its
  common git dir; a `<dir>` in a linked worktree fetches its repository).
  It runs only in a repository that has an `origin` remote, with:
  - stdin null and `GIT_TERMINAL_PROMPT=0`;
  - `GIT_SSH_COMMAND="ssh -o BatchMode=yes -o ConnectTimeout=5"`;
  - `GIT_HTTP_LOW_SPEED_LIMIT=1000` and `GIT_HTTP_LOW_SPEED_TIME=5`.

  This extends the pattern `holdout.rs:877` uses, so git gives up on its
  own and removes its own lock files.
- **Process group and kill:** the fetch does not go through
  `procs::run_with_timeout`. That puts the child in its own process group
  (`procs.rs:1370`), where a terminal Ctrl-C never reaches it, and it
  SIGKILLs it (`procs.rs:985`), which can leave a `*.lock` behind.
  - It is spawned in relais's own process group, so a Ctrl-C reaches git
    and ssh, and git's signal handler removes its locks.
  - A 10 s backstop sends git SIGTERM (the `procs.rs:128` helper), then
    SIGKILL 2 s later.
  - A `*.lock` left under the common git dir is named in the warning.
- **`list`:** up to 8 repositories at once (scoped threads). When stderr is
  a TTY, it first prints `relais: fetching N repositories (--no-fetch
  skips)`; when stderr is not a TTY, it prints nothing.
- **`frontier`/`show`/`check`:** fetch the one repository.
- **On failure** (timeout, auth, offline, a ref lock held by another
  fetch): one line, `relais: <repo>: fetch failed (<reason>); reading the
  last fetch`, on stderr, and the exit code is unchanged. `<reason>` is
  `timed out after 10 s` or the first non-empty line of git's stderr.
  After any failure, one closing line: `relais: run with --no-fetch to
  skip fetching`. The fetch lines come before any `check` problem line and
  start with `relais:`.
- **`--no-fetch`** on all four subcommands skips it.
- **Docs:** the module doc and SPEC §31 stop saying the commands only read.
  They now say the commands write remote-tracking refs through fetch and
  still open no ledger, coordinator or machine config.

## Phases

### Phase 1: one rule, fetch, four commands (one PR)

Files: `crates/relais/src/programme/load.rs`,
`crates/relais/src/cli/programme.rs`, `crates/relais/src/cli/mod.rs`,
`crates/relais/tests/it/programme_cli.rs`, `docs/SPEC.md`, `CHANGELOG.md`,
`README.md`, `claude-plugin/skills/programme/SKILL.md`.

- `crates/relais/src/programme/load.rs`
  - Add `fetch_origin(repo) -> Result<(), String>`, `checkouts(repo)` (the
    worktree list), `local_work(checkout, slug, rev) -> Option<i64>`
    (recency, or `None`), and `resolve(dir) -> Found` (the `<dir>` rule).
  - Rework `find_programmes(root)` into the `list` rule. `Found` gains the
    chosen checkout. `Source` stays `WorkingTree` | `Ref(String)`.
  - `Programme::load` stays as the files reader; `load_at` (from #430) is
    the ref reader. Reuse `git_stdout`, `main_checkout`, `origin_default`
    and `programmes_at`.
- `crates/relais/src/cli/programme.rs`
  - `frontier_command`/`show_command`/`check_command` take `fetch: bool`
    and call `resolve(dir)` then `Found::load()`, instead of
    `Programme::load(dir)`.
  - `check` formats problem and warning lines by source, as above, on
    stderr and in `--json`.
  - `list_command` takes `fetch: bool`, fetches in parallel, then calls
    `find_programmes`.
  - Text `list` appends the source (`working-tree` or the ref) as a
    fourth tab column. Columns 1–3 are unchanged, and the source explains
    a path that is absent on disk.
- `crates/relais/src/cli/mod.rs`
  - Add `#[arg(long)] no_fetch: bool` to the four `ProgrammeCommand`
    variants and pass `!no_fetch` through.
- `crates/relais/tests/it/programme_cli.rs`
  - New tests (below).
  - Existing tests keep passing unchanged: their worlds have no `origin`,
    so there is no fetch and the files are read.
- `docs/SPEC.md` §31, `CHANGELOG.md` (Unreleased, the same bullet as #430,
  widened), `README.md:276` (the list line), and `claude-plugin/skills/programme/SKILL.md`.
  - The skill's step 0 says the printed path may belong to a worktree or
    to an origin-only programme, and that `frontier` on it reads the same
    source.
  - After editing SKILL.md, run `cargo test -p relais --lib install::`,
    which pins the skill text.

## Verification

Integration tests, each with a bare `upstream.git` as `origin`, as in
`list_reads_maps_from_the_origin_default_branch`:

| Test | Input | Expected |
| --- | --- | --- |
| fetch by default | upstream closes D after alpha's last fetch | `list` shows D closed; `list --no-fetch` shows it open |
| fetch failure | `origin` URL set to a missing path | exit 0, stderr names the fetch failure, rows read from the last fetch |
| frontier/show from origin | clean stale checkout, upstream closed D | `frontier <alpha dir>` omits D; `show` counts closed 2 |
| origin-only dir | `later` only upstream | `frontier <alpha>/docs/programmes/later` exits 0 |
| primary checkout local work | uncommitted edit to `B.md` in alpha | `list` and `frontier` read the files (`source: working-tree`) |
| unpushed programme in a worktree | `alpha-wt-x` commits `docs/programmes/new` on branch `side` | `list` prints `alpha-wt-x/docs/programmes/new/map.md` once |
| two worktrees, same slug | both have local work, dates set via `GIT_COMMITTER_DATE` and explicit mtimes; plus an equal-date case | `list` prints the newer one only; on a tie, the primary |
| merged-branch worktree | worktree on a branch squash-merged into origin, then upstream closes D | `list` and `frontier` read origin; D closed |
| check reads origin | clean stale checkout, upstream adds a bad ticket | stderr `<dir>/X.md:<n>: (origin/<b>) …`, `--json` `source` is the ref, exit 2 |
| check reads edits | dirty bad ticket locally | stderr `<dir>/X.md:<n>: …`, exit 2 |
| check with fetch failure | missing `origin` path, bad ticket | stderr line 1 starts `relais:`, then the `<path>:<n>:` lines, exit 2 |
| Ctrl-C during fetch | origin with `remote.origin.uploadpack='sleep 60; git-upload-pack'` (relais sets `GIT_SSH_COMMAND` itself, so the test hangs the transport this way), relais started in a new process group, SIGINT to that group after 1 s | no `git fetch` left (`pgrep`), no `*.lock` under the git dir |

- **Checks:** `amont-agent quiet -- cargo test -p relais --test it
  programme_cli`, `cargo test -p relais --lib install::`, and clippy
  `-D warnings`. The full suite runs at pre-commit.
- **Live, branch build, from `~/Developer/Perso`:**
  - `list` gives 8 rows and fetches each repository once; time it, with
    an expected time under 15 s.
  - `list --no-fetch` gives the same rows.
  - `frontier` on the `homelab-offload` path matches the frontier count
    `list` printed.
  - A charted-but-unpushed programme in a scratch `git worktree add
    --detach` of a toy repository shows up.

## Non-goals

- `list` does not scan programmes on remote branches that are not checked
  out (a pushed PR branch with no local worktree).
- There is no merge of a local copy with origin: when a checkout has local
  work, its copy is read whole, even if origin closed tickets since.
- `origin/HEAD` is not refreshed (`git remote set-head`) during the fetch.
- No release in this plan.

## Code review

- relais: backend, lang:rust, unix, tui

<!-- panel: repos=relais adds= reviewers=backend,lang:rust,tui,unix body-sha=7ec1aa7ec5a4 -->
