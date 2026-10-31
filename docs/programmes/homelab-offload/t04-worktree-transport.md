+++
type = "research"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
+++
# How a worktree gets into a pod and a patch comes back

## Question

Today the owned worktree is local only (`crates/relais/src/workspace/`). Options: a Forgejo push, a git bundle, or an rsync; not every repo is on Forgejo. Research how an immutable input tree and run/attempt identity bind the remote workspace, the returned patch and the verification evidence, and how local edits made during remote execution are detected before a result is applied. t14 researches the matching result contract at the same time: whichever Resolution is written second aligns with the first.
