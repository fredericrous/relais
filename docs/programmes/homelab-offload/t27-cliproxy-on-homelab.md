+++
type = "research"
mode = "afk"
status = "open"
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
