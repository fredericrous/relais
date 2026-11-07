+++
type = "grilling"
mode = "hitl"
status = "open"
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
