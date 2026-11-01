---
status: abandoned
branch: (never built)
repos: [relais, homelab]
adrs: [ADR-0010, decisions:ADR-0045, decisions:ADR-0049, decisions:ADR-0051]
---
# First slice: relais heavy checks run on the homelab

> **Abandoned 2026-11-01, before approval.** relais#370 moved verification into `amont run --stamp`, so the executor seam below is in the wrong tool. The person chose to re-chart first (`docs/plans/2026-11-01-rechart-the-homelab-offload.md`). Kept as an asset: see the notes in `docs/programmes/homelab-offload/t15-first-slice.md`.


## Review panel

👉 **Decide:** Phase 0 is yours: free laptop disk (three held worktrees, 2–5 GB each). Approving the plan bets that one 10Gi runner on wk-3 takes most heavy checks off the laptop.
📍 relais + homelab · planned, nothing built · next: free disk, start the sampler, then the homelab PR. Panel: architect, backend, rust, go, python, typescript, platform (opus-5-5), codex.
**Changed by review:** t18 decided by the architect (relais owns the lease; ADR-0012 pins the contract); the Lease is held per check, fenced by an epoch, with an OOM → fallback rule; the baseline cache is bypassed while the cluster is on.
📄 Full reviews: [2026-11-01-first-slice-heavy-checks.reviews.md](2026-11-01-first-slice-heavy-checks.reviews.md)
**Verdicts:** 7 approve-with-changes in round 1; backend approve-with-changes in round 2; codex rework (4 applied); a delta (go, ts, python approve; platform blocker on rollback, applied); backend approve on the final body. 9 findings carried into implementation; all seats re-bound to the final body.

## Context

The laptop runs at a load average of about 160 on 12 CPUs. relais heavy checks
are a large share of that load:

- check wall-time per day grew 3.5× from 10-25 to 10-31;
- on 10-31, 57 runs waited on a host slot, with a summed wait of 127k s;
- `make test` alone takes 367–837 s at weight 2.

The homelab-offload programme (`relais/docs/programmes/homelab-offload/`)
picked heavy checks as the first thing to move. They need no model
credentials and no change to native dispatch.

The person decided on 2026-10-31:
- **wk-3:** relais pods may run on wk-3 ("wk-3 ok to be runned on").
- **Cluster lease:** relais holds the cluster lease, "but let the architect
  decide".
- **Everything else:** decided here "as staff engineer and backend
  orchestrator & software factory expert".

**Outcome.** On the LAN, relais profile checks execute in a runner pod on wk-3
and give the same verdicts. Each check runs on one executor, the same at base
and at candidate. Whenever the cluster cannot take a check, the laptop runs it
locally.

## Decisions this plan records in the programme map

- **t19, wk-3.** Yes, quoting the person.
- **t18, who owns the cluster lease protocol.** Decided by the architect
  seat, matching the person's lean: relais owns it, in `cluster/lease.rs`.
  - t07 already gives relais the class, the cap and the renew call, and
    keeps `amont-slots` host-only.
  - There is no second consumer.
  - A cluster check takes no host slot, so ADR-0049 and ADR-0051 are
    untouched.

  ADR-0012 records it and pins the wire contract:
  - Lease name `relais-runner-<k>`, in namespace `relais`;
  - `holderIdentity` `host/pid/run/attempt/label`;
  - TTL 60 s;
  - renewal by an in-process thread, which replaces t07's coordinator
    heartbeat, so a holder's death means expiry;
  - expired means free.

  If a second tool needs the protocol, an amont crate adopts that contract
  through a superseding ADR.
- **t15, first slice.** This plan.
- **t01, native dispatch.** Amended only when worker pods land (t02), by a
  relais ADR at that time. A pod worker is relais's own worker: relais's
  router picks its model, and ADR-0001 still governs only the session's
  model. Not built here.
- **t03, credentials.** Direction: a long-lived Claude Code OAuth token
  (`claude setup-token`) in Vault, mounted only into worker pods. Spend is
  capped by relais's own ledger and cap, not by litellm, which lacks
  `/v1/messages` (t06). This is gated on t06's live test. Not built here.
