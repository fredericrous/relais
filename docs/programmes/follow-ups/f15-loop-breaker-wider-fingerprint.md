+++
type = "research"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais"]
+++
# Would a wider no-progress fingerprint catch loops the Bash rule misses

## Question

The loop breaker of #418 (v0.51.0, shadow only) fires on the Nth identical
failing Bash result with no edit in between. Its baseline found 0 such loops
in the 22 readable transcripts of 56 wall-timeout runs, so it stays off.

Lumina's no-progress breaker (`core/progress_breaker.py` in
github.com/Bino5150/lumina at `972c09a0`, read 2026-11-23) uses a wider
rule. Each call is fingerprinted as `(tool, args digest, normalized result
digest)`, for every tool, not only Bash. Normalization turns UUIDs and hex
runs into `<hex>` and digit runs into `#`, so a poll whose only change is a
clock or an id reads as a repeat. A call is stale when its fingerprint was
seen this turn **or the call failed**: without the second rule, one
malformed call every ~15 calls reset the streak forever. Any other call is
progress and resets the streak. Write-tier tools warn at 3 and trip at 5,
read-only tools warn at 10 and trip at 20, and the owner can tune both only
within clamped bounds. A model-facing warning naming only tools and counts
comes before the trip. Their two production runaways ran thousands of calls
with only 3 and 26 distinct (call, result) pairs.

Do relais's wall-timeout runs hold stale streaks under Lumina's rule that
the Bash rule does not see? The answer decides whether relais widens its
shadow detector to that fingerprint (and whether a warn rung before the
stop is worth adding), or keeps the Bash rule and closes this as no change.

## Look at

- `docs/plans/2026-11-08-loop-breaker-native-workers.md` (the rule, the
  shadow bar, and the Baseline row) and where the transcript tail is read
  in `crates/relais/src/runner/` (the `loop_threshold` uses).
- The subagent transcripts still on disk for runs that ended
  `process_crash` with `timed_out: true`, and the `relais: shadow:` lines
  in `<runs-dir>/*/events.jsonl` since v0.51.0 was installed.
- Lumina's `core/progress_breaker.py` (`normalize_result`, `ProgressLedger.observe`,
  the thresholds at the top) for the exact rule to replay.

## Answer looks like

A table over the same transcripts: per run, the longest stale streak under
the Bash rule and under Lumina's rule (all tools, normalized results,
failures stale), split by write and read-only tools, and whether the worker
then passed. Then one decision: widen the shadow fingerprint (with the
thresholds to shadow at), add a warn rung, or no change, with the counts
that decide it.

## Resolution

**No change.** Do not widen the shadow fingerprint, and add no warn rung.
relais's wall timeouts are not call storms. 78% of the workers' wall time
(37,830 of 48,365 s) is spent inside tool calls, and no transcript has more
than 140 calls. Lumina's runaways ran thousands of calls with 3 and 26
distinct pairs. Every streak that either rule flags here is a worker waiting
on a slow test or build. Stopping it and starting the next rung would rescue
nothing.

**What was replayed** (2026-11-24). The runs directory holds 49 runs with an
attempt that ended `process_crash` with `timed_out: true`, which make 50
timed-out dispatches. A timed-out dispatch has no `agent_id` in the ledger,
so each one was matched to its subagent transcript by session id and start
time: the first transcript line within 2 min of `dispatch_started`. That
found 32 transcripts, 31 workers and 1 reviewer; 18 are no longer on disk.
The baseline had 22 of 56. Two rules were replayed over each transcript:

- **The Bash rule** as shipped in `adapter/loopguard.rs`. A failing Bash
  result is fingerprinted by (normalised command, first error line). A
  command whose last segment is a piped `tail`, `head` or `grep` is
  fingerprinted by its output hash, whether it failed or not. An edit or a
  mutating command clears the counts.
- **Lumina's rule** from `core/progress_breaker.py` at `972c09a0`. Every
  tool is fingerprinted by (tool, args digest, normalised-result digest). A
  call is stale when its fingerprint was seen before or the call failed;
  any other call resets the streak. It warns at 3 stale write-tier calls or
  10 stale calls of any tier, and trips at 5 or 20. Bash, Task and MCP tools
  are write tier, as Lumina fails closed. As a sensitivity check, the rule
  was also run with non-mutating Bash as read-only, as Lumina does for its
  `check_background_task` poll.

**Counts.**

- The Bash rule reaches 3 on 2 of 32 transcripts, both before v0.51.0 and
  both through the piped-filter clause:
  - `65f3d9db`, ×6: a `for … sleep 5; done; grep … | head` poll of a
    background vitest run. The suite then passed, 817 tests.
  - `65ec661c`, ×3: a similar poll, after which the run made progress.

  Both are false positives. The other 30 transcripts reach at most 2. That
  includes all 11 timed-out runs since v0.51.0, which agrees with the 0 real
  `relais: shadow:` lines in 1,264 runs. (The 2 grep hits are prose in run
  `65fd1280`.) No run printed `loop guard off … transcript not found`, so the
  shadow detector was watching.
