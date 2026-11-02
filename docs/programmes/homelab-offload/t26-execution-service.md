+++
type = "grilling"
mode = "hitl"
status = "open"
blocked_by = []
repos = ["amont", "homelab", "relais"]
+++
# How work handed to the cluster outlives the laptop

## Question

The person's goal (2026-11-02): **the laptop keeps running what it can.
Work that cannot fit because of load runs on Kubernetes. Once handed over,
it no longer depends on the laptop: if the lid closes, the cluster runs go
on, and their results are there when the laptop, CI or relais asks.**

amont phase 3 (ADR-0016, amont#397/#399 and the 3c hardening) does not meet
the second half, because the laptop is the controller:
- the bundle goes in through a `kubectl exec -i` session the laptop holds;
- the laptop polls the pod, and its hook is the only consumer of the
  verdict;
- the laptop signs from facts it reads live, and the Job and its pod are
  deleted 300 s after it finishes (`ttlSecondsAfterFinished`).

So with the lid closed, a Job runs to its deadline, but its pass is lost.
The hook dies, or falls back locally once the laptop wakes. Each review
round also found races in the laptop-as-controller role (a signal
mid-`create`, a missing UID, a held slot). None of them gives a wrong
verdict, but they show the same shape.

What "outlives the laptop" needs, whichever mechanism carries it:
1. **Durable submission:** the snapshot lands somewhere durable before the
   laptop may leave (a Forgejo ref or an OCI artifact), not a stream into a
   waiting init container.
2. **A lifecycle owner in the cluster:** queueing, slots, retries and cancel
   by run id belong to something that is always on, not to a hook process.
3. **A durable result:** the outcome and the facts the signature rests on
   (exit codes, imageID, Job UID, tree) are kept beyond the Job's TTL, where
   the laptop collects them on wake, and CI or relais can read them.
4. **Trust kept:** today the laptop signs only from Kubernetes-recorded
   facts (t25). A collected result must still be judged from facts the
   laptop can verify, or t25 is reopened on purpose.
5. **Placement from load:** the decision to offload follows the laptop's
   load (t08, t13). It is not a fixed per-repository executor.
6. **Consumers that need not wait:** a hook cannot return before its
   verdict. A detached run fits relais whole-worker runs and heavy checks,
   `amont rehearse`, and pre-push checks CI would otherwise run. It does not
   fit the gate a commit is blocked on.

Candidate mechanisms:
- **Argo Workflows plus Argo Server's REST API.** Workflows persist with no
  client attached; semaphores replace the 409 slots; stop and terminate by
  label; workflow archive and artifacts for durable results. It also serves
  worker pods (t02).
- **A small gate-runner service and CRD.** amont submits a run, gets an id,
  and leaves; the controller owns the Jobs and writes the result to a
  status or a store.
- **Keep the kubectl executor** for synchronous gates only, and add
  detachment for long runs.

MCP fits as a front for agents (relais, Claude sessions) on top of either
service, not as amont's transport: a hook calls a plain HTTP API.

Decide with:
- the person's answers on 1–6 (the mechanism does not wait for t13; t13 sets the load threshold of point 5);
- phase 4's fallbacks per 100 runs by reason, and the Job overhead at p50
  and p90;
- how often runs are lost to sleep;
- whether t02 worker pods are scheduled.

## Resolution
