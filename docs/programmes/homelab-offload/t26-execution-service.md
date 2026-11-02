+++
type = "grilling"
mode = "hitl"
status = "open"
blocked_by = ["t13-laptop-load-baseline"]
repos = ["amont", "homelab", "relais"]
+++
# Does an in-cluster execution service replace laptop-driven Jobs

## Question

amont phase 3 (ADR-0016, amont#397/#399 and the 3c hardening) drives each
cluster gate from the laptop with `kubectl`. It creates the Job (a 409 means
the slot is taken), waits for `fetch`, streams the bundle in with `exec -i`,
polls the pod and deletes the Job. The laptop is the controller, and every
review round found races in that role: a signal mid-`create`, a missing UID,
a held slot. None of them can give a wrong verdict, because any trouble
re-runs the gate locally. A leaked slot lasts at most `activeDeadlineSeconds`
plus the TTL, within the quota.

The person asked (2026-11-02) whether an execution server reached by API or
MCP, possibly on Argo Workflows, should replace it, and chose to ship the
kubectl executor first and decide here with phase 4's numbers.

Options:
1. **Keep kubectl.** Nothing new to run; the costs are the fallbacks and
   leaked slots phase 4 measures.
2. **A small gate-runner API in-cluster.** amont submits a bundle and gate
   spec and gets a run id, which it can cancel and wait on. The server owns
   the queue, the slots, Job lifecycle and cancel. Reopens t25: the laptop
   would sign what the server reports, unless it still reads the Job's
   recorded facts itself, read-only.
3. **Argo Workflows plus Argo Server's REST API.** Semaphores replace the
   409 slots, stop and terminate by label give cancel, and the TTL and
   archive come built in. It also serves worker pods (t02) and research
   fan-out. It adds a controller and a server to operate on the homelab.

MCP fits as a front for agents (relais, Claude sessions) on top of 2 or 3,
not as amont's transport: a hook calls a plain HTTP API.

Decide with:
- phase 4's fallbacks per 100 runs by reason;
- leaked-slot minutes;
- Job overhead at p50 and p90;
- whether t02 worker pods are scheduled.

## Resolution
