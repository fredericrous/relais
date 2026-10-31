+++
type = "research"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais", "amont"]
+++
# How a heavy check runs in a pod and returns its outcome and logs

## Question

Today the coordinator holds a lease by local process-group id and host slots are local flocks (`crates/relais/src/verify/heavy.rs`); a pod has neither. Heavy checks need no model credentials and no native-dispatch amendment. Research a result contract binding each verdict to the immutable tree, run/attempt, check profile and the actual execution environment (OS/architecture, toolchain, container image digest); how base and candidate checks stay comparable; how cache keys use remote provenance; which platform-specific checks must stay local. Report the decisions:ADR-0045 holdout boundary (where acceptance executes, which identity fetches holdouts, how worker and reviewer inputs exclude their sources and logs) before this Resolution closes. Align with t04's transport research.

## Resolution

Explore research, 2026-10-31. The relais inspect run was blocked by low disk.
Recommended shape: an **optional remote executor for profile checks only**,
with the host still in charge.

- **Today.** The host takes `HeavyLocal` coordinator tokens, then amont host
  slots, then binds the lease to the check's process group (with a 30 s
  heartbeat and a 300 s reap) (`crates/relais/src/verify/heavy.rs:1-38,854,876`).
  - The command timeout is the smaller of its own timeout and each budget's
    remainder (`verify/mod.rs:1152-1236`).
  - The spawn and wait are at `verify/mod.rs:1250-1330`, and `CheckOutcome` is
    at `verify/mod.rs:115-141`.
  - `baseline_key` hashes the tree, profile, tools and `Toolchain{os, arch}`
    from the running process (`verify/mod.rs:2516-2610`).
- **Shape.**
  - A remote executor replaces only the spawn and wait. The host keeps the
    lease, the budgets, cancellation and the ledger.
  - Remote capacity is its own resource class with its own cap, and it skips
    amont host slots. Pod startup is charged to admission wait.
- **Result contract.** It binds:
  - the tree sha (the TREE object), run, attempt and phase;
  - the label, the profile hash, argv and env;
  - os, arch, the toolchain probed inside the pod, and the image digest (never
    a tag);
  - the `CheckOutcome` fields, plus `pod_uid` and the exit reason (OOMKilled,
    evicted, deadline);
  - the log bytes, verified against `log_sha256`.
- **Comparability.**
  - Base and candidate run on the same executor and the same image digest.
    A mixed pair is cache-refused and the base re-runs.
  - The cache key gains `{executor, image_digest, os, arch}` from the
    executor's own report, with a `BASELINE_MEANING_VERSION` bump.
- **Stays local:** macOS/iOS checks; checks that need local services, the
  seeded `target/` or `.venv`, the simulator, LAN-only services or the mTLS
  cert; and core-weighted parallelism (a pod sizes from its own limits).
- **The ADR-0045 holdout boundary.**
  - Holdouts stay on the host in v1. Today relais fetches them with the
    person's own git/ssh identity, into a state directory outside every
    worktree (`holdout.rs:778,806`, `runner/holdouts.rs:406-500`), and the
    worker sees only the statement, the verdict and the failing ids
    (`runner/prompt.rs:17-43`).
  - A holdout in a pod would need a read-only per-run token, separate from
    the worker's identity, no shared pod or node pool with a worker, and pod
    logs covered by the exposure scan.

The open questions feed the first-slice ticket and t04: does the image carry
the toolchain or install it per run, how is the tree delivered, what cap
replaces the host slot, and which profiles are Linux-clean.
