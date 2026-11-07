+++
type = "research"
mode = "hitl"
status = "open"
blocked_by = ["t27-cliproxy-on-homelab", "t28-oauth-account-owner"]
repos = ["homelab", "relais"]
+++
# Does a pod reach Claude, Codex and Qwen end to end through the homelab CLIProxyAPI

## Question

Test t03's amended direction live, in a pod in the `relais` namespace, before
any worker pod is built. The pod calls the homelab CLIProxyAPI with a proxy
access key and nothing else:

- Claude Code in the pod, with `ANTHROPIC_BASE_URL` set to the proxy:
  `/v1/messages` streaming and a tool-use round trip;
- the same for one Codex and one Qwen model;
- prompt caching (a second identical request shows
  `cache_read_input_tokens` > 0);
- how spend shows up in relais's ledger, and in the proxy's own usage view;
- what a quota or rate breach on one OAuth account returns, and whether the
  proxy moves to another account.

It is hitl: the person signs the OAuth accounts in through the management
center.

## Look at

- t27's Resolution: the proxy's address, how a key reaches the pod, and the
  egress rule.
- t28's Resolution: which accounts the homelab proxy holds.
- `kubernetes/homelab/apps/litellm/` and `docs/operations/litellm-tailnet-api.md`
  in homelab: how the Mac's proxy already talks to Qwen.
- t05 (namespace quota) and t09 (pod floor).

## Answer looks like

One row per check above, with the request, the expected result, the actual
result and the evidence (a response excerpt, a ledger row). Then one line:
the direction holds, or it fails on a named check.
