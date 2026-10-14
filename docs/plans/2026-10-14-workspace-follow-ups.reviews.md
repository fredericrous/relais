# Reviews: close the workspace-reuse gaps

## Full reviews (reference)

- **backend** round 1 `approve-with-changes` (50k tokens, 149 s): high restore races a running git (take `index.lock`), high non-UTF-8 logs in JSON (blobs), high cache size unbounded (cap and pruning), high the golden cannot compile at `aee42a0` and v0.21.0 sits between (two generation points); medium late-resume restore (report only, D22), retire-scan edge cases, held-source re-check. Round 2 `approve-with-changes` (20k, 14 s): all but the cap resolved; new medium contradiction of D22 in two places, high cap arithmetic, medium golden regeneration rule, low restore event. Final body `approve-with-changes` (26k, 28 s): cap resolved; new medium happier measurement step in the pilot, low cap ceiling.
