# Reviews: relais, a review that does not fit asks the orchestrator

**Round 1, backend** (approve-with-changes, 36k, 81 s). Accepted: high, the coordinator keeps the prompt and returns it in `ClaimOutcome::Spawn`, and `NativeAsk::Continue` carries it; high, successor intents need a column and a migration; high, review-parts/approve must also answer `review_unavailable` runs (accepted; the 14 historical runs were already answered approve, so the check confirms none waits); medium, windowed `Read` and token arithmetic; medium, at most 8 parts; medium, compatibility direction; medium, a 150 KB test patch; low, `relais report` counts.

**Final review, codex** (approve-with-changes, high effort). Applied: high, an oversized single file defeats the budget (every part must fit; review-parts refused otherwise); high, the approval receipt had no creation path (snapshot before the stop, sealed on approve); high, parts would overwrite `review.txt` (immutable per-part paths and hashes).

**Round 2, backend** (approve-with-changes, 31k, 60 s). Applied: high, the receipt publication order and retry rule; medium, the manifest store and a repeat answer; medium, step 3 runs through the unfixed relais (keep the diff under 60 KB or review it yourself); low, review-parts on a fitting patch is one part.

**Final bind, backend** (approve, 36k, 19 s; a resumed agent the hook does not count). Two lows carried.

**Fresh bind, backend** (approve-with-changes, 28k, 40 s): no blocker; the two carried lows again, plus a version-mismatch verification line, a constraints budget in bytes, and the per-part timeout. All five carried into implementation (listed in the Review panel); the body is unchanged since this pass.
