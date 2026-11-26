+++
type = "task"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais"]
+++
# Name the missing directory when aval cannot start

## Question

`aval_resolve_in` runs aval with `current_dir(repo_dir)`. When `repo_dir`
does not exist, the spawn fails with `NotFound`, `confirmed_absent` is
false (it needs `repo_dir.is_dir()`), and the error lands in the
`RunError::Spawn` arm. Since f29 (v0.87.0) that arm ends with the
program-source hint, such as "install greentopo … check that `aval` on PATH
runs", which sends the operator to PATH when the directory is what is
missing. Which detail should a missing `repo_dir` get?

## Look at

- `crates/relais/src/context/mod.rs`: `aval_resolve_in`, its
  `RunError::Spawn` arm and `aval_launch_hint`.
- Its test `a_missing_repo_dir_is_a_tool_failure_not_absence`, which checks
  only the variant.

## Answer looks like

A missing `repo_dir` is a `ToolFailure` whose detail names the directory
and gives no launch hint, checked before the spawn or by the error's
cause. A test pins the detail, and it ships in one relais release.

## Outcome

2026-11-26: `aval_resolve_in` checks `repo_dir.is_dir()` before the spawn
and returns a `ToolFailure` (exit -1) whose detail is "aval cannot run in
<dir>: it is not a directory or does not exist", with no launch hint.
`a_missing_repo_dir_is_a_tool_failure_not_absence` now pins the detail: it
names the directory and carries none of the hint's text. The patch came
from relais run run-66121efa5b002-10e41. Its verify (run-6612320fd2118-12599)
stopped at a verification gap, because the named test had no JUnit report.
The person approved it with that gap accepted and had it landed outside
relais. Run by hand instead: `cargo test -p relais --lib context::`
(98 passed, the pinned test and a regular-file case among them) and `cargo clippy -p relais
--all-targets -- -D warnings` (clean). It ships in the next relais release.
