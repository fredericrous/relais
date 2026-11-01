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
0.45.0 (main shipped v0.44.0 from other work while
this branch was open).

## Verification

- The candidate came from a relais run whose worker finished, then waited the whole 3 h admission budget for verification on a saturated host. It was applied here and passed the commit gate (31 checks, `cargo test`, clippy, the plugin tests), recorded in relais as salvaged (`2c634f33`).
- Tests (`programme_cli.rs`): `list_names_a_map_once_though_a_linked_worktree_holds_it` (alpha plus `alpha-wt-x` → one line); `an_in_flight_ticket_shows_its_branch_last_commit_age` (a tip dated 3 h back → `in-flight prog/<slug>/D 3h` in `frontier --all` and `show`, word for word; `branch_age_secs` in JSON; a future date → `0m` and 0) → pass in the gate.
- Live, branch build, toy root (`lake` with its linked worktree `lake-wt-x` still present): `relais programme list --root <toy>` → expected one line → actual one line (`object-store`, `open 3 frontier 1 closed 1`). `frontier --all` → `t04-quota … in-flight prog/object-store/t04-quota 15h`.
- `frontier --all` field counts → expected the same on every line (plan B2) → actual five on each (a ready ticket's fifth field empty), asserted as `[5, 5]`; without `--all`, four.
- `make plugin` run alone: `amont slot run --max-wait 1800 -- make plugin` exited 75, because no host slot freed in 30 minutes on a saturated machine, so it did not run. The commit gate's plugin-test check (part of the 31) passed on `33bb5989`, and CI's `plugin (validate, test)` job runs it again on the pull request.
- The skill's Work branch now says: the `prog/` branch lives in the map's repository; giving a ticket up deletes it, local and on origin; a branch that has not moved for days is worth asking about. The `description:` is unchanged.

## Implementation review

- Round 1 approve-with-changes (54k tokens, 45 s). Fixed: one field count under `--all` (the plan's B2); the clock fallback's reason on its line; the `make plugin` attempt recorded.
- Delta approve (61k tokens, 25 s). B3's `make plugin` alone rests on the gate and on CI's plugin job, read before the merge.
- Rebased onto main after #360–#364 (v0.44.0): round 1 on the new tree approve-with-changes (68k tokens, 33 s). Fixed: the release is 0.45.0 here; decisions' closing PR records it. A push rehearsal on the rebased tree failed one timing test under heavy load (its log went with the rehearsal worktree); the direct `cargo test --workspace` on the same tree passed (rc 0).
