+++
type = "research"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["homelab"]
+++
# How relais on the laptop reaches the cluster API off-LAN

## Question

Where the laptop's kubeconfig or Kubernetes API access comes from is not documented. Look at the mTLS client certificate (duro-issued), the Headscale tailnet and its subnet router (`apps/tailscale-router`, `apps/headscale`), and what identity relais would use.

## Resolution

Explore research, with a read-only `kubectl config` check, 2026-10-31. The
relais inspect run was blocked by low disk.

**relais reaches `https://192.168.1.65:6443` (the control-plane VIP) on the
LAN, and the same address off the LAN through the existing Headscale subnet
route. It authenticates as a scoped ServiceAccount, never as admin.**

- **API endpoint.**
  - The VIP is 192.168.1.65:6443 (`docs/operations/add-control-plane.md:191-199`),
    in front of control planes .67, .75 and .76
    (`bootstrap/configs/homelab.yaml:82-84`).
  - No gateway route fronts it, and the public `:6443` is dead behind CGNAT.
    The Talos API is :50000 on the nodes.
  - Today's kubeconfig (`admin@homelab`) is an admin client certificate.
- **Off the LAN.**
  - `apps/tailscale-router/deployment.yaml:29-30` advertises
    `192.168.1.0/24` via `headscale.daddyshome.fr`, with SNAT.
  - The litellm tailnet API already relies on that route.
  - Fronting the API with the Envoy mTLS gateway has no precedent and is
    rejected.
- **Identity.**
  - A ServiceAccount `relais/relais-coordinator` with a bound token (24 h or
    less), re-minted by a launchd job.
  - The token is stored in the login Keychain and read by a kubeconfig `exec`
    plugin (`security find-generic-password`), following the NAS break-glass
    precedent.
  - Client certificates and OIDC are rejected: Talos signs only admin
    certificates, and no apiserver OIDC flags are set.
- **Role in `relais`.**
  - jobs: create, get, list, watch, delete;
  - pods: get, list, watch, delete;
  - `pods/log`: get;
  - `coordination.k8s.io` leases: full;
  - events: read, if needed.

  The namespace's quota and PSA labels are what bound it (see t05).
- **Open, for t02, t09 and t17:**
  - Live-test off-LAN `curl -k https://192.168.1.65:6443/version`, plus the
    Headscale ACL for tcp/6443.
  - The router runs on the cluster it fronts: what happens when it is down?
  - Same-user worker agents can read the Keychain item, so real isolation
    needs a separate macOS user or another boundary. This feeds the t09
    floor.
  - Which longer-lived credential mints the token.
  - Whether `--service-account-issuer` and the token audience are set.
- **Known blocker.** The memory note `reference_forgejo_offlan_access_matrix`
  records that macOS installs no route for `192.168.1.0/24` from the Tailscale
  GUI build, even with the route approved and accepted. The off-LAN path above
  is unproven until that is fixed.
- **Side finding.** The memory note claiming kubectl works through the public
  endpoint is stale.
