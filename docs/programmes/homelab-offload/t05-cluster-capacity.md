+++
type = "research"
mode = "afk"
status = "open"
blocked_by = []
repos = ["homelab"]
+++
# What capacity relais gets on the cluster without starving the services or Forgejo runners

## Question

Nodes, a namespace ResourceQuota, a PriorityClass relative to the Forgejo runners (`kubernetes/homelab/apps/forgejo-runners/`, `apps/priority-classes.yaml`), and node-local ephemeral storage. Known: wk-4 is down, wk-3 is reserved for AI work, the Forgejo `ci` pool was cut to 2 for disk contention with the Ceph OSDs, CephFS is about 46x slower on small files.