- **t09, the sandbox floor.** Every relais pod gets:
  - PSA `restricted`;
  - non-root, no privilege escalation, all capabilities dropped,
    RuntimeDefault seccomp;
  - `automountServiceAccountToken: false`;
  - no secrets mounted;
  - no hostPath;
  - egress `world` denied, plus egressDeny to the `vault` namespace and the
    `kube-apiserver` (the diag-sandbox pattern).

  Other in-cluster services stay reachable: the clusterwide `dns-visibility`
  allow-all cannot be narrowed by an allow rule. This is stated as the real
  floor. The automatic loop (t11) may only tighten.
- **t13, laptop-load baseline.** The sampler starts in phase 0, before
  anything is enabled. The retrospective baseline is the events data from
  10-25 to 10-31 already recorded in t13.

A new relais ADR, ADR-0012 `execution.cluster-executor`, records three things:
- a check runs on the cluster only when its base and candidate both run
  there;
- any infrastructure doubt falls back to local, never to red;
- the t18 lease contract.

## Design

**Shape.** One long-lived runner pod per cluster slot: StatefulSet
`relais-runner` on wk-3, starting with **1 replica**.

- **Leases.** A check takes the slot by holding that replica's Kubernetes
  `Lease`.
  - The Lease is held **for one check**: taken after the `ClusterJob`
    coordinator token, released when the check ends. It is never held for
    the whole run, so the pod is not idle while a model works.
  - When a check cannot get the Lease within the admission wait, that check
    runs locally, at both base and candidate.
- **Tree.** The tree ships as a git bundle through `kubectl exec -i`, keyed
  by the commit sha. A tree already prepared in the pod is reused across a
  run's checks.
- **Execution.** Every setup step and command runs through a small in-pod
  helper, `relais-job`.
- **Caches.** The PVC keeps the cargo, rustup, pnpm store and `target/`
  caches warm. The pnpm `store-dir` and `/work` share it, so pnpm hard-links
  instead of copying.

No Job is created per check, so there is no pod-start latency.

**Runner pod** (homelab):
- Memory `requests == limits` = **10Gi**. A cargo link that runs out of
  memory kills its own pod, and a pod at its request is never the kubelet's
  first eviction pick.
  - wk-3 today: 62Gi allocatable, 40.8Gi requested, 24Gi actually used.
    llamacpp-chat has a 48Gi limit against a 16Gi request.
  - The pod fits by requests. The remaining risk is chat itself growing
    under pressure, which exists today. It is watched in the 14-day
    measure; raising chat's request is the follow-up if it happens.
- CPU: 2 request / 8 limit.
- PriorityClass `relais-batch` (-500, `preemptionPolicy: Never`). This
  governs scheduling only.
- `nodeSelector gpu=amd-strix-halo`. wk-3 has no taint, so no toleration is
  needed.
- A `local-path` PVC. local-path does not enforce its size: it is a
  directory under `/var/local-path-provisioner` on the partition kubelet
  shares.
  - Free space on wk-3's `/var` is measured before the PR.
  - `relais-job prepare` refuses to start below 20% free.
  - Above a 150Gi high-water mark it prunes the oldest `target/` dirs.

  So the cache cannot cause DiskPressure evictions.

**In-pod helper `relais-job`.** Stdlib Python 3, in the image. It is
unit-tested with `python -m unittest` in homelab `ci-validate`, covering:
exit 0, exit 3, death by SIGKILL, `kill` reaping a grandchild, output with
no final newline, and a tree mismatch.

- Every job id carries the holder. The pod cannot read the Lease (no token,
  apiserver denied). ADR-0012 defines an explicit acquisition epoch stored
  in a Lease annotation and incremented atomically with each acquisition;
  `resourceVersion` is used only for API compare-and-swap. Renewals retain
  the epoch. Pass Lease UID, epoch and holder to every helper operation.
  A pod-local lock serializes ownership activation and command registration.
  `prepare` rejects stale epochs before any write, prune or reap; it
  persists a newer epoch and reaps the previous group before allowing
  new work. `run`, `kill`, `probe` and `fetch` require the exact active
  ownership tuple. Ownership state survives pod restarts on the PVC.
  A changed Lease UID fails closed until a drained reset reconciles the
  Lease and PVC state; epochs never silently restart. Renew and release
  also require matching ownership and CAS. Tests delay an old `prepare`
  until after takeover and verify no successor files or processes change,
  including concurrent helper calls and pod restart.
