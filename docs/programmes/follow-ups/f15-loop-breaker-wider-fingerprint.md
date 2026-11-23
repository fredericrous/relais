+++
type = "research"
mode = "afk"
status = "open"
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
