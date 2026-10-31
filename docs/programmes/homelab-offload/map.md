# Relais runs on the homelab

## Destination

relais places each run, a whole worker run or heavy checks, on the laptop or
the homelab cluster. Cluster runs stay under CPU, RAM, slot and token caps,
and their sandbox adapts automatically above a fixed floor. The laptop's load
drops by the target the laptop-load baseline sets, and this runs in
production.

## Notes

- Repositories: relais (owner), homelab, amont. Every session reads the
  `worktree-task` skill; homelab work also reads `forgejo-recipes`.
- The person's answers (2026-10-31): both whole runs and heavy checks move,
  chosen per task; usage control is cluster CPU/RAM caps, concurrency slots,
  a token/cost budget and a laptop-load threshold; the sandbox improves
  itself fully automatically; the end point is production.
- Workers today: every dispatch is a native Claude Code subagent inside the
  person's session (`adapter/native.rs`, `admission/native.rs`); `claude -p`
  was deleted in #173. `Backend`/`Harness` (`backend.rs:487`) is the only
  launcher seam.
- The old sandbox (`git show 07ed5c5f^:crates/relais/src/sandbox/`) was
  Claude Code's own `sandbox` settings: credential floor, probe and
  verification store, dispatch gate, denial report. Its improvement loop was
  manual (#164, #170).
- Decisions that bear on this: ADR-0001 `routing.session-model`, ADR-0009
  `workspace.local-reuse`, ADR-0010 `execution.resource-budgets` (unchanged by
  this programme), ADR-0003 `learning.automatic-promotion` (the model for the
  sandbox loop, which needs its own record), decisions:ADR-0045 (holdouts the
  writer cannot reach), decisions:ADR-0051 `harness.tool-boundaries` (amont
  owns host slots).
- Cluster facts: wk-4 down, wk-3 reserved for AI work, Forgejo `ci` pool cut
  to 2, CephFS slow on small files (use node-local ephemeral storage), no
  gVisor or Kata, Tetragon detect-only, Cilium default-deny egress templates
  in `apps/kubeopencode/`, mirrors for npm, Go and images but none for cargo.
- Branch convention: the programme skill names ticket branches
  `prog/<slug>/<id>`, but amont's push gate rejects a `prog/` prefix
  (`amont-runtime/src/vocabulary.rs:81`). Until that is reconciled, ticket
  sessions use `docs/homelab-offload-<id>`.
- Plan that charted this map: `docs/plans/2026-10-31-relais-runs-on-the-homelab.md`.

## Decisions so far

## Not yet specified

- A Rust build cache for cold pods (sccache, a cargo mirror).

## Out of scope
