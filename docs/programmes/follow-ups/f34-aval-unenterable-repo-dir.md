+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
+++
# Name the directory aval cannot enter

## Question

f32 checks `repo_dir.is_dir()` before aval's spawn, so a missing
`repo_dir` or a regular file gets a detail naming it. A `repo_dir` that
exists but cannot itself be searched (mode 000 or 0400) still passes
`is_dir()`. The spawn's `current_dir` then fails with `PermissionDenied`,
which is not the `NotFound` the `RunError::Spawn` arm matches, so it falls
through to the catch-all `Err(e)` arm, and the detail ends with the f29
launch hint, which points at PATH and `bin`. A parent that cannot be
searched makes `stat` fail, so `is_dir()` is false and the guard catches
it, but its detail then says "not a directory or does not exist", which is
wrong too. The comment above the check also says "unreadable", which the
check does not catch. Which detail should an unenterable `repo_dir` get?

## Look at

- `crates/relais/src/context/mod.rs`: `aval_resolve_in`, the `is_dir`
  guard added by f32, its `RunError::Spawn` arm and `aval_launch_hint`.
- The tests `a_missing_repo_dir_is_a_tool_failure_not_absence` and
  `a_file_repo_dir_is_a_tool_failure_naming_it`.

## Answer looks like

An unenterable `repo_dir`, on its own or through a parent, is a
`ToolFailure` whose detail names the directory and the permission, with no
launch hint. It is checked before the spawn with a call that needs search
permission on the directory itself (such as
`metadata(repo_dir.join("."))`; `read_dir` passes on 0400). The spawn
error alone cannot do it: a failed `chdir` and a declared `bin` that is
not executable both give `PermissionDenied`. The guard's detail changes
for the parent case, and its comment says only what it checks. Unix tests
pin the detail for a mode-000 and a mode-0400 directory and for an
unsearchable parent (skipped when run as root), and it ships in one relais
release.
