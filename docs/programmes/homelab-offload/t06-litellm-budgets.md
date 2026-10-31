+++
type = "research"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["homelab"]
+++
# Can litellm per-key budgets cap Claude spend, and does Claude Code run through it

## Question

The homelab runs litellm with per-consumer virtual keys (`forgejo-runners/eval-litellm-key.yaml`, `kubeopencode/litellm-key.yaml`). Can a key carry a hard spend or token budget per day and per run, and can Claude Code (as a worker) use litellm as its Anthropic endpoint with the features relais needs?

## Resolution

No, not today, and Claude Code cannot use this litellm as its Anthropic base yet
(relais inspect run `run-65f24c2031f42-7dc5`, 2026-10-31; nothing tested live).

- litellm `v1.83.14-stable` (`homelab/kubernetes/homelab/apps/litellm/`), keys
  minted by `key-bootstrap-script.yaml` into Vault `dynamic/data/litellm/<c>`.
  No key sets `max_budget`, `budget_duration`, `tpm_limit` or `rpm_limit`; the
  only per-key limit is `max_parallel_requests: 1` for `cli-proxy-api`.
- Per-day caps exist only as dollar `max_budget` + `budget_duration` on a key
  token. A key rotation starts a new token at zero, so a cap spanning rotations
  needs a team or user. Local qwen likely costs $0 unless costs are set.
- Per-run caps need a key minted per run, which needs the master key; an
  in-cluster team-scoped minter keeps it in the cluster.
- Claude Code through litellm: there is no Anthropic model entry, the private
  route serves only `/v1/models` and `/v1/chat/completions` (not
  `/v1/messages`), and an Envoy policy caps parallel requests at 1. Today Claude
  Code on the tailnet points at CLIProxyAPI instead
  (`docs/operations/litellm-tailnet-api.md:136-158`).
- `num_retries: 3` multiplies spend under 429.
- Needs a live test before t03 decides: budget enforcement and breach status,
  `/v1/messages` SSE and tool use, `anthropic-beta` forwarding and prompt
  caching under `drop_params: true`, spend accounting, and API versus
  subscription billing.
- Side findings for the homelab: the `eval-litellm-key.yaml` comment claims a
  spend limit that does not exist; ADR-0007 "remote fallback" is not
  configured.
