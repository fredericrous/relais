# Reviews: relais, finish resource admission, then split three large modules

**Round 1, backend** (approve-with-changes, 22k, 41 s). All accepted:
- high: a ledger migrated by the branch would strand after renumbering (answered by measuring the real ledger: main's v37/v38 only);
- high: move-only needs an observable check;
- medium: compare test names, not counts;
- medium: a cost, time and retry budget;
- medium: a freeze and conflict policy during the split PRs;
- low: `pub(in crate::runner)` for nested items `scheduler.rs` imports.

**Final review, codex** (approve-with-changes, high effort):
- high, applied: `nothing_a_model_can_reach_names_a_trials_arm` (`ledger/mod.rs:9976`, `:10018`) reads `main.rs` and `mod.rs` by relative path, so the split would weaken it; a test-only preparatory commit makes it layout-independent first.
- medium, applied: compare the library's effective API (paths, signatures) rather than banning added `pub` lines, since a moved `pub` type is an added line.
- medium, applied: flat clap variants (`Doctor`, `Init`) stay flat; no new enums.
- medium, applied: check each new child module's dependencies before extraction, because `scripts/check-module-cycles.py` collapses children.

**Round 2, backend** (approve-with-changes, 30k, 33 s). Round-1 findings 1–6 resolved. New, all moved into the body (see Binding pass):
- medium: the Design line "each with its own clap subcommand enum" contradicts codex's carried finding; existing variants keep their shape.
- medium: the preparatory commit needs a Changes step 0 (its own PR) and a Verification entry: passes on both layouts, fails on each negative control.
- medium: the line-multiset script must trim whitespace and normalise `super::`/`crate::` qualifiers, or the dedented `mod tests` move fails it.
- low: list each new child's `use` dependencies against the module-cycle rule (the same as codex's third medium).

**Binding pass, backend** (approve-with-changes, 22k, 22 s). No new blocker. Three body lines contradicted the carried findings, so all seven moved into the body: flat clap variants stay flat; the effective-API comparison replaces the added-`pub` ban; the line-multiset script trims whitespace and normalises qualifiers; a Phase 4 step 0 with its Verification entry; a per-PR check of each child's dependencies against the module-cycle rule.

**Final bind, backend** (approve-with-changes, 49k, 93 s). All five moved findings resolved. Five new mediums, carried into implementation (listed in the Review panel), and the review labels fixed; the body is unchanged since this pass.
