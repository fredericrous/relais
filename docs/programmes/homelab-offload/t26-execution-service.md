+++
type = "grilling"
mode = "hitl"
status = "closed"
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

Decided by the architect and staff-engineer seats on the person's delegation
(2026-11-02): "take the best decisions as a software architect and staff
engineer. goal is autonomy, reduce load, get our coding sessions completed
faster. solution can be hybrid. sometimes stream from the laptop sometimes
clone directly the project. i can provide a service account/creds".

**Two lanes, one cluster.**
- **Synchronous lane** (a gate a commit or push is waiting on): the ADR-0016
  `kubectl` executor stays as it is, with the bundle streamed through `exec`.
  The laptop is awake by definition, and a closed lid falls back locally on
  wake. Phase 4's overhead numbers decide whether this lane later moves onto
  the detached lane's template. The criterion is Job overhead at p90 no worse
  than a WorkflowTemplate run. Until then there are two executors, on purpose.
- **Detached lane** (work nobody is blocked on):
  - `amont rehearse`;
  - `platform=any` pre-push gates;
  - relais heavy checks (through `amont run --stamp`);
  - later, relais whole-worker runs (t02).

  Each becomes an **Argo Workflow**, already installed on the homelab
  (`controllers/argo-workflows`, used by trade-agents). Nothing new to run.

**Detached lane design:**
1. **Submission is a Forgejo snapshot first, a direct clone second.**
   - At submit time, a detached run's commit is usually not on any remote: a
     pre-push, a rehearsal, a relais candidate. So the laptop pushes a
     snapshot commit to a per-project Forgejo repo (`amont-snapshots/<repo>`,
     ref `refs/snapshots/<run>`) and reads it back with `ls-remote`, because
     Forgejo has lost accepted pushes.
   - The snapshot keeps its **real parent**, so a push sends only the delta,
     not the whole tree. The parentless form is ADR-0016's, for the bundle.
   - A commit already reachable from a Forgejo pull-mirror of the project
     (origin/main baselines) is fetched from the mirror and nothing is
     pushed. A mirror that lags falls back to the snapshot.
   - GitHub repositories get Forgejo **pull-mirrors**, so pods only ever
     fetch from Forgejo in the cluster. No GitHub token sits in a gate
     namespace, and the sandbox keeps denying world egress.
   - Snapshots are garbage-collected after 7 days by a CronWorkflow.
2. **Argo owns the lifecycle.**
   - A WorkflowTemplate `gate-run` in `amont-gates`, pinned by image digest.
   - Slots are Argo semaphores (a ConfigMap), not 409s. Cancel is
     `argo stop` by label, through the Kubernetes API.
   - The controller watches `amont-gates`; add it to `workflowNamespaces`.
   - Each Workflow sets its own `ttlStrategy` (7 days), `serviceAccountName`
     and deadline, not the chart's 1-day / 7200 s / `argo-workflow`
     defaults.
   - The laptop submits a `Workflow` through the Kubernetes API as
     `amont-gate-runner` (its Role gains workflow verbs). Argo Server stays
     ClusterIP-only: it runs `authModes: server`, which is unauthenticated.
   - Only our template runs: check `templateReferencing: Strict`, which is
     controller-wide, against trade-agents' inline workflows before turning
     it on.
