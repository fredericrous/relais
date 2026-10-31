+++
type = "grilling"
mode = "hitl"
status = "open"
blocked_by = ["t01-native-dispatch-amendment", "t03-credentials-billing", "t04-worktree-transport", "t05-cluster-capacity", "t12-laptop-cluster-path"]
repos = ["relais", "homelab"]
+++
# What launches a worker in a pod

## Question

Options: revive a headless `Backend` implementation (`crates/relais/src/backend.rs:487`, today `NativeBackend` and `MockBackend`) that runs in the pod; reuse the homelab's KubeOpenCode `AgentTemplate` (`homelab/kubernetes/homelab/apps/kubeopencode/`: ephemeral agent pods, litellm key, Forgejo token, Cilium default-deny egress; it runs OpenCode, not Claude Code); or run Claude Code plus the relais plugin inside the pod. Also settle whether the plugin's interactive review and decide steps work when the worker is remote.
