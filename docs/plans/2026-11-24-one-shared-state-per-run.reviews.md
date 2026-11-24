# Full reviews: One SharedState per relais run

Panel on `docs/plans/2026-11-24-one-shared-state-per-run.md`, all seats on opus.

## Round 1 (body e2cd624c4e19)

- **backend** approve-with-changes (49k, 71 s). 1 medium: the greps cannot fail (`\|` under `grep -E` is a literal pipe) — accepted, merged. 2 low: a factory error's label would move from "the machine settings" to "shared state" — rejected: `load_machine()` already reports a `shared_state()` failure under its own "shared state" label (the `?` at ctx.rs:63 precedes the `map_err`); the seat agreed in round 2. 3 low: the pilot has no baseline — accepted, merged.
- **lang:rust** approve-with-changes (43k, 57 s). 1 high: the same grep — accepted. 2 medium: count the whole file, the tests build none — accepted. 3 low: the release tests become hermetic, say so — accepted. 4 low: map the label once via `machine_pricing_of` — accepted into Behaviour.
- **tui** approve-with-changes (26k, 29 s). 1 high: the grep — accepted. 2 medium: the count needs a runnable command — accepted (whole-file `grep -cF`). 3 medium: diff the pilot's stderr between binaries — accepted.
- **unix** approve-with-changes (29k, 42 s). 1 medium: pin where `shared` is built, add a no-policy pilot — accepted. 2 medium: the grep — accepted. 3 low: diff stdout and stderr in full — accepted.

## Round 2 (body aadeefd778d6)

- **backend** approve (52k, 19 s): all resolved; the label finding withdrawn.
- **lang:rust** approve (43k, 8 s): all resolved.
- **unix** approve (29k, 7 s): all resolved. Carried into implementation (low): keep `"the machine settings"` as the label of `machine_pricing_of`.
- **tui** approve-with-changes (31k, 16 s): all resolved. Carried into implementation (low): name the pilot's capture files and the no-policy expectation.
