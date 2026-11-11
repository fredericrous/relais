+++
type = "grilling"
mode = "hitl"
status = "open"
blocked_by = ["t01-native-dispatch-amendment", "t03-credentials-billing", "t04-worktree-transport", "t05-cluster-capacity", "t12-laptop-cluster-path", "t24-litellm-live-test"]
repos = ["relais", "homelab"]
+++
# What launches a worker in a pod

## Question

Options: revive a headless `Backend` implementation (`crates/relais/src/backend.rs:537`, today `NativeBackend` and `MockBackend`) that runs in the pod; reuse the homelab's KubeOpenCode `AgentTemplate` (`homelab/kubernetes/homelab/apps/kubeopencode/`: ephemeral agent pods, litellm key, Forgejo token, Cilium default-deny egress; it runs OpenCode, not Claude Code); or run Claude Code plus the relais plugin inside the pod, or in a Coder workspace on wk-3. Also settle whether the plugin's interactive review and decide steps work when the worker is remote.

## Update 2026-11-11

- **The launcher seam moved.** It was cited as `backend.rs:487`. On main
  (v0.55.0) `Harness` is at `backend.rs:537` and `Backend` at `:550`.
- **A fourth option exists: a Coder workspace node.** homelab already runs
  Coder (namespace `code-server`). A session there is a native dispatch, so
  t01's "every dispatch is native" needs no amendment. It overlaps the
  programme agents-many-nodes (homelab repo), tickets t04 and t05 there.
- **What a worker needs from relais today** (review of `native.rs`,
  `adapter/native.rs`, `admission/native.rs`): a run starts only from a live
  Claude Code session with the relais plugin loaded (`RELAIS_HOST=claude-code-mod`
  and `--protocol`, `native.rs:99-108`), a plugin hello every 30 s (a lapse of
  60 s ends the attempt as `mod_gone`), and a worktree path the session can
  reach. Usage is read from local session transcripts. So a worker in a pod
  needs a new launcher, not only a pod.

## Look at

- t03's Resolution and t24 (how a pod reaches models).
- `crates/relais/src/backend.rs:537-560` (the seam) and
  `crates/relais/src/adapter/native.rs`.
- `claude-plugin/hooks/register.ts`, `runs.ts` and `callbacks.ts`.
- The agents-many-nodes tickets t04 and t05 in the homelab repo.

## Answer looks like

One row per option (headless `Backend`, KubeOpenCode, Claude Code plus the
plugin in a pod, Coder workspace): what runs, how a run starts, how review and
decide work with a remote worker, where usage is booked, and the work to build
it. Then one line: the option the first remote worker uses.
