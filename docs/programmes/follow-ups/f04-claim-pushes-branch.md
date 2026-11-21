+++
type = "task"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais"]
+++
# A claim pushes its programme branch

## Question

Should `relais programme claim` push the `prog/<slug>/<id>` branch when it
claims, so `relais programme frontier` on another node sees the ticket in
flight? Claims are files on one host today.

Filed from the inbox (2026-11-19, from homelab agents-many-nodes an-t03).
The same line notes that an-t03 answers homelab-offload t17 (the laptop
sleeps): a session hands off resumable state, never its transcript.

## Look at

`crates/relais/src/programme/claim/` (where a claim is written, per host),
the frontier's "held" test (a `prog/<slug>/<id>` branch or an unexpired
claim), and homelab's `agents-many-nodes` map, ticket an-t03.

## Answer looks like

A decision: push at claim time, yes or no, and what happens when the push
fails or the remote is down. If yes, the place in `claim` that pushes, what
`frontier` reads on the remote, and a test with two clones.

## Resolution

2026-11-21: **no.** `relais programme claim` does not push the
`prog/<slug>/<id>` branch. Claims stay files on one host until programme
claims move to the primary (homelab agents-many-nodes, an-t02).

What was checked (relais on origin/main 5742aa07):

- `claim` writes only `<state dir>/claims/<repo-key>/<slug>/<id>.toml`
  (`programme/claim/store.rs`). It never creates the `prog/` branch; the
  session does that at the programme skill's step 3 (`worktree-task`) and
  pushes it once, at the end.
- `frontier` already counts `refs/remotes/origin/prog/<slug>/<id>` as in
  flight (`programme/load.rs`, `branch_tips`), and it fetches with
  `git fetch --prune`. A branch pushed by any means is seen on every node
  that fetches, and a deleted one stops holding there. Nothing on the read
  side needs to change.
- The branch-name gate allows `prog/<slug>/<id>` in relais and homelab, and
  a push with no new commits owes only the builtin pre-push gates. Pushing
  is possible; the answer is no on the reasons below.

Why not push at claim time:

1. **A branch has no expiry.** A claim holds for 8 h after its last
   refresh (`work`) or 2 h (`handoff`), and the session's end deletes it. A
   pushed branch holds until someone deletes it. A session that crashes or
   whose node sleeps would hold its ticket on every node indefinitely. That
   is the stale hold the 8 h rule was measured to avoid (plan
   `2026-11-19-a-session-claims-its-ticket.md`, Phase 0).
2. **Release and SessionEnd would need the network.** Matching the claim's
   lifecycle means deleting the remote branch on `release`, on SessionEnd
   and on expiry. These hooks run with tight time budgets (a 30 ms lock wait
   for the refresh pass) and must not fail when the remote is down.
3. **It conflicts with how sessions push.** Sessions push once, through
   `greengate land`, after the implementation review. A branch pushed at
   claim time, pointing at `origin/main`, would turn the session's later
   land into a push to an existing branch, and a `handoff` reservation
   from `next --claim` would publish a branch for work that has not started.
4. **The person chose the claim file over a `prog/` branch on 2026-11-19**,
   because a branch alone fails during the gap right after a handoff.
5. **No second node works programme tickets yet.** The headless session
   (an-t05) is open, and the proof (an-t09) is blocked on it. The lasting
   fix, claims on the primary, is already recorded in an-t02.

When the push or the remote fails: there is no push, so a claim never fails
or waits on the network.

Until claims move to the primary, a session that moves between nodes may
push its `prog/` branch by hand when it hands off mid-ticket (an-t09 step 1
already does). Other nodes then see the ticket held through the existing
remote-ref read.

Reopen this if a second node runs programme sessions before claims reach
the primary: then the question becomes "push at the first commit", with
the release and expiry questions above, not "push at claim".
