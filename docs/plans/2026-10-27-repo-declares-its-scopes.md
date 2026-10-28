---
canonical: github:fredericrous/aval/docs/plans/2026-10-27-repo-declares-its-scopes.md
phases: [4]
status: active
branch: feat/scoped-decisions
---
# A repository declares the scopes it is in

Pointer. The plan and its review panel live in aval (aval#46, released 1.11.0);
the ADR is decisions ADR-0046 (decisions#102). This repository carries relais's
part of Phase 4: the reviewer's decisions block names the scope that answered,
a cross-scope contradiction keeps its head ids and `by_scope` in the blocked
message, the `holds-until:` on default-scope verdicts goes (its condition is
met: aval answers at the repository's declared `in_scopes`), and SPEC §7 says so.

## Verification record

relais (branch, `cargo test -p relais` through the pre-commit gate, 31 checks):
- aval 1.11.0's cross-scope contradiction JSON (captured: `heads` objects with
  `adr`, `by_scope` rows) → parsed into head ids and `by_scope` → tests
  `a_cross_scope_contradiction_names_each_scope_and_adr` (two ADRs, and one
  ADR at two scopes) and `a_contradiction_without_by_scope_names_the_head_ids`
  pass; the message never reads "0 heads".
- The reviewer's decisions block → `key (record, scope): choice` → test
  `the_decisions_block_names_the_answering_scope` passes.
- The default-scope `holds-until:` → removed, replaced by a comment naming
  `in_scopes` (ADR-0046).
- Budget trim → partial gap reads `relevant partial`, only shown keys named →
  `a_block_a_little_over_keeps_its_ranked_keys_and_drops_trailing_rules` passes.

Not observed before the push: the relais end-to-end check (a review run in
customer-vision whose decisions block lists `ui.design-system (ADR-0006,
duro-stack)`). Two runs there waited out the 30-minute lease budget while other
sessions held the heavy slots, and the branch's own decisions-block rendering
needs this relais released. It runs after the next relais release, when the
machine is quiet. Its input already holds: in customer-vision `aval relevant`
ranks `ui.design-system` active at duro-stack, and `amont-agent review-brief`
carries it (Phase 3 record).

## Decision log

- 2026-10-28 — Found during this phase: the worker's `aval relevant --top 20`
  (ADR-0043 work) made the ranked block pass the 16 KiB guidance budget, and
  `fit_budget` dropped the whole block, so a worker got no decisions at all
  (run-65ed51f4760d0-b28e: "relevant missing … 20415 bytes against a 16384-byte
  budget"). Fixed on this branch: whole entries leave from the end, rules
  before keys, the gap names them; worker lines name the answering scope
  (test `a_block_a_little_over_keeps_its_ranked_keys_and_drops_trailing_rules`).
- 2026-10-28 — The relais run's checks never got a lease (machine load, other
  sessions); its candidate was applied by hand, read, and verified through the
  commit gate.
