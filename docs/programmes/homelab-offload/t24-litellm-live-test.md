+++
type = "research"
mode = "hitl"
status = "open"
blocked_by = []
repos = ["homelab", "relais"]
+++
# Does the credential path t03 chose work end to end in a pod

## Question

Test t03's direction live, in a pod, before any worker pod is built:

- a Claude Code OAuth token from `claude setup-token`;
- `/v1/messages` streaming and a tool-use round trip;
- prompt caching (a second identical request shows
  `cache_read_input_tokens` > 0);
- how spend shows up in relais's ledger;
- what a budget or rate breach returns.

It is hitl: the person mints the token.
