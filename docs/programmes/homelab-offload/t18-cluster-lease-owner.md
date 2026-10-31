+++
type = "grilling"
mode = "hitl"
status = "open"
blocked_by = ["t07-slot-model"]
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
