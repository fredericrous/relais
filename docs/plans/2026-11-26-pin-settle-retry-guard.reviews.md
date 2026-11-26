# Full reviews: pin the settle retry's first-Unknown guard

Programme ticket agent run (no plan-mode transcript for `plan-review record`).

- backend (round 1): approve-with-changes. Fault must not call `inner.settle` and must join two exhaustive arms; Verification needs an actual column; the mutation must compile. All accepted.
- lang:rust: approve. Same `inner.settle` point (low), exhaustive arms, paste the mutation output in the PR body. Accepted.
- tui: approve. Give the exact test command in Verification. Accepted.
- unix: approve, no findings.
- backend (round 2): approve; all three findings resolved, no new finding.
