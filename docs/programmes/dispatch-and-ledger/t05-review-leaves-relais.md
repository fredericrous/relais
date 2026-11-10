+++
type = "grilling"
mode = "hitl"
status = "closed"
blocked_by = ["t01-verdict-consumers"]
repos = ["relais"]
+++
# Where review happens once relais's reviewer is removed

## Question

relais's semantic reviewer goes; a relais run inside a planned
worktree-task gets implementation-review at F4b. What reviews a relais run
outside one (a one-sentence change, a run in a repository without the
workflow skills): nothing, an opt-in reviewer, or the reviewer kept only
there? Two consumers found in t01 must be answered too: the inspect report
review is the only verification an `inspect` run has, and review is forced
when a candidate edits test or verification inputs, the guard against a
worker weakening the tests.

## Resolution

The person, 2026-11-10: **"Opt-in reviewer"**, chosen over keeping two
narrow guards and over removing all review.

- **Default:** relais runs no semantic reviewer. A change run is accepted on
  its checks, and review happens at implementation-review (F4b) when the
  change ships through worktree-task.
- **Opt-in:** a contract with `review: required` gets the reviewer, and
  with it the two guards t01 found. An `inspect` report is judged by the
  report review, and a candidate that edits test or verification inputs is
  reviewed.
- **What it costs:** an `inspect` run that does not opt in has no
  verification beyond its report existing, and a worker that weakens a test
  in a run that does not opt in is caught only by F4b and the push gate. The
  ADR (t07) states both.
- **Build consequences:** `review_required`'s floors and the
  edited-test-inputs trigger fire only under `review: required`, and
  `runner/review/*` stays as the opt-in path. The
  default-off switch removes most of the review spend without deleting the
  code. Deleting it is a later choice, once the opt-in rate is known.
