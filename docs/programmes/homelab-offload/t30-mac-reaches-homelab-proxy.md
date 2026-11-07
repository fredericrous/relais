+++
type = "research"
mode = "hitl"
status = "open"
blocked_by = ["t27-cliproxy-on-homelab"]
repos = ["homelab"]
+++
# Does the Mac reach the homelab proxy from anywhere, before the cutover

## Question

After t28, the Mac's own Claude Code goes through the homelab CLIProxyAPI.
Does the Mac reach it on the LAN and off it (Headscale), with TLS and the
Mac's access key? What latency does a streamed `/v1/messages` call add over
today's 127.0.0.1, and what does the Mac see when the cluster is down? It is
hitl: the off-LAN leg needs the person away from home.

## Look at

- t12's Resolution: the macOS Tailscale build installed no route for
  192.168.1.0/24, and the live test is open.
- `docs/operations/litellm-tailnet-api.md` in homelab: how the Mac already
  reaches `llm.daddyshome.fr` over the tailnet.
- t27's Resolution: the proxy has no HTTPRoute yet, so this ticket decides
  how the Mac reaches it (an internal route like LiteLLM's).

## Answer looks like

One row per leg (LAN, off-LAN, cluster down): the command, the expected
result, the actual result, and the time to the first token for a streamed
call against 127.0.0.1 today. Then one line: the cutover is safe, or it waits
on a named fix.
