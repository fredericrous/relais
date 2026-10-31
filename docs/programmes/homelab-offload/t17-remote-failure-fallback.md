+++
type = "grilling"
mode = "hitl"
status = "open"
blocked_by = ["t02-remote-launcher", "t04-worktree-transport", "t08-placement-policy", "t12-laptop-cluster-path"]
repos = ["relais"]
+++
# What happens and what the person sees when a remote run cannot start or dies

## Question

Cases: the cluster is unreachable, a quota is hit or the budget is spent; the pod is evicted or OOM-killed; a patch is lost; the laptop sleeps or goes offline; SIGINT/SIGTERM or the session ends and the pod must be cancelled; an orphaned pod is reaped. Map the pod's exit status to a relais outcome and keep a local fallback safe to retry. Every message says what happened and why, gives the exact next command, and fits 80 columns.