- Lumina's rule trips on 2 of 32 transcripts and warns on 4 more:
  - `65f3d9db`: trips at call 66 on the same vitest poll. Normalisation makes
    this worse, because a growing `wc -l` count reads as no change. This is
    a false positive.
  - `65f0347e`: trips at call 97. About 25 identical
    `timeout 118 tail -f … ; tail -20 /tmp/clippy.out` polls while clippy ran
    for about 35 min at 79% CPU on a loaded host. Its tests passed at call
    134. This is a host stall, not a model loop.

  With Bash polls as read-only, nothing trips.
- No timed-out run's successor was accepted. They ended blocked,
  needs_decision, needs_review or failed, so a trip would have rescued no
  run.

The Bash column of the table is the longest count of one fingerprint. The
Lumina columns are the longest stale streak over all tools and over
write-tier tools. "Trip" is the call at which Lumina trips: all Bash in the
write tier, or non-mutating Bash as read-only.

| run | worker | calls | distinct | Bash max | Lumina stale | Lumina write-stale | trip | trip (Bash read-only) | successor ended |
|---|---|---|---|---|---|---|---|---|---|
| `65e9e26ee32ac` | fable (reviewer) | 27 | 15 | 0 | 7 | 0 | – | – | no successor |
| `65ead843594b0` | sonnet | 60 | 60 | 1 | 1 | 1 | – | – | no successor |
| `65ebe2731332d` | sonnet | 38 | 38 | 1 | 2 | 1 | – | – | blocked |
| `65ec661cb28fa` | sonnet | 140 | 128 | 3 | 3 | 3 | – | – | no successor |
| `65ee47653e91f` | opus | 6 | 6 | 1 | 2 | 2 | – | – | blocked |
| `65ef580d36317` | opus | 7 | 7 | 1 | 1 | 1 | – | – | blocked |
| `65ef6c5be313c` | sonnet | 20 | 18 | 0 | 2 | 2 | – | – | no successor |
| `65efb16db924f` | sonnet | 62 | 57 | 1 | 2 | 2 | – | – | no successor |
| `65efd34aed95e` | sonnet | 37 | 31 | 1 | 2 | 1 | – | – | blocked |
| `65efedd17f79f` | sonnet | 33 | 31 | 1 | 4 | 4 | – | – | no successor |
| `65f00d23ff828` | fable | 22 | 22 | 1 | 1 | 1 | – | – | no successor |
| `65f0347e22e71` | sonnet | 134 | 91 | 1 | 9 | 9 | #97 | – | no successor |
| `65f1afb7e0d9f` | sonnet | 127 | 114 | 1 | 4 | 2 | – | – | no successor |
| `65f2a671ea883` | sonnet | 47 | 42 | 1 | 4 | 2 | – | – | no successor |
| `65f33d11b9afa` | sonnet | 32 | 28 | 1 | 4 | 2 | – | – | no successor |
| `65f3ba4f1080b` | sonnet | 65 | 58 | 2 | 3 | 2 | – | – | no successor |
| `65f3d9dbf952a` | sonnet | 73 | 58 | 6 | 5 | 5 | #66 | – | no successor |
| `65f49d2acbbc2` | sonnet | 27 | 25 | 1 | 2 | 0 | – | – | blocked |
| `65f6041b8b981` | sonnet | 60 | 47 | 1 | 3 | 1 | – | – | no successor |
| `65faaaa725e69` | sonnet | 18 | 15 | 1 | 3 | 0 | – | – | no successor |
| `65faaba8f3cfe` | sonnet | 85 | 80 | 1 | 2 | 1 | – | – | no successor |
| `65fdf979b7cc6` | sonnet | 33 | 29 | 1 | 2 | 1 | – | – | no successor |
| `65fe1bb588f1d` | sonnet | 84 | 80 | 1 | 3 | 2 | – | – | no successor |
| `65fe314e75700` | sonnet | 57 | 52 | 1 | 2 | 1 | – | – | no successor |
| `6602a71dd67a4` | sonnet | 76 | 68 | 2 | 3 | 3 | – | – | needs_decision |
| `66038b34f33b8` | sonnet | 60 | 59 | 1 | 2 | 2 | – | – | failed |
| `6604f909f8772` | sonnet | 9 | 7 | 1 | 2 | 1 | – | – | no successor |
| `6606b1fb10437` | sonnet | 83 | 81 | 1 | 1 | 1 | – | – | needs_decision |
| `6609237862af8` | sonnet | 106 | 92 | 1 | 4 | 4 | – | – | needs_review |
| `660c032dd8c5d` | sonnet | 29 | 25 | 1 | 3 | 2 | – | – | blocked |
| `660c4731e8488` | sonnet | 63 | 57 | 1 | 3 | 2 | – | – | needs_decision |
| `660ca667b0445` | sonnet | 89 | 70 | 1 | 3 | 1 | – | – | needs_decision |

**What follows.** The piped-filter clause is what fires on these polls. Since
`for …; done; … | head` is how workers wait, the shadow detector would have
reported these 2 false positives if it had been live. Before
`loop_threshold` is enabled, the plan's carried item has to land: count a
piped filter only when its output also matches a failure marker. That is
[f17](f17-piped-filter-polls.md). Where the timed-out wall time goes, slow
suites and host contention, is [f18](f18-timeout-wall-time-waits.md).
