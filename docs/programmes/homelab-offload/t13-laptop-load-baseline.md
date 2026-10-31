+++
type = "research"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
+++
# What loads the laptop today, and what target counts as done

## Question

Measure the 1/5/15-minute load average and CPU-seconds per day, split between heavy checks and Claude Code processes, over 7 days and before anything moves; record heavy-slot occupancy. The first run records what already exists (ledger timings, heavy-slot waits) and writes the sampler spec here: command, output path, stop condition. Starting the sampler is the first step of this ticket's next session; the ticket closes when the window ends, with the target the Destination uses.

## Findings so far

Explore research, 2026-10-31 (the relais inspect run was blocked by low disk).
The ticket stays open until the 7-day window ends.

- **Today:** `uptime` shows a load average of 159.8 / 154.7 / 119.9 on 12
  CPUs (15 days up). The biggest `ps` %CPU sums are `claude` 229,
  `fseventsd` 127, Chrome 89, and relais from an amont-push temp dir 51.
- **Where it is recorded:**
  - Check durations are only in `check_ended` events in
    `~/.local/state/relais/runs/<run>/events.jsonl`
    (`crates/relais/src/protocol.rs:121-135`). The ledger receipts have none.
  - Host-slot waits are `waiting_for_slot {class, position, waited_s}`,
    emitted about every 30 s; take the maximum per run (`protocol.rs:175`,
    `verify/heavy.rs:59`).
  - Admission wait has no persisted per-run figure. Only
    `blocked (admission_wait_exhausted)` and the coordinator's slow-request
    log show it.
- **Last 7 days** (check wall-seconds, runs that waited on a slot, summed
  maximum slot wait):

  | Day | Check wall-seconds | Runs that waited | Summed max wait |
  |---|---|---|---|
  | 10-25 | 12.7k | 3 | 0.8k s |
  | 10-28 | 45.5k | 34 | 27k s |
  | 10-30 | 51.8k | 50 | 47k s |
  | 10-31 (partial) | 41.2k | 57 | 127k s |

  The longest single wait was 2230 s, at queue position 10.
- **Sampler spec (step 1 of this ticket's next session):**
  - **Agent:** a launchd agent, `~/Library/LaunchAgents/local.laptop-load.plist`,
    with `StartInterval` 60 and `RunAtLoad`. The script lives outside the
    data directory.
  - **Per sample:** `epoch ncpu load1 load5 load15` from `sysctl -n
    vm.loadavg`.
  - **CPU per bucket:** the difference in cumulative `ps -axo
    pid,ppid,time,comm,command` CPU time per pid. The buckets are claude,
    cargo/rustc, node, relais-launched (any process with a `relais` ancestor
    on the ppid chain), and other.
  - **Output:** `~/.local/state/laptop-load/samples.tsv`.
  - **Stop:** a `start` epoch file; after 604800 s the script runs
    `launchctl bootout` on itself.
  - **Caveat:** short-lived children are under-counted.
- **Candidate target:**
  - The working-hours median of the 15-minute load average falls below 12,
    with the exact threshold set after the window.
  - relais-attributed CPU-seconds per day drop by a stated percentage.
  - The median slot wait per run and the number of waiting runs per day both
    fall.
