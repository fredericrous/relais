---
status: done
branch: docs/homelab-offload-rechart
repos: [relais]
adrs: [decisions:ADR-0041, decisions:ADR-0051]
---
# Re-chart the homelab offload after verification moved into amont

## Review panel

👉 **Decide:** none — approve if charting t20–t24 before building is right, now that verification runs through amont.
📍 relais · the first-slice plan is superseded (it was never built) · next: re-chart the map, research t22/t23. Panel: backend (opus-5-5).
**Changed by review:** exact frontier sets; a t24 live-test ticket now blocks t02; t15 keeps t13; t07/t14 are marked superseded.
📄 Full reviews: [2026-11-01-rechart-the-homelab-offload.reviews.md](2026-11-01-rechart-the-homelab-offload.reviews.md)
**Verdicts:** backend approve-with-changes twice. 3 carried into implementation: t20 in t22's table row, the `t20-stamp-platform-rule` slug, and Decisions so far lines for the 5 closed tickets.

## Context

The first-slice plan, "relais heavy checks run on the homelab", was reviewed
by eight seats and Codex. It went stale before it was approved:

- **amont runs the checks now.** relais v0.44–0.46 run every check as
  `amont run --stamp <gate>` (decisions "one verdict per tree", relais #370).
- **Stamps are per platform.** amont's stamps are keyed by platform
  (`x86_64-macos`), and ADR-0041 makes macOS primary.
- **The profile shrank.** msrv and audit moved to CI (#363).
- **Timings are recorded.** The ledger now records per-check timings
  (#367).

The plan placed the remote executor inside relais's spawn-and-wait. That would
run `amont` inside the pod, but amont owns gates, stamps and their signing
(ADR-0051). And a verdict from a Linux pod is not the macOS stamp the
laptop's push asks for.

The person decided on 2026-11-01:
- **Stamp platform:** a check run in a Linux pod may satisfy the laptop's
  commit and push gate **per gate**. Each gate declares whether it is
  platform-independent; macOS-sensitive gates stay local.
- **Next step:** chart it first. Settle the new questions as tickets before
  any plan.

## What this change writes

All of it lives in `relais/docs/programmes/homelab-offload/`, plus one plan
record. Branch `docs/homelab-offload-rechart`.

**Tickets closed with Resolutions** (decisions given 2026-10-31 and
2026-11-01):
- **t19, wk-3:** yes, quoting the person.
- **t01, native dispatch:**
  - Amended only when worker pods land (t02), by a relais ADR then.
  - A pod worker is relais's own worker: its router picks the model, and
    ADR-0001 governs only the session's model.
  - Decided as staff engineer, as the person delegated.
- **t03, credentials:**
  - Direction: a long-lived Claude Code OAuth token (`claude setup-token`)
    in Vault, mounted only into worker pods.
  - Spend is capped by relais's ledger and cap, not by litellm.
  - Gated on t06's live test, charted as a new ticket,
    t24-litellm-live-test (research / hitl), which is added to t02's
    blocked_by.
- **t09, the sandbox floor:**
  - PSA `restricted`; non-root; no privilege escalation; all capabilities
    dropped; RuntimeDefault seccomp.
  - No SA token automount, no secrets mounted, no hostPath.
  - Egress: `world`, the `vault` namespace and the `kube-apiserver` denied.
    The clusterwide `dns-visibility` allow-all keeps other in-cluster
    services reachable, and that is recorded as the real floor.
  - The loop (t11) may only tighten.
- **t20, the stamp platform rule (new):**
  - The person chose "per gate".
  - Each gate in `amont.conf` declares whether it is platform-independent.
    A stamp earned on Linux counts on the laptop only for those gates.
  - The rule is recorded as a decisions ADR amending ADR-0041, written when
    t22 is built.

**Tickets reopened or re-blocked:**
- **t18, cluster-lease owner.** The architect decided "relais owns it"
  while relais was the runner. If amont runs remote gates, the runner owns
  the lease. t18 stays open, blocked_by [t07, t21], with the architect's reasoning
  and the wire contract (Lease name, holder, TTL 60 s, epoch fence) recorded
  as notes.
- **t15, first slice.** It is now blocked by t13, t21, t22 and t23.
  - Its notes keep what still holds from the reviewed design: the runner
    pod on wk-3 (10Gi memory with request equal to limit, `relais-batch`
    priority, local-path cache with a free-space floor), the crates and
    rustup caching proxy, the egress floor, the ExecCredential kubeconfig,
    the per-check Lease with an epoch fence, the in-pod helper, and OOM and
    lost-connection handling.
  - The reviewed plan is landed as
    `docs/plans/2026-11-01-first-slice-heavy-checks.md`, `status:
    abandoned`, with its full reviews as an asset.
- **t13, laptop-load baseline.** Add that per-check timings now sit in the
  ledger (#367). The sampler still waits for the person to free laptop
  disk.

**New tickets:**

| id | name | type / mode | blocked_by | repos |
|---|---|---|---|---|
| t21-remote-runner-owner | Which tool runs a gate on the cluster: amont's `run`, or relais | grilling / hitl | t22 | amont, relais |
| t22-stamp-from-remote-run | How amont earns and signs a stamp for a tree a pod verified | research / afk | — | amont, decisions |
| t23-gate-platform-inventory | Which gates in the fleet are platform-independent, and which repos are Linux-primary | research / afk | — | amont, relais, homelab |
| t24-litellm-live-test | Does the credential path t03 chose work end to end in a pod (token, `/v1/messages`, tool use, caching, spend accounting) | research / hitl | — | homelab, relais |

- **t21** decides who owns the remote runner. ADR-0051 gives amont the
  gates, the stamps and the host slots, so amont is the likely owner, with
  relais calling `amont run` unchanged. Running remotely would also offload
  amont's own commit and push test runs. The architect seat decides, as the
  person asked for t18.
- **t22** asks how such a stamp is made:
  - the signing key stays on the laptop, and what the laptop checks before
    signing (tree sha, image digest, log sha256, per t14's contract);
    blocked by t20 (closed);
  - the stamp records the executor, the platform and the image digest;
  - `amont stamps` answers per gate and platform;
  - what pre-push accepts.
- **t23** asks, per repository:
  - which gates are platform-independent (for relais: fmt, clippy, the
    cycle checks; cargo-test is likely macOS-sensitive under ADR-0041);
  - which repositories are Linux-primary (homelab, services,
    trade-agents);
  - what share of laptop check-seconds those gates are, from the #367
    timings.

**Map edits:** mark the t14 and t07 entries in Decisions so far as
superseded pending t21, keeping t14's result contract as an input to t22.
The Destination's "relais places each run" is restated once t21 decides.

**Not yet specified:** a Rust build cache for cold pods. The pod's
persistent cache answers part of it, and it stays open until t21.

## Non-goals

- Any code or manifest. This change charts tickets and lands a plan record;
  the build waits for t21, t22 and t23.

## Code review

- relais: none — the change is Markdown under `docs/` only (the map, its
  tickets and an abandoned plan record), with no code for the profiles to
  read. implementation-review still reads the diff against this plan.

## Steps

1. Run `amont-agent board --repo relais`. Then start `worktree-task` on
   `docs/homelab-offload-rechart` from `origin/main` (refreshing the pack if
   it is behind).
2. Land the plan commit. Write the ticket changes above and the
   `abandoned` plan record with its reviews sidecar.
3. Run `relais programme check docs/programmes/homelab-offload`. It must exit
   0. Then run `relais programme frontier docs/programmes/homelab-offload
   --json`.
   - Expected ids, exactly: t13 (sampling), t22, t23 and t24. t02 stays
     off the frontier because t24 blocks it.
   - frontier exits 0 even when it lists nothing, so compare the ids.
4. Research t22 and t23 now, as relais `inspect` runs. If the disk is still
   under the floor, use Explore agents, and record which path each took.
   Write their Resolutions.
5. Commit with explicit paths, run the implementation review, land, open the
   PR, and merge it on green. Tear down.

## Verification

- `relais programme check docs/programmes/homelab-offload` exits 0.
- `frontier --json` after the research: exactly t13, t21 and t24 (t24 is
  hitl and stays open), compared by id.
- The PR is merged, and `origin/main` holds the map and the abandoned plan
  record.

## Outcome

| Check | Expected | Actual |
|---|---|---|
| `relais programme check docs/programmes/homelab-offload` | exit 0 | exit 0, after every edit |
| frontier at step 3 | exactly t13, t22, t23, t24 | exactly those |
| frontier after research | t13, t21, t24 | t13, t21, t24 **and t25**, a new ticket the t22 research made precise (where the pod's signing key lives) |

## Decision log

- 2026-11-01: the laptop disk had been freed (42 GB). The t22 relais
  `inspect` run in amont still ended `baseline_unrunnable`: amont's `make`
  hit its 3600 s limit under laptop load, which is this programme's own
  problem. t22 and t23 were researched by Explore agents. t23 spans many
  repositories, and read hints must stay in one repo.
- t22 found that amont ADR-0015 already decides "a remote executor must
  never stamp". The vehicle is a signed attestation. Stamps carry no
  platform; attestations do.
- t23: the timing data covers one day only, so the share of laptop load is
  left to t13. Labels in `check_timings` need gate names.
- Carried from review and done: the `t20-stamp-platform-rule` slug, t22
  blocked by t20, and Decisions so far lines for t01, t03, t09, t19 and t20.

<!-- panel: repos=relais reviewers=backend body-sha=12f073381d4c -->
