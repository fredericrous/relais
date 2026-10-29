---
canonical: amont:docs/plans/2026-10-29-every-heavy-job-shares-one-queue.md
phases: [3]
status: done
---
Part of [Every heavy job on the host shares one slot queue](https://github.com/fredericrous/amont/blob/main/docs/plans/2026-10-29-every-heavy-job-shares-one-queue.md).
This repository carries Phase 3: relais takes host slots through amont's `amont-slots` crate (1.62) instead of its own copy of the flock protocol, with shared host slots on by default (`shared_host_slots = "off"` turns them off), and the runner, not the check, holding a slot.

## Decision log

- 2026-10-29: relais may not read amont's git config (`harness.cross-tool-reads`), so it cannot see `amont.hostSlots`. `[resources] host_slots = N` fixes relais's count. Unset, both tools use the crate's automatic sizing and agree. Set `host_slots` to the same value as `amont.hostSlots` whenever that one is set; otherwise the two tools disagree on the cap.
- Phase 3's precondition, the load of one holder: measured by process-group CPU, not load1, because this host was never idle (load 6–600 all day). One full amont `cargo test`: mean 0.5 runnable threads, p90 0.7, peak 4.7 (compiling), over 856 s. Fixtures that start their own process group escape the sum, so this undercounts. One holder does not fill 12 cores, so the person's threshold (admit while load1 < cores) stands.
- Live: this branch's own commit gate (`cd964f25`) queued behind another session's amont rehearsal (`queued behind amont: pre-push-run-tests-js (amont-push-…)`) and its own clippy. Waits: 48 s, 2m23s, 10m48s. The 10m48s single sample is above the plan's 10-minute p95 bar. The cause was the 60 s ramp spacing one commit's own checks. The person decided that a commit's own checks skip the ramp once a sibling holds a slot. That is an `amont-slots` change, shipping as amont 1.62.1 after this PR.
- `.cargo/config.toml` pins `AMONT_HOST_SLOT=held` for relais's tests, as amont's does. Without it, any test that builds a runner with default settings would queue on the machine's real slots, one per ramp on CI. When the runner holds slots, the check spawn drops an inherited `AMONT_HOST_SLOT` and takes the admission's, so the tests still prove the runner sets it.

## Verification (Phase 3)

Observed before the push, on `0e0bb9cc`.

- **Heavy-admission and protocol tests:** 47 passed on the first commit, 34 in the focused re-run.
  - a missing directory is made 0700 and a slot is held;
  - an unsafe (0755) directory warns once and falls back;
  - a weight above the cap is refused, not capped;
  - an old relais counting `slot-*` files never counts a holder record;
  - a check waits for a held slot, runs holding it, and its wait names the holder (`"behind":"other: xcodebuild …"`);
  - the cargo pin reaches the tests.
- **`a_run_with_no_setting_queues_in_amonts_directory`:** None → `amont_slots::dir()`, `"off"` → none, a path → that path. Passed.
- **Doctor:** default → "shared host slots in …, sized from the load"; `"off"` → "no shared host slots"; `host_slots = 2` → "2 fixed". Passed.
- **Plugin:** `make plugin` run alone → 332 pass, including "waiting for a host slot behind amont: cargo-test (amont, 2m) · 60s".
- **The `slot=held` assertion shown red:** with the runner's own insertion removed, the check printed `slot=`. With the file restored (`cmp` identical), the tests are green.
- **Commit gates:** `cd964f25` passed after the live queue waits above; `6de1a132` and `0e0bb9cc` passed the full suite. The first try of `6de1a132` failed the slot-wait test in the parallel suite, because a forked child held the admission lock and no event named the holder. The test now releases the other holder once an event names it.

## Implementation review

- approve-with-changes, round 1 then Delta.
- Fixed:
  - the plan records above;
  - same-line reasons on the test discards;
  - `HOST_SLOT_ENV` from the crate;
  - the default-directory helper and its test;
  - `deferred` samples written for budget and cancel ends;
  - the cargo test pin;
  - `held` from the admission, not inheritance;
  - the `cargo run` caveat noted.
- Deliberate: `ancestor_holds_slots` stays as a two-line boundary predicate; the holder's command is the check label (`make check@<sha>`).
