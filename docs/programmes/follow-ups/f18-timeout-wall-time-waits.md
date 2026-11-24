+++
type = "research"
mode = "afk"
status = "closed"
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

## Resolution

**Act in relais's native stop detection, not on any of the four
candidates.** The largest share of a timed-out worker's wall time is
neither waiting nor work. In 12 of 31 runs the worker had already given
its final answer (`end_turn`, usually "DONE") while a background shell it
started was still running. The native adapter waits for
`$.agent.list()` to show the agent ended (`observe` in
`claude-plugin/hooks/dispatches.ts`). That listing stays `running` until
the shell ends. When the shell never ends, as with an `until grep` loop on
a marker that never comes, the dispatch is killed at its wall time. That is
**33,220 of 81,284 s (41%)**. Building it is
[f21](f21-settle-at-final-turn-with-shells.md).

**Method** (2026-11-24, run inline, not through relais: the data is
`~/.claude/projects`, the relais state directory and `/tmp`, outside any
repository). The scripts are in [f18-assets](f18-assets/):
`wall_time_split.py --json rows.json`, then `table.py`. Each timed-out
dispatch is matched to its transcript as f15 did. This gives 49 dispatches
and 31 transcripts; f15 had 50 and 32. The one left out is the reviewer in
`65e9e26e`: its dispatch timed out, but the run did not end in a timed-out
phase, which is what the script selects. Wall time runs from
`dispatch_started` to `dispatch_ended`. Each instant goes to one class:

- **after DONE**: from the worker's last transcript line to the kill;
- **suite**: a foreground test, build, lint or `slot run` call;
- **poll**: a `sleep`/`until`/`tail -f` loop, or a Monitor/TaskOutput call;
- **model**: the gaps between a result and the next assistant line;
- **other**: every other tool call.

Parallel calls are counted as a union. Without the after-DONE tail, the
total is 48,064 s, against f15's 48,365 s. `coordinator.log` holds only
election lines, so it was not used.

**Split.**

| class | s | share |
|---|---|---|
| after DONE (worker finished, dispatch not settled) | 33,220 | 41% |
| suite | 19,815 | 24% |
| poll | 12,609 | 16% |
| … polls after a finish marker was already seen (upper bound) | ≤ 2,926 | ≤ 4% |
| … empty or missing-file poll results (upper bound) | ≤ 4,465 | ≤ 5% |
| model turns | 11,239 | 14% |
| other tools, spawn | 4,400 | 5% |

**The after-DONE tail, checked.**

- All 12 tail runs had a background task alive at the end. 10 started one
  with `run_in_background`. In the other 2 (`65ef6c5b`, `65faaaa7`), a Bash
  call was moved to the background at its 120 s timeout
  (`backgroundTaskId`).
- The tail runs shared one more mark: a `cost` event at the moment of
  the last line. This was `onTurn` booking the final turn, so the plugin
  saw the end of the turn and only the stop was missing.
- `hook_journal` (11-19 to 11-24) holds 51 `SubagentStop`s of relais
  workers. All 40 that had no other task running settled within 1 s. All 9
  that had a shell running settled 124–1,108 s later, when the shell ended.
  These were successful runs, so the same cause slows runs that do not
  time out. The 2 that never settled are this ticket's `660c032d` and
  `660c4731`.
- No worker resumed on its own after a stop. 5 agents stopped twice, and
  each second stop belongs to a second ledger dispatch (a relais repair
  continue). Zero task notifications reached any of the 31 transcripts.
  `65f2a671` ended "I'll end my turn here and wait for its completion
  notice", and nothing ever woke it.

**What the other classes say.**

- *The suite's own length* (24%) is real but second. The longest are
  `66038b34` (2,041 s) and `65faaba8` (1,972 s).
- *Host contention* could not be shown to dominate. The signals are
  partial:
  - `samples.tsv` covers 11-01 to 11-17, and only 7 of the runs.
  - `waits.jsonl` covers 11-17 onward.
  - The `waiting_for_slot` counts are not normalised by time.
  - `65f0347e` reads as quiet, though f15 saw clippy at 79% CPU for about
    35 min there. That is a known false negative.

  Runs tagged as contended spend no larger share in suites (23%) than quiet
  ones (25%). One run, `660ca667`, lost 2,213 s of its 3,600 s to its own
  greengate slot wait.
- *Polling past a finished job or on the wrong file* is at most 9%. The
  worst is `65ec661c`, with 2,396 s of polls after the vitest run had
  finished.
- *One hang*: `6604f909` spent 2,336 s in a single probe command, with
  nothing running.

**The four candidates, one by one.**

- *Attempt wall time*: no. The tail fills whatever wall time is left. In
  `660c4731`, a 9,002 s wall held 8,193 s of tail.
