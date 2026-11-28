+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["amont"]
+++
# CI and the machine write HEADS.md with the same greentopo

## Question

Filed from the amont inbox (2026-11-27, this session, relais-modularise t05). amont's CI pins greentopo 2.0.0 (`ci.yaml` `GREENTOPO_VERSION`), while the Mac runs 3.0.0. A worktree-task pack refresh with `greentopo heads --write` writes one code span of the `work.programme-map` row with single backticks where 2.0.0 writes double, so heads-fresh fails on CI (greenharness/greengate#538). relais's `HEADS.md` had the same split. Should the CI pin move to 3.0.0, or should the refresh run the pinned version?

## Look at

- amont `.github/workflows/ci.yaml` (`Ensure greentopo`, `GREENTOPO_VERSION`) and relais's workflows.
- The worktree-task skill's fleet-pack refresh step.

## Answer looks like

One greentopo version for both CI and the refresh step, and a pack refresh on a fresh worktree that passes CI's heads-fresh first time.
