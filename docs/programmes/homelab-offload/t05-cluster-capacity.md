+++
type = "research"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["homelab"]
+++
# What capacity relais gets on the cluster without starving the services or Forgejo runners

## Question

Nodes, a namespace ResourceQuota, a PriorityClass relative to the Forgejo runners (`kubernetes/homelab/apps/forgejo-runners/`, `apps/priority-classes.yaml`), and node-local ephemeral storage. Known: wk-4 is down, wk-3 is reserved for AI work, the Forgejo `ci` pool was cut to 2 for disk contention with the Ceph OSDs, CephFS is about 46x slower on small files.

## Resolution

Explore research with read-only `kubectl`, 2026-10-31. The relais inspect run
was blocked by low disk.

**There is almost no headroom today outside wk-3.**

- **Nodes** (allocatable, then CPU / memory requested):

  | Node | Allocatable | Requested | Notes |
  |---|---|---|---|
  | wk-1-gpu | 12 CPU / 28.8 GiB | 56% / **99%** | OSD |
  | wk-2 | 8 CPU / 24.9 GiB | 84% / **90%** | OSD |
  | wk-3-gpu | 32 CPU / 62 GiB, 1.84 TB disk | 18% / 64% | AI-reserved, no OSD |
  | wk-4-gpu | — | — | **NotReady**; would add about 11 CPU / 12 GiB |

  The control planes are tainted NoSchedule. Ceph runs on 2 OSDs while wk-4
  is down. Sources: `infrastructure/homelab/terraform.tfvars`,
  `rook-cluster/ceph-cluster.yaml:96`.
- **Runners at their maximum replicas:**
  - requests about 3.3 CPU / 7.5 GiB, limits about 12 CPU / 28 GiB, 6 pods;
  - all on wk-1, wk-2 and wk-4 (`forgejo-runners/scaledjob-*.yaml`).
  - The binding constraint is disk I/O on the disks the OSDs share: Postgres
    WAL fsync went past 50 ms on 09-18, and Forgejo runner tasks failed.
    The constraint is not CPU.
- **Proposed enforcement:**
  - **Namespace:** a `relais` namespace, with a ResourceQuota of requests
    4 CPU / 8Gi, limits 8 CPU / 16Gi, 8 pods, ephemeral storage 40/80Gi, and
    0 PVCs. Its LimitRange: default 250m / 512Mi to 2 CPU / 4Gi, max
    4 CPU / 8Gi, ephemeral storage default 2Gi, max 10Gi. It is modelled on
    `preview-system/preview-resourceset.yaml:202-235`.
  - **PriorityClass:** `relais-batch` at -500 with `preemptionPolicy: Never`.
    That is above `expendable` and below the runners and services, so it
    never preempts them.
  - **Placement:** spread with `topologySpreadConstraints` (maxSkew 1).
  - **Scratch:** `emptyDir` on the node's ephemeral partition. `local-path`
    sits on the same disk; CephFS is slow and hits the OSDs.
  - **Quota scope:** worker pods and build pods share one quota.
- **Consequence.** Until wk-4 returns, a relais pod fits only by preemption
  or on wk-3. Whether wk-3 may take relais work is t19. Other questions for
  the plan: Pending versus evictable under pressure (-500, -1000 or 0); disk
  write load from relais builds on the thin-pool slices; namespace PSA (no
  buildah privilege needed).