- `prepare <job> <sha> <tree>`:
  1. Read the bundle on stdin.
  2. Check the free-space floor and prune.
  3. Reap any leftover group from a dead holder.
  4. Clone, check out `<sha>`, and verify that `HEAD^{tree}` equals
     `<tree>`.
  5. Run `rustup toolchain install` for the toolchain `rust-toolchain.toml`
     pins and for `Cargo.toml`'s `rust-version`, through the proxy.
     `make msrv`'s `rustup run` never installs one.

  A failed install means fallback to local, never a red check.
- `run <job> <cwd> <deadline_s> <nonce> -- argv…`:
  - Runs the command with `start_new_session`, records the pgid, sets the
    cache env plus `CARGO_BUILD_JOBS`/`MAKEFLAGS` from the pod's CPU limit
    (not the laptop's weights), and streams output.
  - After a forced newline, the last line is `RELAIS-EXIT <nonce>
    exit|signal <n>`. relais generates the nonce for each attempt and
    accepts only an exact match on the last line.
  - relais strips the trailer before writing the local log, so
    `log_sha256` is comparable to a local run.
  - **A missing trailer means `cluster_lost`, never a check failure.** A
    dropped `kubectl exec` also exits 1.
  - A cgroup OOM kills `rustc`, not the helper: `relais-job` compares
    `memory.events` `oom_kill` before and after and reports `RELAIS-EXIT
    &lt;nonce&gt; oom`, which relais treats as a fallback (reason `oom`), never
    red. A unit test covers it.
  - When `deadline_s` passes (the applied limit plus the lease TTL), it
    kills its own group, so a holder that dies or sleeps leaves no job
    running.
- `kill <job>` kills the recorded group. relais calls it on timeout and on
  cancel.
- `fetch <job> <path>` copies a JUnit file back.
- `probe <job> <program…>`:
  - Runs after `prepare`, in the prepared repo directory, so it reports the
    toolchain `rust-toolchain.toml` pins.
  - Versions every program `resolve_toolchain` would (each command's
    `argv[0]`, e.g. `make`), plus the pinned `rustc`, `pnpm --version`
    (corepack-resolved) and `uname -sm`.
  - Its os/arch replace the laptop's. A program it cannot version refuses
    the cache, as it does locally.

**Egress.** Cilium deny rules beat allows, and `toFQDNs` cannot get past
`egressDeny world`, so every download comes from inside the cluster:
- npm comes from verdaccio. `npm_config_registry` and
  `COREPACK_NPM_REGISTRY` point at `npm.packages.svc:4873`, so corepack can
  fetch a repo's `packageManager` pnpm.
- crates and rustup come from a new `crates` nginx caching proxy in
  `packages`, on the `prebuilds` pattern.
  - It proxies index.crates.io, with `config.json`'s `dl` rewritten to
    itself, plus static.crates.io and static.rust-lang.org.
  - Its cache is bounded: `max_size=40g` on a 50Gi PVC, after checking
    `ceph df`, since Ceph's full ratio is raised while wk-4 is down.
  - Index paths are cached for at most 5 minutes; crates and toolchains for
    90 days.
  - rustup installs repo-pinned toolchains on demand, so a toolchain bump
    needs no image rebuild. The probed versions are the toolchain identity.
- Repos that depend on the private `@fredericrous` and `@duro-app` scopes
  are not eligible until verdaccio proxies those scopes.

**Image** `services/relais-runner/Dockerfile` (homelab).
- Debian slim, plus: git, make, build-essential, pkg-config, python3,
  rustup (no toolchain), node 22 with corepack, tini, and `relais-job`. It
  runs as uid 1000.
- Cargo `[source]` replacement and `RUSTUP_DIST_SERVER` point at the proxy.
- It is built by a copy of `build-homelab-runner.yaml` and pinned by tag
  plus digest.

**relais (code):**
1. **Config.**
   - `MachineSettings.executor.cluster` (`policy/mod.rs:921-1001`, on the
     `ResourceSettings` pattern):
     - on/off: `enabled`;
     - connection: `kubeconfig` (a path, required; never the default context
       `admin@homelab`), `namespace`, `statefulset`;
     - capacity and leases: `replicas`, `lease_ttl_seconds` (60),
       `renew_seconds` (15);
     - scope: `repos` (an allowlist).
   - `CommandSpec.local: bool` (`policy/mod.rs:223-268`), skip-serialized
     when false.
2. **Admission and the wire.**
   - `ResourceClass::ClusterJob` and `CapGroup::Cluster`
     (`admission/mod.rs:101-160`). The `group_cap` arm and an `ALL` array of
     4 change with them.
   - `ConcurrencyLimits.max_cluster_jobs`, defaulting to `replicas`.
     Cluster admission charges exactly one token per check, independently
     of `CommandSpec.weight`; its capacity validation also uses that one
     token. Local fallback restores the declared heavy weight and takes
     normal host admission. Test that weight-2 `make test` runs remotely
     with one replica and consumes two host slots on local fallback.
   - Rename `CapGroup::Remote` to `ModelApi`.
   - `ResourceClass` is serde on the coordinator wire, so `PROTOCOL_VERSION`
     goes 6 → 7 (`coordinator/mod.rs:207`). A test proves an old daemon is
     refused, and the coordinator is stopped at install.
   - `Ask` (`heavy.rs:301`) gains a class. A cluster ask takes `ClusterJob`
     tokens and skips amont host slots. It sends no `disk` ask
     (`heavy.rs:588`), skips `disk.short` (644), and skips the
     no-coordinator `wait_for_disk` (552).
   - `tree_before` and `growth.record` (`verify/mod.rs:1302,1423`) move into
     the local executor. A remote check never stalls on the laptop's disk.
3. **Lease** `crates/relais/src/cluster/lease.rs`.
   - Acquisition is a compare-and-swap (`kubectl replace` with
     `resourceVersion`); the Lease is created if absent.
   - A renew thread runs every 15 s, and Drop releases the Lease
     (`resources.lifecycle-is-scoped`).
   - A failed renew means `cluster_lost`.
   - It sits behind a `Kube` trait with `SystemKube` (kubectl through
     `procs::run_with_timeout`, `procs.rs:736`, always with `--kubeconfig`)
     and `FakeKube`, on the `Git`/`FakeGit` pattern.
4. **Executor seam.**
   - At `run_attempt` (`verify/mod.rs:1271-1457`), the spawn-and-wait moves
     behind an `Executor` with two implementations: `Local`, today's code
     unchanged, and `Cluster`.
   - `Cluster` streams into the same local log file with the trailer
     stripped. relais computes `log_sha256` locally; JUnit comes back
     through `fetch`.
   - Cancellation: the existing `AtomicBool` is polled. Cancel and timeout
     both call `kill`.
5. **Placement**, decided per command. A command runs on the cluster when
   all of these hold:
   - the cluster is enabled;
   - the repo is allowlisted;
   - the command is not `local`;
   - the Lease is acquired, then, under it, `prepare` and `probe` succeed
     (prune never runs without the Lease).

   The rules that follow from it:
   - Setup runs on every executor that hosts at least one of the phase's
     commands. relais's `make toolchain` runs on both. A setup failure on
     one executor blocks only that executor's commands.
   - **One rule for losing the cluster.** On `cluster_lost` (pod lost or
     preempted, renew failed, trailer missing), that check re-runs locally
     at both base and candidate, and the receipt records
     `executor_fallback{reason}`.
     - The run ends `blocked (cluster_lost)` only when the local re-run is
       refused too. It never ends red.
     - Fallback reasons are cluster off, repo not listed, `local`, prepare,
       probe, lease or lost.
   - The base's image digest is recorded from the pod status `imageID`. If
     the candidate's digest differs, that check re-runs at base.
   - `CheckStarted` gains `executor` (`local`, or `cluster:<pod>`).
     `relais status` shows the share of checks run on the cluster and the
     fallback counts by reason. That is t08's display, and it is how
     "broken" is told apart from "off".
6. **Cache key.**
   - `Toolchain` (`verify/mod.rs:2584`) gains
     `provenance: Option<{executor, image_digest, os, arch}>`, skip-serialized
     when local.
   - Local keys do not move, and `BASELINE_MEANING_VERSION` stays at 5: the
     inputs changed, not the meaning (`verify/mod.rs:2689`).
   - For this slice, when cluster execution is enabled, bypass all baseline
     cache reads and writes, including runs that ultimately fall back
     entirely to local. Record this cache refusal and each check's actual
     executor/toolchain provenance. The current cache stores a whole
     profile under one toolchain key and cannot represent mixed placement.
     Cluster-disabled runs retain existing cache behavior. Tests cover
     mixed local/cluster commands and fallback after earlier remote checks,
     proving that neither reads nor writes a baseline entry. Caching mixed
     execution is deferred until keys describe every check's environment.
7. **Out of this slice:** holdouts stay local. relais's `make audit` (GitHub
   advisory DB) and `make plugin` (the `claude` CLI under bun; a fixed
   5000 ms per-test limit wants a quiet host) stay local.

