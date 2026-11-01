+++
type = "grilling"
mode = "hitl"
status = "open"
blocked_by = ["t07-slot-model", "t21-remote-runner-owner"]
repos = ["relais", "amont"]
+++
# Who owns the cluster lease protocol

## Question

The slot-model research proposes counting cluster capacity as one Kubernetes
`Lease` object per slot, renewed by the relais coordinator. decisions:ADR-0051
and ADR-0049 give amont the *host* slots in the std-only `amont-slots` crate;
a cluster lease is neither a host slot nor std-only. Is the protocol owned by
relais alone, or by a new amont crate (following ADR-0049's pattern) because
other heavy tools on the machine will queue on the cluster too? Is a decision
record needed?

## Notes

On 2026-11-01 the architect seat decided "relais owns it" while relais was
still the runner (reviewed plan `docs/plans/2026-11-01-first-slice-heavy-checks.md`).

The reasons:
- t07 gives relais the class, the cap and the renew call;
- no second consumer exists;
- a cluster check takes no amont host slot.

The wire contract it pinned:
- a Lease `relais-runner-<k>` in namespace `relais`;
- holder `host/pid/run/attempt/label`;
- TTL 60 s, renewed by an in-process thread;
- expired means free;
- fencing by an acquisition epoch in a Lease annotation, because
  `resourceVersion` ordering is not guaranteed on Kubernetes v1.33.

Since then, relais verifies through `amont run --stamp` (relais#370). If t21
makes amont the remote runner, the runner owns the lease, so this ticket waits
on t21.
