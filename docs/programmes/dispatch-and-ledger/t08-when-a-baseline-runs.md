+++
type = "grilling"
mode = "hitl"
status = "closed"
blocked_by = ["t01-verdict-consumers", "t03-attempt-ends-on-amont"]
repos = ["relais", "amont"]
+++
# When relais runs a baseline

## Question

Outside the relais repository, the baseline cache almost never hits (t02),
so each run pays a full check before the worker starts. Which rule replaces
it: skip it when greengate proves the base and otherwise run it up front;
run it only after a candidate goes red, to attribute the failure; or run it
alongside the worker under the host slot queue?

## Resolution

The person, 2026-11-10: **"Proof, else lazy"**, chosen over running the
baseline up front or in parallel.

- **When greengate proves the base** (every profile gate stamped or attested
  on the base tree), there is no baseline. This is already built
  (`runner/baseline.rs:64-91`).
- **Otherwise,** no baseline runs before the worker. A base run happens
  only after a candidate goes red, and it runs only the red checks, to tell
  a failure the worker introduced from one already on the base.
- **What it costs:**
  - A red base is no longer caught before a worker is paid. It is caught at
    the first red candidate, and the run ends as it does today
    (`BaselineFailureNotWaived`).
  - "Pre-existing failures are visible from the start" (SPEC §10) no
    longer holds.
- **Build consequences (t01):** the new-failure filter,
  `guarded_preexisting`, `baseline_resource_exhausted`, and the
  identical-to-base reuse need a base run on demand. The baseline cache
  still serves that run.
