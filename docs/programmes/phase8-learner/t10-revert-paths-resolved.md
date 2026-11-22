+++
type = "task"
mode = "afk"
status = "open"
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
