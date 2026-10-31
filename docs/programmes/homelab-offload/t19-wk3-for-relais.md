+++
type = "grilling"
mode = "hitl"
status = "open"
blocked_by = ["t05-cluster-capacity"]
repos = ["homelab"]
+++
# May relais pods run on wk-3, or does offload wait for wk-4

## Question

The capacity research found wk-1 at 99% and wk-2 at 90% of memory requested,
wk-4 NotReady, and wk-3 (Strix Halo, 32 CPU / 62 GiB / 1.84 TB non-OSD disk,
18% CPU requested) reserved for AI work. Options: allow relais pods on wk-3
under a quota and a priority below the AI workloads; wait for wk-4 and keep
wk-3 reserved; or run relais pods on wk-1/wk-2 by preemption at a low priority.
Is the AI reservation's reason still binding for CPU-only pods?
