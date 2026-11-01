+++
type = "grilling"
mode = "hitl"
status = "open"
blocked_by = ["t22-stamp-from-remote-run"]
repos = ["amont", "relais"]
+++
# Which tool runs a gate on the cluster

## Question

relais now runs every check as `amont run --stamp <gate>` (relais#370).
decisions ADR-0051 gives amont the gates, the stamps and the host slots.

Options:
- amont's `run` gains a cluster executor (with its lease), and relais calls
  `amont run` unchanged. This would also offload amont's own pre-commit and
  pre-push test runs.
- relais keeps an executor of its own, the reviewed but superseded design.

The architect seat decides, as the person asked for t18.
