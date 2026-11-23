+++
type = "task"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais"]
+++
# Recorded reverts name the files they revert

## Question

None of the 93 `revert` rows in `router_reassess` matches any task's
files (t02, "cannot see" 3). `revertedPaths` in
`claude-plugin/hooks/router.ts` hashes arguments as written:
- a literal `$P`;
- paths relative to the command's `cd <dir>` or `git -C <dir>`, hashed
  against the session cwd;
- the pathspec `.` and `git revert` with no paths.

Make a revert's files hash the same way as the edits they undo.

## Look at

- `claude-plugin/hooks/router.ts` `revertedPaths`, `revertRecord`,
  `fileHash`, `repoRelative`, and the Bash `tool.call` site that calls
  them.
- t02's Resolution, item 3 (session 943bdf5e as the worked case).

## Answer looks like

Three changes:
- paths resolve against the command's effective directory;
- an argument still holding `$` is skipped, never hashed;
- `.` and `git revert` resolve to the task's touched files and the
  reverted commit's files, as the `holds-until: R2` comment plans.

Plugin tests cover each shape. A live check counts how many new revert
rows overlap their own task's files.

## Resolution

2026-11-23. **Code fixed; the live count is
[t13](t13-revert-overlap-live-count.md).** The plan
`docs/plans/2026-11-23-revert-paths-resolved.md` (panel: backend,
lang:typescript, tui, unix, Codex) was implemented through relais (run
`run-660e6ac42c953-6975`, accepted on 1 attempt). Phase 1 is commit
`fix(plugin): reverts hash the files they revert`.

- **Effective directory.** `revertedPaths(command, cwd, taskFiles)` splits
  the command outside quotes only. It tracks `cd`, restores the directory
  at a subshell's `)`, and composes `git -C`. Each path is made absolute
  and normalised, then hashed with the unchanged `fileHash(path,
  sessionCwd)`, as task rows are. The t02 worked case is a test.
- **Never hashed.** An argument holding `$` or a backtick. A relative path
  under an unknown directory (`cd`, `cd -`, `cd $X`, `cd ~`). A
  `:`-magic pathspec, which leaves its segment unresolved.
  `--git-dir`/`--work-tree`/`GIT_DIR`/`GIT_WORK_TREE`.
- **No row.** A heredoc, a lone `&`. A revert segment not joined to the end
  of the command by `&&` alone (`;`, a newline, `||`, a pipeline): the
  call's success does not show the revert ran.
- **`.` and directories** expand to the task's files beneath them.
- **`git revert <commits>`** reads `git show -z --name-only --no-renames
  --no-relative` off the hook's path (`GIT_TERMINAL_PROMPT=0`, the gate
  timeout), joined on `--show-cdup`. It keeps the call's session, task and
  state. `endTask` and `onSessionEnd` wait for unfinished lookups. `-m 1`
  reads the first-parent diff; another mainline, and a HEAD-relative commit
  after a committing revert, read nothing.

The hooks run from the installed plugin (0.72.0), so no live row comes from
this code until a release and the person's plugin reinstall. t13 counts them
then. Rows from a `.` expansion are reported apart, because they overlap
their own task by construction.
