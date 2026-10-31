---
status: active
branch: docs/homelab-offload-chart
repos: [relais]
adrs: [ADR-0001, ADR-0003, ADR-0009, ADR-0010, decisions:ADR-0045, decisions:ADR-0051]
---
# Chart the programme: relais runs on the homelab

## Review panel

👉 **Decide:** confirm the Destination line. Approving the plan means you bet that charting 17 tickets and closing 8 research tickets now is worth one PR.
📍 relais · map not written yet · next: write the map and fan out 8 research runs. Panel: architect, backend, rust, po, tui, unix (opus-5-5), codex.
**Changed by review:** added tickets t13–t17 (laptop-load baseline, remote heavy checks, first slice, visibility, failure/fallback); the sandbox loop needs its own ADR-0003-style record; the branch is `docs/` because amont rejects `prog/`.
📄 Full reviews: [2026-10-31-relais-runs-on-the-homelab.reviews.md](2026-10-31-relais-runs-on-the-homelab.reviews.md)
**Verdicts:** 7 approve-with-changes in round 1 including codex, 1 in round 2, 1 approve on the final body. 2 notes carried into implementation.

## Context

The laptop's CPU is always under heavy load. relais already has slots
(admission `CapGroup`s, plus amont host slots from #343). It also once had a
worker sandbox, removed in #173 / 07ed5c5f. The person wants relais runs to
move to the homelab Kubernetes cluster, under usage control, with sandboxing
that improves itself.

This is multi-session work, and several of its decisions cannot be stated
yet. So it is charted as a **programme map** (`relais:programme` skill). This
plan charts the map and fans out the research. It does not design the build.
Each resolved ticket later becomes an ordinary plan.

What the person answered (2026-10-31):
- **What moves:** both whole worker runs and heavy checks. relais chooses per
  task.
- **Usage control:** all four kinds — cluster CPU/RAM caps, concurrency
  slots, a token/cost budget, and a laptop-load threshold.
- **Sandbox:** improves itself fully automatically.
- **End point:** running in production.

Facts found during orientation:
- **Workers today.** Every dispatch is native: a Claude Code subagent spawned
  by the relais plugin inside the person's interactive session
  (`adapter/native.rs`, `admission/native.rs`).
  - `claude -p` and `ClaudeBackend::launch` were deleted on purpose in #173
    (`docs/plans/2026-10-06-all-native-mod.md`).
  - `Backend`/`Harness` (`backend.rs:487`) is the only launcher seam.
- **The old sandbox** (`git show 07ed5c5f^:crates/relais/src/sandbox/`) was
  Claude Code's own `sandbox` settings block.
  - Its parts: a credential floor (`floor.rs`), a probe and evaluator with a
    verification store, a dispatch gate, and denial recording
    (`denials.rs`).
  - On Linux it needed `bwrap` and `socat`.
  - Its "self-improving" loop was manual: people edited the rules text after
    reading denials (#164, #170). No automatic loop has existed.
- **Slots.** `CapGroup::Remote` already means model-API work, so the name
  collides with remote execution.
  - Host slots are local flock files (`amont-slots`), and the coordinator
    listens on a local Unix socket.
  - ADR-0051 makes amont the owner of host slots.
- **Decisions that bear on this:**
  - ADR-0001 (`routing.session-model`): relais "never re-routes its own
    workers".
  - ADR-0009 (`workspace.local-reuse`): a CI or server run keeps full
    isolation.
  - ADR-0010 (`execution.resource-budgets`): the four budgets.
  - A remote launcher amends the all-native choice, so it must go through a
    decision, not an assumption.
- **The cluster:**
  - wk-4 is down, and wk-3 is reserved for AI work.
  - The Forgejo `ci` pool was cut to 2 because of disk contention with the
    Ceph OSDs.
  - CephFS is about 46x slower on small files, so the scratch space needs
    node-local ephemeral storage.
  - No gVisor or Kata. Tetragon runs detect-only, and Cilium default-deny
    egress templates exist (`apps/kubeopencode/cilium-egress.yaml`).
  - KubeOpenCode already runs ephemeral agent pods: `AgentTemplate`, a
    litellm key and a Forgejo token.
  - Package mirrors: verdaccio, goproxy and zot. No cargo mirror and no
    sccache.
  - Where the laptop gets its kubeconfig or API access is not documented.

## What gets written (after approval)

**Where the map lives:** `relais/docs/programmes/homelab-offload/map.md`, with
`repos = ["relais", "homelab", "amont"]`.

**Destination (for the person to confirm):** relais places each run, whether
a whole worker run or heavy checks, on the laptop or the homelab cluster.
Cluster runs stay under CPU, RAM, slot and token caps, and their sandbox
adapts automatically above a fixed floor. The laptop's load drops by the
target that the laptop-load baseline ticket (t13) sets, and this runs in
production.

**Notes:** this section carries the orientation facts above and the
decisions that bear on the work, ADR-0003 (`learning.automatic-promotion`)
included: its envelope, automatic rollback and anti-flap design is the model
for the sandbox loop, but it covers task artifacts only, so the loop needs
its own decision record. The ADR-0010 time budgets stay as they are. It names
the skills every session reads: `worktree-task`, plus `forgejo-recipes` for
the homelab.

### Tickets

In the frontmatter, `blocked_by` names full file stems (for example
`t01-native-dispatch-amendment`), and each ticket names its `repos`.

| id | name | type / mode | blocked_by | repos |
|---|---|---|---|---|
| t01-native-dispatch-amendment | Is "every dispatch native" amended for remote runs, how is that recorded, and which model a pod worker gets under ADR-0001 | grilling / hitl | — | relais |
| t02-remote-launcher | What launches a worker in a pod | grilling / hitl | t01, t03, t04, t05, t12 | relais, homelab |
| t03-credentials-billing | Which credential a pod uses, and where the token budget is enforced | grilling / hitl | t06 | relais, homelab |
| t04-worktree-transport | How a worktree gets into a pod and a patch comes back | research / afk | — | relais |
| t05-cluster-capacity | What capacity relais gets on the cluster without starving the services or Forgejo runners | research / afk | — | homelab |
| t06-litellm-budgets | Can litellm per-key budgets cap Claude spend, and does Claude Code run through it | research / afk | — | homelab |
| t07-slot-model | How remote capacity becomes a slot kind | research / afk | — | relais, amont |
| t08-placement-policy | Which signal and rules choose laptop or cluster for each run | grilling / hitl | t01, t05, t07, t13 | relais |
| t09-sandbox-floor | What the automatic loop may never widen | grilling / hitl | t10 | relais, homelab |
| t10-pod-denial-signals | Which denial signals a pod produces, and how they map onto the old `denials.rs` | research / afk | — | relais, homelab |
| t11-auto-adjust-loop | How the sandbox policy adapts from denials by itself | grilling / hitl | t02, t09, t10 | relais, homelab |
| t12-laptop-cluster-path | How relais on the laptop reaches the cluster API off-LAN | research / afk | — | homelab |
| t13-laptop-load-baseline | What loads the laptop today, and what target counts as done | research / afk | — | relais |
| t14-remote-heavy-checks | How a heavy check (no model) runs in a pod and returns its outcome and logs | research / afk | — | relais, amont |
| t15-first-slice | Which slice ships first and alone | grilling / hitl | t05, t13, t14 | relais |
| t16-remote-run-visibility | How a pod's events, logs and progress reach status and the progress view on the laptop | grilling / hitl | t02, t04 | relais |
| t17-remote-failure-fallback | What happens and what the person sees when a remote run cannot start or dies | grilling / hitl | t02, t04, t08, t12 | relais |

What each ticket covers:
- **t02, remote launcher.** Options: revive a headless `Backend` in the
  pod, reuse KubeOpenCode `AgentTemplate`, or run Claude Code plus the
  relais plugin in the pod. It also settles whether the plugin's review and
  decide steps work when the worker is remote.
- **t03, credentials and billing.** The choice is a subscription OAuth token
  or an API key through litellm.
- **t04, worktree transport.** Options: a Forgejo push, a git bundle, or an
  rsync. Not every repo is on Forgejo. Research how an immutable input tree
  and run/attempt identity bind the remote workspace, returned patch and
  verification evidence. Define how local edits made during remote execution
  are detected before applying a result.
- **t05, cluster capacity.** It covers the nodes, a namespace ResourceQuota,
  a PriorityClass relative to the runners, and node-local ephemeral storage.
- **t07, slot model.** Two questions: the name collision with
  `CapGroup::Remote`, and where remote capacity is counted (the cluster
  needs its own source of truth). It must stay inside the amont slot
  ownership set by ADR-0051. A flock slot is freed when its holder dies, but
  a remote slot is not, so this ticket also settles a lease with a TTL and a
  heartbeat.
- **t08, placement policy.** It also covers showing the placement: status
  prints laptop or cluster, the node, and the reason, as text rather than by
  colour alone.
- **t09, sandbox floor.** Candidates: the credential deny list, the egress
  ceiling, no privilege escalation, and the restricted PSA level.
- **t10, pod denial signals.** Candidate sources: Hubble drops, Tetragon
  events, seccomp/AppArmor, and gVisor. Also check whether `bwrap` inside a
  pod is even possible under restricted PSA.
- **t11, automatic loop.** The person chose fully automatic. This ticket
  settles the mechanism, the rollback, and the evidence threshold. Its
  Resolution is a new decision record modelled on ADR-0003: a granted
  envelope, automatic rollback, anti-flap, and it never edits the floor that
  t09 sets.
- **t13, laptop-load baseline.** Measure the 1/5/15-minute load average and
  CPU-seconds per day, split between heavy checks and Claude Code processes,
  over 7 days and before anything moves. Also record heavy-slot occupancy.
- **t14, remote heavy checks.** Today the coordinator holds a lease by local
  process-group id, and host slots are local flocks (`verify/heavy.rs`). A
  pod has neither. Heavy checks need no model credentials and no
  native-dispatch amendment. Research a result contract binding each verdict
  to the immutable tree, run/attempt, check profile and actual execution
  environment: OS/architecture, toolchain and container image digest.
  Determine how base and candidate checks remain comparable, how cache keys
  use remote provenance, and which platform-specific checks must remain
  local. Coordinate this contract with t04's transport research.
- **t15, first slice.** The candidate is heavy checks first, decided against
  the t13 baseline.
- **t16, remote run visibility.** Today `quiet::watch` and `native status`
  read a local run directory (`quiet.rs:445`). Options: mirror
  `events.jsonl` back, stream through the coordinator, or have status ask
  the pod. Alongside t02, t03 and t14, explicitly preserve decisions
  ADR-0045's holdout boundary: determine where acceptance executes, which
  identity can fetch holdouts, and how worker/reviewer inputs exclude their
  sources and logs. Distinguish person-readable evidence from the permitted
  worker feedback (criterion statement, failing test ids and pass/fail).
  t14's research must report this boundary before its Resolution closes.
- **t17, failure and fallback.** Cases:
  - the cluster is unreachable, a quota is hit, or the budget is spent;
  - the pod is evicted or OOM-killed;
  - a patch is lost;
  - the laptop sleeps or goes offline;
  - SIGINT/SIGTERM, or the session ends, and the pod must be cancelled;
  - an orphaned pod has to be reaped.

  It also maps the pod's exit status to a relais outcome, and keeps a local
  fallback safe to retry. Every message says what happened and why, gives
  the exact next command, and fits in 80 columns.

**Not yet specified (fog):**
- A Rust build cache for cold pods (sccache, a cargo mirror).

**Out of scope:** none yet.

## Steps

1. Run `amont-agent board --repo relais`. Then start a `worktree-task` on the
   branch `docs/homelab-offload-chart` from `origin/main`: amont's push gate
   rejects a `prog/` prefix (`amont-runtime/src/vocabulary.rs:81`). The map's
   Notes record that the programme skill's `prog/<slug>/<id>` convention
   conflicts with that gate; this chart changes neither.
2. Write `map.md` and the 17 ticket files, with the frontmatter that the
   `work.programme-map-layout` rule requires.
3. Run `relais programme check docs/programmes/homelab-offload`. It must exit
   0. Then run `relais programme frontier docs/programmes/homelab-offload --json`.
   The ids must be exactly t01, t04, t05, t06, t07, t10, t12, t13 and t14.
   frontier exits 0 even when it lists nothing, so compare the ids, not the
   exit code.
4. Fan out the eight research tickets (t04, t05, t06, t07, t10, t12, t13,
   t14) as relais `inspect` runs in parallel, each in the repo its `repos`
   column names, one line per path taken. Wait
   for all of them. Then write each Resolution, set `status = "closed"`, add
   the Decisions so far lines, and turn any fog the answers made precise into
   tickets.
   - A run that fails or returns no data leaves its ticket open with a note,
     and the PR merges with that ticket still open.
   - t13 needs 7 days of samples. Its run records what already exists
     (ledger timings, heavy-slot waits) and writes the sampler spec into
     the ticket: the command, the output path and the stop condition. Starting
     the sampler is the first step of t13's next session, not of this plan.
     The ticket closes when the window ends.
5. Run `check` again until it exits 0. Commit `map.md` and the tickets as
   explicit paths, open a PR, and merge it when green (`merge-when-green`).
6. The grilling tickets run in later sessions, one per session, starting
   with t15 (the first slice) or t01.

## Verification

- `relais programme check docs/programmes/homelab-offload` exits 0.
- At merge (t13 still sampling, other research closed),
  `relais programme frontier docs/programmes/homelab-offload --json` lists
  exactly t01, t03, t09 and t13, plus any research ticket left open by a
  failed run. When t13 closes, it lists exactly t01, t03, t09 and t15.
  (Superseded by the research, see the Decision log: t01, t03, t09, t13, t18
  and t19 at merge; once t13 closes, t01, t03, t09, t18 and t19, since t15
  also waits on t19.)
- `relais programme list` shows the map from `~/Developer/Perso`.
- The PR is merged, and `origin/main` contains the map.

## Outcome

Verification, observed before the push:

| Check | Expected | Actual |
|---|---|---|
| `relais programme check docs/programmes/homelab-offload` | exit 0 | exit 0 |
| frontier before research | t01, t04, t05, t06, t07, t10, t12, t13, t14 | exactly those 9 |
| frontier at merge | t01, t03, t09, t13 (+ any failed research) | t01, t03, t09, t13 + t18, t19 (new tickets the research made precise) |
| `relais programme list` | shows the map | listed: open 12, frontier 6, closed 7 |

## Decision log

- 2026-10-31: relais `inspect` runs were refused twice on contract shape:
  acceptance must be `{statement, evidence: {kind: "llm_review"}}`, and read
  hints must be inside the run's own repository (`../homelab` was refused).
- 2026-10-31: 7 of 8 inspect runs ended `blocked (disk_low)`: 4.3–4.8 GB free
  against relais's 8 GiB floor. Only t06 ran through relais (¢19). The other
  research ran as Explore agents, which the programme skill allows for
  research. Freeing disk is the person's call.
- t13 stays open: the sampler spec is in its ticket, and starting the sampler
  is the first step of its next session.
- The vendored decisions pack was behind (d694a08 → eaacb7d). The
  worktree-task Start step refreshes it on the branch, in its own commit.
- New tickets from the research: t18 (who owns the cluster lease protocol) and
  t19 (may relais use wk-3, or wait for wk-4). t15 now waits on t19.
- t12: off-LAN reach is unproven, because the macOS Tailscale build installs
  no `192.168.1.0/24` route (memory note). The stale memory row on kubectl
  through the public endpoint was corrected.

<!-- panel: repos=relais reviewers=architect,backend,language,po,tui,unix,codex body-sha=680919108aaa -->
