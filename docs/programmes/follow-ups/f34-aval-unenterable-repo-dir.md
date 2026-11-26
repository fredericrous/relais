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
`repo_dir` or a regular file gets a detail naming it. A directory that
exists but cannot be entered (mode 000, or no search permission on it or a
parent) still passes `is_dir()`. The spawn's `current_dir` then fails with
`PermissionDenied`, the error lands in the `RunError::Spawn` arm, and the
detail ends with the f29 launch hint, which points at PATH and `bin`. The
comment above the check also says "unreadable", which the check does not
catch. Which detail should an unenterable `repo_dir` get?

## Look at

- `crates/relais/src/context/mod.rs`: `aval_resolve_in`, the `is_dir`
  guard added by f32, its `RunError::Spawn` arm and `aval_launch_hint`.
- The tests `a_missing_repo_dir_is_a_tool_failure_not_absence` and
  `a_file_repo_dir_is_a_tool_failure_naming_it`.

## Answer looks like

An unenterable `repo_dir` is a `ToolFailure` whose detail names the
directory and the permission, with no launch hint: either checked before
the spawn (for example by reading the directory) or by telling a
`current_dir` failure from a missing program. The guard's comment says only
what it checks. A unix test pins the detail (a mode-000 directory, skipped
when run as root), and it ships in one relais release.
