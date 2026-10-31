+++
type = "research"
mode = "afk"
status = "open"
blocked_by = []
repos = ["homelab"]
+++
# Can litellm per-key budgets cap Claude spend, and does Claude Code run through it

## Question

The homelab runs litellm with per-consumer virtual keys (`forgejo-runners/eval-litellm-key.yaml`, `kubeopencode/litellm-key.yaml`). Can a key carry a hard spend or token budget per day and per run, and can Claude Code (as a worker) use litellm as its Anthropic endpoint with the features relais needs?