- *Slot admission for workers' own runs*: not first. It cost one run in 31
  (2,213 s).
- *Worker guidance on waiting*: at most 9% of the time. f21's fix also
  stops the polls left behind at the end.
- *Laptop offload (homelab-offload t13)*: not supported by this data. t13
  is still open. Its sampler ran from 11-01 to 11-17.

**Per run** (seconds; "host at the time": load1 over 12 when sampled, 50 or
more `waiting_for_slot` events of other runs in the window, the run's own
greengate slot wait, or 0.5 or more other dispatches at once).

| run | worker | wall s | after DONE | suite | poll (late) | model | other | host at the time |
|---|---|---|---|---|---|---|---|---|
| `65ead843594b0` | sonnet | 1200 | 0 | 508 | 70 (69) | 585 | 38 | quiet |
| `65ebe2731332d` | sonnet | 1162 | 0 | 36 | 0 (0) | 871 | 255 | quiet |
| `65ec661cb28fa` | sonnet | 5222 | 0 | 1138 | 3369 (2396) | 680 | 35 | quiet |
| `65ee47653e91f` | opus | 79 | 0 | 60 | 0 (0) | 15 | 4 | quiet |
| `65ef580d36317` | opus | 172 | 0 | 143 | 0 (0) | 21 | 8 | 1.0 dispatches |
| `65ef6c5be313c` | sonnet | 1201 | 905 | 57 | 0 (0) | 109 | 130 | 0.9 dispatches |
| `65efb16db924f` | sonnet | 1173 | 0 | 327 | 114 (0) | 519 | 213 | quiet |
| `65efd34aed95e` | sonnet | 1200 | 0 | 926 | 0 (0) | 190 | 84 | quiet |
| `65efedd17f79f` | sonnet | 1200 | 0 | 1022 | 1 (0) | 169 | 9 | quiet |
| `65f00d23ff828` | fable | 336 | 0 | 174 | 0 (0) | 154 | 8 | 1.4 dispatches |
| `65f0347e22e71` | sonnet | 2700 | 1011 | 218 | 752 (237) | 670 | 49 | quiet |
| `65f1afb7e0d9f` | sonnet | 5241 | 2961 | 1545 | 102 (0) | 577 | 54 | 373 slot waits |
| `65f2a671ea883` | sonnet | 1200 | 767 | 142 | 0 (0) | 256 | 35 | 112 slot waits |
| `65f33d11b9afa` | sonnet | 5267 | 4728 | 404 | 0 (0) | 129 | 6 | quiet |
| `65f3ba4f1080b` | sonnet | 5120 | 3301 | 278 | 879 (0) | 606 | 56 | quiet |
| `65f3d9dbf952a` | sonnet | 5182 | 1916 | 759 | 1912 (0) | 538 | 57 | load1 56, 125 slot waits |
| `65f49d2acbbc2` | sonnet | 1201 | 0 | 616 | 0 (0) | 567 | 17 | load1 22 |
| `65f6041b8b981` | sonnet | 1200 | 0 | 644 | 91 (0) | 452 | 13 | load1 20 |
| `65faaaa725e69` | sonnet | 1200 | 880 | 113 | 0 (0) | 76 | 131 | load1 25 |
| `65faaba8f3cfe` | sonnet | 2697 | 0 | 1972 | 143 (0) | 502 | 80 | load1 23, 55 slot waits |
| `65fdf979b7cc6` | sonnet | 1200 | 0 | 1016 | 0 (0) | 161 | 24 | quiet |
| `65fe1bb588f1d` | sonnet | 2700 | 0 | 1087 | 936 (0) | 560 | 117 | quiet |
| `65fe314e75700` | sonnet | 1200 | 0 | 537 | 87 (14) | 376 | 199 | quiet |
| `6602a71dd67a4` | sonnet | 5196 | 3346 | 659 | 836 (0) | 318 | 37 | quiet |
| `66038b34f33b8` | sonnet | 2401 | 0 | 2041 | 0 (0) | 344 | 15 | quiet |
| `6604f909f8772` | sonnet | 2401 | 0 | 0 | 0 (0) | 63 | 2338 | quiet |
| `6606b1fb10437` | sonnet | 2618 | 0 | 1509 | 540 (210) | 417 | 152 | quiet |
| `6609237862af8` | sonnet | 5284 | 3879 | 485 | 395 (0) | 499 | 26 | quiet |
| `660c032dd8c5d` | sonnet | 1728 | 1329 | 184 | 111 (0) | 94 | 9 | quiet |
| `660c4731e8488` | sonnet | 9002 | 8193 | 298 | 52 (0) | 399 | 60 | 131 slot waits |
| `660ca667b0445` | sonnet | 3600 | 0 | 916 | 2219 (0) | 323 | 142 | own slot wait 2213s |
