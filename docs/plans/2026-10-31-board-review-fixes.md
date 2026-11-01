---
canonical: decisions:docs/plans/2026-10-31-board-review-fixes.md
phases: [3]
status: active
branch: fix/programme-review
---
# Board review fixes

Pointer. The plan and its review panel live in decisions. This repository
carries phase 3 (B1–B3): `relais programme list` names each map once, an
in-flight ticket shows its branch's last-commit age, and the `programme`
skill says where the branch lives and how to give a ticket up; released as
0.44.0.

## Verification

- The candidate came from a relais run whose worker finished, then waited the whole 3 h admission budget for verification on a saturated host. It was applied here and passed the commit gate (31 checks, `cargo test`, clippy, the plugin tests), recorded in relais as salvaged (`2c634f33`).
- Tests (`programme_cli.rs`): `list_names_a_map_once_though_a_linked_worktree_holds_it` (alpha plus `alpha-wt-x` → one line); `an_in_flight_ticket_shows_its_branch_last_commit_age` (a tip dated 3 h back → `in-flight prog/<slug>/D 3h` in `frontier --all` and `show`, word for word; `branch_age_secs` in JSON; a future date → `0m` and 0) → pass in the gate.
- Live, branch build, toy root (`lake` with its linked worktree `lake-wt-x` still present): `relais programme list --root <toy>` → expected one line → actual one line (`object-store`, `open 3 frontier 1 closed 1`). `frontier --all` → `t04-quota … in-flight prog/object-store/t04-quota 15h`.
- The skill's Work branch now says: the `prog/` branch lives in the map's repository; giving a ticket up deletes it, local and on origin; a branch that has not moved for days is worth asking about. The `description:` is unchanged.

## Implementation review
