---
canonical: decisions:docs/plans/2026-10-15-gates-keep-unchanged-verdicts.md
phases: [3]
status: done
---
# Gates keep unchanged verdicts (pointer)

The plan lives in the decisions repository, which owns its ADR-0034. This
repository carries phases [3].

## Verification (phase 3)

- Stand-in for "two relais runs on the same base in relais": the runner tests drive the real `RunEngine` on real git fixtures with `cache_baseline = true`.
  - `baseline_results_are_cached_only_when_the_profile_opts_in` (green base): the third run hits (`baseline_cached`).
  - `a_cache_hit_still_runs_task_setup_and_a_failure_launches_nothing`: on the hit, `base-setup0.log` is absent, so the base did not run. A hit copies the base's logs (ADR-0009), so `base-cmd0.log` cannot show it.
  - `a_red_base_is_rerun_every_time_not_cached`: two runs on a red base, neither cached, both name the failure.
  - `a_baseline_verdict_is_keyed_by_the_tree_not_the_commit` and the `verify` unit tests (all-green only, v4 entry unread).
  - Full `cargo test` (1614 tests) and `clippy -D warnings`: green on 7a905d02.
- After the release, still to do: the real relais tasks in amont, and the ledger count of tasks per `main` tree. `cache_baseline = true` stays commented out in relais, amont and amont-agent until `relais --version` shows the release and the coordinator has restarted.

## Implementation review

- approve after a delta. Fixed: the messages and doc comments now say an over-limit entry keeps the green verdict without its logs, and `NotCached` has its own decision, `baseline_not_cached` (only the runner names it). Deliberate: no extra "base did not run" assertion (the setup-log test proves it); two leftover doc comments (`verify/mod.rs:2061`, `:2973`) still say "failures", cosmetic. 54k + 57k tokens, 55 s + 15 s.