## Numbers

- **Capacity.** About 300 checks a day at 40–52k laptop wall-seconds a day
  (t13). One replica has 86,400 s a day. It is enough if wk-3 runs a check
  at least as fast as the loaded laptop, which is likely: 32 cores at 5%
  CPU against 12 cores at load 160. A second replica follows if the
  measured cluster share is lease-limited.
  - Peak: if the 40–52k s fall in about 12 working hours (43.2k s), one
    replica is over 100% busy at peak, and checks that wait past the
    admission wait (default 1800 s) run locally. The pass bar is judged on
    that basis. A warm `make test` on wk-3 at `CARGO_BUILD_JOBS=8`, with its
    peak memory, is timed before Phase 3; a second replica is added then if
    the arithmetic says so and wk-3 memory allows.
- **wk-3 memory.** Allocatable 62Gi; requested 40.8Gi; used 24Gi. The 10Gi
  runner fits.
- **Transfer.** relais's `.git` is 25 MB, so a bundle takes about 2 s at
  100 Mb/s. It is sent once per prepared tree, not once per check.
- **Measured before the PR:** free space on wk-3's `/var`, and `ceph df`.

## Non-goals

- Worker pods (t02).
- Credentials (t03).
- The automatic sandbox loop (t11).
- The placement policy and load threshold (t08).
- Off-LAN reach (t12's Tailscale blocker). Off-LAN, relais falls back to
  local.
- amont's own pre-commit and pre-push test runs.
- TS repos on the private scopes.

## Phases

0. **The person frees laptop disk.** 4.3 GB is free against relais's 8 GiB
   floor, so every relais run and this work's own builds stall. Candidates
   are the sweep's held worktrees:
   - `relais-wt-budget-cut`, 4.8G, its PR #340 closed;
   - `relais-wt-timeout-rerun`, 4.9G, never pushed;
   - `relais-wt-probe-test-budget`, 2.2G.

   Deleting them is the person's call. Then start the t13 sampler: a
   launchd shell script following the t13 spec, recorded in the t13 ticket.
1. **homelab PR** (Forgejo). First measure free space on wk-3's `/var` and
   run `ceph df`.
   - Image and helper: `services/relais-runner/` (Dockerfile, `relais-job`,
     `test_relais_job.py`), and the build workflow.
   - Mirror: `apps/packages/crates.yaml`.
   - Priority: `relais-batch` in `apps/priority-classes.yaml`.
   - `apps/relais/`, on the diag-sandbox layout:
     - the namespace, with PSA `restricted`, not ambient;
     - ResourceQuota and LimitRange;
     - the StatefulSet, with a local-path volumeClaimTemplate;
     - the CNP;
     - ServiceAccount `relais-coordinator`, with a Role allowing pods
       get/list/watch, `pods/exec` create, and leases full;
     - its token Secret, the breakglass pattern with `ssa: IfNotPresent`.
   - The Flux CR `apps/relais.yaml` and its line in
     `apps/kustomization.yaml`.
     - It is hand-written, not `task new:app`. Neither generator profile
       fits: `deployment` writes a Service and netpol and needs
       `--allow-from`, and every profile writes a Vault policy and a
       SecretStore, against t09.
     - It carries `dependsOn` for the existing packages Kustomization,
       and `healthChecks` on StatefulSet `relais-runner`. Priority classes
       remain plain resources applied by the parent apps Kustomization;
       there is no priority-classes Flux Kustomization to depend on.
       Validate that every dependency resolves to a rendered Flux CR.
     - The amont-homelab pre-commit checks gate it.

   Merge, then watch Flux reconcile it and the pod become Ready.

   **Laptop kubeconfig.** The token goes into the Keychain. The kubeconfig is
   `~/.config/relais/kube/homelab.yaml`, mode 600. Its `exec` plugin is a
   small wrapper that prints an `ExecCredential`
   (`client.authentication.k8s.io/v1`, `status.token`) from
   `security find-generic-password -w`, so the token never appears in argv.
2. **relais PR** (GitHub), carrying items 1–6 and ADR-0012:
   - `SPEC.md` (the cluster executor section, next to SPEC:329's "execution
     backend" line);
   - `CHANGELOG.md`;
   - the machine.toml docs;
   - the programme map: Resolutions for t01, t03, t09, t15, t18 and t19,
     plus the t13 note.

   Not in this PR: `relais.toml`. `CommandSpec` denies unknown fields
   (`policy/mod.rs:224`), so `local = true` waits until the binary that
   parses it is installed. Then release it through `tag-release`, install
   it, and stop the coordinator.
3. **Enable.**
   - A small relais PR marks `local = true` on audit and plugin in
     `relais.toml`.
   - Turn on `[executor.cluster]` in the person's `machine.toml`, writing it
     by hand in this session.
   - Run one real relais `verify` of a recent candidate, with the executor
     on and with it off.
   - Observe for 14 days.

## Rollback

- **Off switch:** `executor.cluster.enabled = false` in machine.toml. The
  next check runs local.
- **relais:** in this order, because `CommandSpec` and `MachineSettings` deny unknown fields (`policy/mod.rs:224`, `:924`) and the older binary would refuse both files:
  1. revert the `local = true` PR in `relais.toml`;
  2. delete the `[executor.cluster]` table from machine.toml;
  3. reinstall the previous release.
  - The meaning version is unchanged and local keys do not move, so cached
    local bases stay valid.
  - The coordinator is restarted, for protocol 6.
- **homelab:** revert the PR. Flux prunes `apps/relais`. The local-path
  reclaim policy is Delete, so only caches are lost. `relais-batch` and the
  crates proxy can stay.

## Verification

- **Unit and integration (relais).** `make check` exits 0, and the new tests
  appear by name in its output:
  - `FakeKube` lease CAS: acquire, conflict, expiry, a renew failure leading
    to `cluster_lost`, and the holder fence.
  - The trailer parser: exit, signal, missing, malformed, and a fake
    `RELAIS-EXIT` with the wrong nonce printed by the check.
  - An old-protocol coordinator is refused.
  - A fake `kubectl` on PATH, through `tests/it/fake_bin`, drives a full
    `kind: verify` run, covering:
    - cancel and timeout sending `kill`;
    - a probe failure leading to fallback;
    - `cluster_lost` leading to a local re-run at both sides;
    - a digest mismatch leading to a base re-run.
  - A local baseline key is byte-identical before and after.
- **homelab.**
  - `python -m unittest` on `test_relais_job.py` passes in `ci-validate`.
  - `kustomize build kubernetes/homelab/apps/relais` and `flux diff` run
    before the merge.
  - After the merge, `flux get kustomization relais` turns Ready only after
    `relais-runner-0` is Ready on wk-3.
  - `kubectl --kubeconfig ~/.config/relais/kube/homelab.yaml auth whoami`
    names `system:serviceaccount:relais:relais-coordinator`.
  - `kubectl auth can-i`: exec and leases are allowed; secrets and other
    namespaces are denied.
  - From the pod:
    - `curl` to `npm.packages.svc` and `crates.packages.svc` succeeds;
    - `curl https://example.com` fails, and so do vault and the API server;
    - a cold `cargo fetch` of relais succeeds;
    - `corepack pnpm --version` works with egress denied.
- **End to end.** A relais `verify` of one recent candidate, with the
  executor on and with it off:
  - both receipts show the same pass/fail per label and the same JUnit test
    counts;
  - `CheckStarted.executor` shows `cluster:relais-runner-0` for lint, test
    and msrv;
  - the sampler's laptop CPU during `make test` averages below 1 core,
    against the local run;
  - `relais-job kill` mid-check ends that check as Cancelled;
  - deleting the pod mid-check re-runs that check locally at both sides,
    with `executor_fallback{reason=lost}` and no red;
  - with the cluster unreachable, the run is fully local, with
    `executor_fallback`.
- **Measure, over 14 days, against the t13 baseline:**
  - the share of eligible checks run on the cluster, and fallbacks by
    reason;
  - slot waits per day;
  - the 15-minute load median;
  - llama.cpp evictions and DiskPressure events on wk-3.

  Pass bar: at least 60% of eligible checks run on the cluster, slot waits
  per day are at least halved, and no llama.cpp eviction is caused by the
  runner.

<!-- panel: repos=relais,homelab reviewers=architect,backend,language,platform,codex body-sha=a010a9cd9631 -->
