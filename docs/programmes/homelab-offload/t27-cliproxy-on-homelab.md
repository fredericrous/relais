+++
type = "research"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["homelab", "relais"]
+++
# How CLIProxyAPI and its management center run on the homelab

## Question

What does it take to run CLIProxyAPI and Cli-Proxy-API-Management-Center as
homelab apps that worker pods can call?

- **Image:** an upstream image or a build of our own; pinned by digest.
- **OAuth files:** the proxy rewrites them when a token refreshes, so where
  they live (a PVC, or Vault with write-back) and how they are backed up.
- **Access keys:** one per consumer (the Mac, relais pods), from Vault
  through ESO.
- **Management:** the secret key, and whether the center is served by the
  proxy or as its own app, reachable only from the tailnet.
- **Network:** a Service for the `relais` namespace under default-deny
  egress, and the proxy's own egress to Anthropic, OpenAI and LiteLLM.
- **Usage:** whether the proxy reports usage per access key, for t03's spend
  cap.

## Look at

- `~/.cli-proxy-api/config.yaml` on the Mac: its shape only (server,
  management, `oauth.auth-dir`, access keys, openai-compatibility). Never
  copy its secrets.
- The upstream repositories: router-for-me/CLIProxyAPI (Docker image,
  config reference, management API) and
  `~/Developer/Perso/Cli-Proxy-API-Management-Center` (how it is built and
  served).
- In homelab: `kubernetes/homelab/apps/litellm/` and
  `docs/operations/litellm-tailnet-api.md` (the Mac's proxy and its LiteLLM
  key), and `apps/kubeopencode/` (the default-deny egress templates).
- In this programme: t05 (quota), t09 (floor), t10 (Hubble signals) and t12
  (how the laptop reaches the cluster).

## Answer looks like

A short deployment design for a homelab plan:

- one table row per choice above: the option picked, why, and the file or
  doc that shows it works;
- the list of Kubernetes objects (Deployment, PVC, Service, ExternalSecrets,
  CiliumNetworkPolicy, HTTPRoute) with their namespace;
- the open risks, each with the ticket that answers it.

Nothing is deployed by this ticket.

## Resolution

Explore research on Haiku, 2026-11-07. The image, the write-back and the
panel setting were spot-checked upstream. Nothing is deployed.

- **Image:** upstream `eceasy/cli-proxy-api` (Docker Hub, as in the official
  `docker-compose.yml`), pinned by digest. Our own build from the repo's
  `Dockerfile` is the fallback. The digest and Docker Hub pull limits are
  left to the deploy plan.
- **OAuth files:** one RWO PVC (`rook-ceph-block`) mounted at
  `/root/.cli-proxy-api`, one replica, `Recreate` strategy. A refresh
  rewrites the file atomically (`internal/auth/codex/token.go`,
  `WriteJSONFileAtomic` then `os.Rename`), so the storage must be writable,
  and two replicas would race. Backup is t29.
- **Access keys:** one `access.api-keys` entry per consumer (the Mac, relais
  pods), generated in Vault and rendered by ESO into the config Secret, as
  with LiteLLM's virtual keys.
- **Management:** `management.secret-key` from Vault, `allow-remote: false`,
  no HTTPRoute. The center is the proxy's own bundled panel
  (`disable-control-panel`), not a separate app. It is downloaded from
  GitHub at runtime: disable it, or allow that host (t09).
- **Network:** namespace `cliproxy`, a ClusterIP Service on 8317, and a
  CiliumNetworkPolicy allowing DNS, the envoy gateway (Qwen through
  `llm.daddyshome.fr`) and FQDNs `api.anthropic.com`, `claude.ai`,
  `platform.claude.com` and `auth.openai.com`. In `relais`, egress is
  default-deny, with only `cliproxy:8317` allowed. Patterns:
  `apps/kubeopencode/cilium-egress.yaml` and `apps/litellm/`.
- **Usage:** `GET /api-key-usage` (with `usage-statistics-enabled: true`)
  groups usage by key, in memory only, and resets on restart. relais's
  ledger stays the spend cap (t03). Whether "key" means the client key or
  the upstream account is unverified; t24 checks it.
- **Objects:** Namespace, Deployment, PVC, Service, ExternalSecret (config
  and keys), two CiliumNetworkPolicies. No HTTPRoute.
- **Next:** a homelab deploy plan, after t28 decides which accounts the
  homelab proxy holds.
