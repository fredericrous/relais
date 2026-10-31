+++
type = "research"
mode = "afk"
status = "open"
blocked_by = []
repos = ["homelab"]
+++
# How relais on the laptop reaches the cluster API off-LAN

## Question

Where the laptop's kubeconfig or Kubernetes API access comes from is not documented. Look at the mTLS client certificate (duro-issued), the Headscale tailnet and its subnet router (`apps/tailscale-router`, `apps/headscale`), and what identity relais would use.
