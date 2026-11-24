+++
type = "research"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
+++
# What a timed-out worker's wall time waits on

## Question

In the 32 wall-timeout transcripts that [f15](f15-loop-breaker-wider-fingerprint.md)
read, 78% of the wall time was spent inside tool calls, and no transcript
had more than 140 calls. Workers spend that time waiting on long test
suites, builds and polls of background jobs. One clippy ran for about
35 min on a loaded host.

How does that waiting split? The candidates are:

- the suite's own length;
- host contention, from other relais runs or gates at the same time;
- a worker polling the wrong file or waiting past a job that has already
  finished.

The answer decides where to act. It could be relais's attempt wall time, or
slot admission for a worker's own test runs (see `greengate slots`). It
could also be worker guidance on waiting, or the laptop offload
(homelab-offload t13).

## Look at

- The f15 Resolution: how to match each timed-out dispatch to its
  transcript (session id and start time), and the per-run table.
- Each transcript's longest calls, with their timestamps, against
  `greengate slots` and `~/.local/state/relais/coordinator.log` for the same
  window.
- `docs/programmes/` in relais for homelab-offload t13, the laptop load
  baseline.

## Answer looks like

A table over the same runs: wall time per class (the suite itself, waiting
for a slot or the host, polling, the model's own turns), the slots that
were busy at the time, and one decision about where to act.
