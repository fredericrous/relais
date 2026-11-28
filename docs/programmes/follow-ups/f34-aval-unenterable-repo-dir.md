+++
type = "task"
mode = "afk"
status = "closed"
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

## Resolution

2026-11-28: `greentopo_resolve_in` (renamed from `aval_resolve_in`) checks
`metadata(repo_dir.join("."))` before the spawn. That call needs search
permission on the directory itself and on every parent. `PermissionDenied`
returns a `ToolFailure` (exit -1) whose detail is "greentopo cannot run in
<dir>: permission denied, the directory or one of its parents cannot be
searched", with no launch hint. A missing path or a non-directory keeps
f32's "not a directory or does not exist". The comment says only what the
check does. `an_unenterable_repo_dir_is_a_permission_failure_naming_it`
pins the detail for mode 000 and 0400, and
`an_unsearchable_parent_gives_the_same_permission_detail` for a parent;
both skip their assertion when the directory stays enterable (run as
root).

The patch came from relais run run-6615674823a3a-134c3, which hit its wall
time. Its verify (run-661588445fccb-184ae) failed on two checks: one
unformatted line in the new test, fixed by `cargo fmt`, and clippy's
`fetch_update` deprecation in `verify/heavy/tests.rs`, a file the patch
does not touch. That one came from this Mac's toolchain: Homebrew's
`cargo-clippy` 0.1.100 comes first on PATH, so even
`rustup run 1.94.1 cargo clippy` ran it, and CI on main (pinned 1.94.1)
is green. Landed by hand after this run with `~/.cargo/bin` first on PATH
(clippy 0.1.94): `cargo clippy -p relais --all-targets -- -D warnings`
clean, `cargo test -p relais --lib -- context::` 102 passed, the two new
tests among them. It ships in the next relais release.
