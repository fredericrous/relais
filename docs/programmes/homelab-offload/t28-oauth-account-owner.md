+++
type = "grilling"
mode = "hitl"
status = "closed"
blocked_by = ["t27-cliproxy-on-homelab"]
repos = ["homelab", "relais"]
+++
# Which proxy owns each OAuth account once the homelab runs one

## Question

Two proxies that refresh the same OAuth account can invalidate each other's
token. Once the homelab runs CLIProxyAPI, which proxy owns each Claude and
Codex account?

- the Mac points at the homelab proxy and stops holding accounts;
- the accounts are split, some on the Mac and some on the homelab;
- both keep their own, with no account shared between them.

## Resolution

The person, 2026-11-07: **the Mac uses the homelab proxy.** The homelab
CLIProxyAPI holds every OAuth account: the three Claude accounts, the
`@ext.apicil.com` work account included, and the Codex account. The Mac's
own proxy stops. Its Claude Code points `ANTHROPIC_BASE_URL` at the homelab
proxy, with the Mac's access key.

- **One owner:** each account is refreshed by one proxy, so no token race.
  Usage shows in one view (t27).
- **Cutover order** (the deploy plan): stop the Mac's proxy, then move the
  OAuth files into the PVC or sign in again through the panel, then start
  the homelab proxy. The two must never refresh the same account at the
  same time.
- **The cost:** the Mac's Claude Code now needs the cluster reachable from
  wherever the Mac is. t12 left the off-LAN route unproven; t30 tests it,
  before the cutover.
