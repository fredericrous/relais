+++
type = "research"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais", "amont"]
+++
# How remote capacity becomes a slot kind

## Question

`CapGroup::Remote` (`crates/relais/src/admission/mod.rs:101`) already means model-API work, so the name collides with remote execution. Host slots are local flock files (`amont-slots`) and the coordinator listens on a local Unix socket; the cluster needs its own source of truth for remote capacity, inside the amont slot ownership set by decisions:ADR-0051. A flock slot is freed when its holder dies; a remote slot is not, so this also settles a lease with a TTL and a heartbeat.

## Resolution

Explore research, 2026-10-31 (the relais inspect run was blocked by low disk).
Candidate design, which a later plan confirms.

- **Today.**
  - `ResourceClass` (`crates/relais/src/admission/mod.rs:101`) maps
    `ModelWork` to the private `CapGroup::Remote` (:125, :139).
  - The caps are `ConcurrencyLimits` (`policy/mod.rs:~1698`). It has
    `deny_unknown_fields`, so a new key is a schema change.
  - Leases are bound to a pid and process group, with start times guarding
    against pid reuse (`bind_group` :2735). They are swept when the group is
    gone, after `LEASE_GRACE` 300 s (:43).
  - `amont-slots` is std-only and flock-based, and holder death frees a slot
    (`amont/crates/amont-slots/src/lib.rs:1-22`).
- **Naming.**
  - Rename `CapGroup::Remote` to `ModelApi`: it is private, so this is a
    pure refactor, and the `max_active_agents` key stays.
  - Add `ResourceClass::ClusterJob`, `CapGroup::Cluster` and
    `max_cluster_jobs`.
  - Add `Refusal::ClusterUnavailable` and `Decision::WaitingForCluster`,
    modelled on `WaitingForDisk`.
  - Avoid the word "remote", which `RemoteGate` already uses.
- **Source of truth.** One Kubernetes `Lease` object per slot, in a
  dedicated namespace. Acquisition is a compare-and-swap on
  `resourceVersion`, holder `host/pid/dispatch_id`.
  - The TTL is about 60 s, renewed every 15 s through the coordinator
    heartbeat. Both numbers are proposals, not measurements.
  - A cluster-side reaper deletes expired Leases.
  - Job pods carry `activeDeadlineSeconds` and a label tied to their Lease,
    so no pod runs unaccounted for.
  - A ResourceQuota on pod count is only a backstop.
- **Laptop sleep.** The Lease expires and the pod is reaped. On wake, a
  failed renew means the lease was lost: cancel and re-queue, never resume.
- **Ownership** (decisions:ADR-0051, ADR-0049).
  - relais owns the class, the cap, the policy key, the decision variants
    and the renew call.
  - `amont-slots` stays host-only.
  - Who owns the cluster lease protocol is a new question for the person
    (t18).
- **Open, for t08, t17 and the plan:** a weight greater than 1 under a
  Lease-per-slot scheme; whether long jobs get a longer TTL class across
  sleep; whether a cluster load signal exists to mirror `Auto` sizing.
