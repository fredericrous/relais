+++
type = "task"
mode = "afk"
status = "open"
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
