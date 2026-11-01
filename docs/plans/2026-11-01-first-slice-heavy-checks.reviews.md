# First slice, heavy checks on the homelab: full reviews


- **Round 1** (all approve-with-changes):
  - **typescript** (31k, 47 s): setup on every executor; corepack via
    verdaccio; private scopes ineligible; pnpm store on the same PVC;
    case-sensitivity note.
  - **python** (32k, 52 s): `relais-job` in Python with unittest; a
    relais-made nonce; the msrv toolchain install; image covers the lint
    scripts; no manifest tests needed.
  - **rust** (43k, 60 s): `PROTOCOL_VERSION` bump; probe per `argv[0]` after
    prepare; the disk seam; an exit/signal trailer, stripped; provenance as
    an `Option`; `CARGO_BUILD_JOBS` from the pod.
  - **architect** (41k, 77 s): t18 decided, relais owns the lease (ADR-0012
    contract); the setup rule; probe in the repo dir; no meaning bump; a
    deadline self-kill.
  - **go** (42k, 60 s): no Go impact; a hand-written Flux CR with
    healthChecks; the ExecCredential wrapper.
  - **backend** (40k, 81 s):
    - per-check lease; `relais.toml` after install; one cluster-lost rule;
    - probe after prepare; trailer stripped;
    - numbers; status share and pass bar; observable checks;
    - holder fence; `adds=ops` (rejected: already detected).
  - **platform** (48k, 95 s): memory against llamacpp eviction; local-path
    floor and prune; the real egress floor; a bounded crates cache;
    rollback; Flux dependsOn and healthChecks.
- **Round 2, backend** (51k, 63 s), approve-with-changes: [blocking] an
  in-pod fencing token; [high] OOM via `memory.events`; lease before
  prepare; peak-hour capacity; line references.
- **codex**, rework, 4 applied:
  - [blocking] no priority-classes Kustomization to depend on;
  - [high] one cluster token per check, whatever the weight;
  - [high] an epoch fence (`resourceVersion` ordering is not guaranteed on
    v1.33);
  - [high] bypass the baseline cache while the cluster is on.

  Its medium finding was carried into implementation: rollback must remove
  the new config fields before reinstalling the older binary.
- **Final backend** (30k, 35 s), approve. Carried into implementation:
  - name the new tests under Verification;
  - scope the cache bypass to allowlisted repos;
  - the bypass adds base runs, which weighs against the pass bar.
- **Delta** on body 387ae4b1e8a6:
  - **go** approve (35k, 20 s). Carried: the kubeconfig exec v1 needs
    `interactiveMode: Never`.
  - **typescript** approve (35k, 15 s). Carried: corepack integrity keys
    with verdaccio.
  - **python** approve (38k, 51 s). Carried:
    - a cgroup `memory.oom.group` kill takes the helper too, so it reads as
      `lost`;
    - a `BrokenPipeError` on a dropped exec;
    - a pgid reused after a pod restart, so check the start time.
  - **platform** approve-with-changes (50k, 48 s): [blocking] ordered
    rollback, applied.
- **Backend** on body a010a9cd9631: approve (23k, 27 s). Carried: rebase
  branches cut with `local = true` before a rollback.

- **Re-bind** on body a010a9cd9631:
  - go approve. Carried: rollback step 2 also removes a hand-set
    `max_cluster_jobs` (`ConcurrencyLimits` denies unknown fields).
  - python approve.
  - typescript approve.
  - platform approve.