3. **The sandbox floor changes, deliberately (amends t09).**
   - Argo's executor reports results through the apiserver, which the
     `amont-gates` CNP denies, and a network policy is per pod. Detached-lane
     pods therefore get the apiserver in their CNP. Their token is mounted
     only into the executor's containers (`automountServiceAccountToken:
     false` on the pod, plus the executor's own service account limited to
     `workflowtaskresults`).
   - Gate code can reach the apiserver over the network, with no token.
   - The fetch runs as its own step, reaching only Forgejo.
4. **Results are durable.** The Workflow's node status, recorded by the
   Argo controller, carries:
   - the exit codes, the observed tree and the workflow UID;
   - the template's pinned image digest, which replaces the pod's
     `imageID` that pod GC removes.

   It is kept 7 days. Logs go to the Ceph RGW artifact store, which already
   exists. A record lost to TTL means the run is redone, never trusted.
5. **Trust: the laptop stays the only signer, by decision (t25 kept).**
   - The laptop collects finished Workflows: amont at every hook start, as
     one cheap list call, and the relais coordinator for its runs. It turns a
     pass into the same cluster token and `x86_64-linux` attestation as ADR-0016.
   - **What this slice does not deliver:** while the lid is closed, nothing
     acts on a result. CI runs anyway, and the result only speeds up the
     laptop's next commit, push or relais step. relais's repair and review
     loop stalls once the leaf step running at the time finishes.
   - **t25 reopens when relais worker pods land (t02).** The likely answer
     is an in-cluster signer limited to `platform=any` gates, with its own
     allowed-signers entry, so a closed lid no longer stalls the
     coordinator.
6. **Placement is by load.**
   - amont sends a `platform=any` gate to a lane when the host slots are
     busy or the laptop's load is over the threshold t13 sets. That replaces
     "defer to CI" when the cluster is reachable.
   - relais places whole runs by t08.
7. **What a closed lid does, per consumer:**
   - a commit gate falls back locally on wake (unchanged);
   - a detached pre-push gate lets the push go ahead, like a deferral to CI.
     Its result becomes an attestation when collected, and a CI leg that
     runs first simply runs;
   - a rehearsal returns at once, and the next push collects it;
   - a relais run shows "running remotely" and resumes on wake.
8. **Credentials, the minimum:**
   - a Forgejo bot account with read on the mirror and snapshot repos (the
     pods) and push on `amont-snapshots/*` (the laptop, through the person's
     existing token);
   - the pull-mirrors' own GitHub read token, if any repository is private;
   - later, the Claude credential for worker pods (t24).

   Vault is down (2026-11-01), so secrets use the breakglass
   `ssa: IfNotPresent` pattern, keeping each value out of every transcript:
   `! read -s T && kubectl … create secret generic … --from-literal=token="$T"`.

**Next:** a build plan, "detached cluster runs", through plan mode and the
review panel. It spans amont (the lane, the collector, an ADR beside
ADR-0016), homelab (pull-mirrors, the snapshot org, Argo namespace and
template, CNP, RBAC) and relais (collect, a remote run state). Its first
slice is `amont rehearse --cluster` end to end.


## Build state 2026-11-11

A review of greengate (v3.1.0) against this Resolution. The detached lane
(ADR-0017: `rehearse --cluster`, `cluster collect`, collect at hook start, the
Forgejo snapshot and mirror fetch, the laptop-signed `x86_64-linux` block) is
built and released. These parts are **not built**:

- **A `gate-run` WorkflowTemplate, Argo semaphores and artifacts.** The
  Workflow is an inline spec with `parallelism: 1` and a 48 h TTL
  (`workflow.rs:191-199,417`), not a 7-day result. The Argo controller has no
  `artifactRepository` configured, so "the artifact store already exists"
  holds for the Ceph object store only, not for Argo.
- **Placement by load** (item 6). A gate goes to the cluster whenever
  `executor cluster` is set; the detached lane is chosen only by `--cluster`
  or `amont.rehearse.cluster`.
- **An in-cluster signer, and relais collect with a "running remotely" state.**
- **Detached pre-commit gates.** `eligible` takes only pre-push externals
  (`detached.rs:149-170`). relais declares `module-cycles` and `child-cycles-*`
  at pre-commit with no pre-push twin, so the first slice as planned would
  print "no gate of this rehearsal can go to the cluster". They fit the Job
  lane, or need pre-push twins.
- **Enabling it.** No `amont.cluster.*` key is set on the laptop and no
  repository declares `platform=any` or `executor cluster`: phase 3 never ran,
  and its live checks (a closed-lid run, a held notes-ref lock, collect within
  300 ms, a pod at the 4Gi ceiling) are unrecorded.
- **Names change at greengate 4.0** (a local branch, `feat/greengate-hard-rename`,
  not on origin): `amont.cluster.*` becomes `greengate.*`, `AMONT_*` becomes
  `GREENGATE_*`, the run label `amont.run` becomes `greengate.run`, and the
  homelab namespace `amont-gates` has an unticked cutover to `greengate-gates`.
