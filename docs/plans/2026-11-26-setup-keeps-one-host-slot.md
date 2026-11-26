---
status: active
branch: fix/hsa-t12
repos: [relais]
adrs: []
plan-id: p-01M8BK78CT1XJ6B0PN5G7TFYCR
---
# A setup keeps one host slot

## Review panel

👉 **Decide:** none — approve if one kept slot per setup, given back before any coordinator wait, ends the per-step re-queue.
📍 relais · 2 phases · 1 release · next: Phase 1. Panel: backend, lang:rust, tui, unix (opus); codex: not run (programme ticket agent).
**Changed by review:** lock order covers the unreachable coordinator; kept slot named (`Mutex<Option<Lease>>`, `KeptSlot`); give-back and reuse events.
**Verdicts:** round 2: tui approve; backend, lang:rust, unix approve-with-changes; 4 carried into implementation; words 935 → 1297 (×1.39, mostly high findings); remove 0, merge 0.
📄 Full reviews: [2026-11-26-setup-keeps-one-host-slot.reviews.md](2026-11-26-setup-keeps-one-host-slot.reviews.md)

## Picture

```
 today                               after this plan
 -----                               ---------------
 step 1 --take--> slot --run--> free  step 1 --take--> slot --run--+
 step 2 --take--> (tail of line)                                   |
          ...wait...  --> slot        step 2 --rename-------> same |
 step 3 --take--> (tail of line)      step 3 --rename-------> same |
          ...wait...  --> slot        setup ends --> free <--------+
                                      token queued --> free, take afresh
```

A setup step used to rejoin the tail of the host-slot line; after this
plan the setup takes one slot and relabels it for each step.

## Goal

A relais run takes a host slot per setup step and goes back to the tail of
greengate's queue between steps. Under load it starves: three submissions in
a row ended `admission_wait_exhausted` at 1,800 s on 2026-11-21, and admitted
runs waited again at their next step (705 s then 966 s). amont programme
`host-slot-admission`, ticket t12 builds t07's choice: the setup takes one
`greenharness_slots::Lease`, relabels it per step with `Lease::rename`, and
exports the held marker, so no step after the first queues again.

## Non-goals

- The coordinator's own `heavy_local` tokens stay per step. Holding a
  coordinator dispatch across commands would need a new coordinator
  contract (one dispatch bound to several process groups).
- The held marker's form. It stays the bare `held` that crate 5.0 and
  greengate read. t15 changes it to `held:<lease>` later, in amont and
  relais together.
- The checks after the setup. Each check still takes its own slot.
- No crate change in amont (`greenharness-slots` stays at 5.0.0).

## Code review

- relais: backend, lang:rust, unix (tui left out: no prompt or flag changes; the holder name, events and journal rows that change are covered by Verification)

## Behaviour

- `run_setup_gated_within` opens a **kept host slot** on its admission for
  the length of the setup (a `Mutex<Option<Lease>>` field on `HeavyAdmission`, poison recovered as `Tries::next` does, opened by `keep_slot() -> KeptSlot<'_>`; `Admitted` copies `kept_weight: usize` and borrows nothing, so it stays `Sync` for the keep-alive thread), as a guard dropped on every exit, errors and
  cancels included. Dropping it gives the slot back, which writes the one
  `released` journal row. relais installs no SIGINT/SIGTERM handler, so a
  signal ends the runner without the drop: the kernel frees the flock when
  the fd closes, but no `released` row is written and the holder record keeps
  the last label. This gap is accepted and now spans the whole setup, not one step.
- The first step that queues for a host slot takes it through
  `slots::take` as today. The `Lease` is stored in the kept slot instead of
  in the step's `Admitted`, so the step's end does not free it.
- A later step whose weight equals the kept lease's weight and whose
  coordinator answer is an immediate `Granted` reuses it:
  - It does not call `slots::take`: no new `Waiter`, no wait, no journal row,
    no admission budget charged. It emits `Event::Decision { what: "host_slot_kept" }`.
  - `Lease::rename` relabels the holder record with the step's label. The
    holder is built the same way `take` builds it (`relais`, the label,
    `session` = the run id). A failed rename is best-effort: the record keeps
    the previous step's name, and relais says so once per setup on stderr
    (`relais: host slot rename: …`) and as an `EnvironmentWarning` event.
  - Its `Admitted` reports the kept weight from `host_slots()`, so the
    inherited `GREENHARNESS_HOST_SLOT` is removed and the admission's own
    marker is set. Its environment is the one the take path gives:
    `environment(.., UnderHostSlot)` plus the marker.
- **Lock order.** The run never waits at the coordinator while it holds the
  kept slot: today a run holds a coordinator token while waiting for a host
  slot, so the reverse order could deadlock.
  - The kept slot is given back before the wait when a later step's
    coordinator answer is `Queued` or `WaitingForDisk`, and that step then
    takes a slot afresh.
  - It is also given back when the disk falls short after the grant, as
    `drop(lease)` freed it before.
  - It is given back when the slot directory becomes unusable mid-setup.
  - It is given back before the first retry for an unreachable coordinator, or before registering the run again (`ask`, `heavy.rs:639`).
- A step with a different weight gives the kept slot back and takes a fresh
  one, which it then keeps.
- Each give-back before the setup ends emits `Decision { what:
  "kept_slot_given_back", reason }`, the reason naming the trigger
  (coordinator queued, disk short, slot dir unusable, weight changed).
- The marker value is written in one place (`HELD_MARKER`), still the bare
  `held`, replacing the literals at `heavy.rs:624` and `:840` (the reader at `:91` stays for t15). t15 replaces that place with `held:<lease>`. A kept lease keeps one
  lease id for the whole setup, so the per-lease form fits.
- Outside a setup (`admit` with no kept slot open) nothing changes.

## Phases

- [ ] Phase 1 — the kept slot in the admission and its use in setup, with the
  tests: `crates/relais/src/verify/heavy.rs`, `crates/relais/src/verify/mod.rs`,
  `crates/relais/src/verify/heavy/tests.rs`,
  `crates/relais/src/runner/engine.rs`, `CHANGELOG.md`
- [ ] Phase 2 — release relais with this change (`tag-release`; the next
  version after the in-flight v0.89.0, or inside it if that session agrees)
  and install it: `CHANGELOG.md`, `Cargo.toml`

## Decision log

- 2026-11-26 — A coordinator answer other than `Granted` gives the kept slot
  back, because waiting for a token while holding a slot inverts today's lock
  order (token, then slot). A setup that the coordinator contends still
  re-queues per step: accepted, and named in the ticket's Resolution.
- 2026-11-26 — A kept slot writes no `admitted` row between steps, and a
  waiter's 300 s stall clock resets only on an admission or a change of
  place. Measured in `waits.jsonl` (24 relais setups with `released` rows):
  a whole setup holds p50 0.8 s, p95 419 s, max 425 s; 2 of 24 pass 300 s.
  Accepted: such a setup is one long holder, like a 7-minute gate, and the
  stall window is ADR-0015's, not this plan's.
- 2026-11-26 — Reuse needs an equal weight. A heavier step cannot widen a
  lease, and a lighter one would hold slots it does not use.

## Verification

| Input | Expected |
|---|---|
| `run_setup_gated_within`, 3 steps, 1 slot, another waiter joins between step 1 and step 2 (in `SetupGate::ran(0)`) | that waiter stays `Waiting` through step 2; one relais `admitted` and one `released` row; no sleep, admission wait, overrun or `waiting_for_slot` event after step 1; `exhausted()` is `None` |
| the same, with the guard line removed | fails (the waiter is granted between steps) |
| the same 3 steps, with another run granted the only coordinator token in `ran(0)` | the kept slot is given back before the wait; one `kept_slot_given_back` decision event naming the coordinator; the journal shows a second `admitted` row |
| 2 steps, step 2 of another weight | two `admitted` rows, two `released` rows |
| 2 steps, disk short after step 2's grant | the kept slot is released before the disk wait |
| setup where step 1 errors, and one cancelled in `ran(0)` | exactly one `released` row each |
| `admit` twice under an open kept slot | the second `host_slots()` is 1 and its env carries the marker; the holder record names the second label |
| `cargo test -p relais --lib verify::heavy::tests::host_slots` | the new kept-slot tests listed and passing |
| `kill -TERM` the runner during step 2 of a 2-step setup | another waiter takes the slot within one poll; the journal shows `admitted` with no `released`, as Behaviour states |
| a real run with a 2-step setup against the live slot dir | one relais `admitted` row and one `released` row with `held_ms` spanning both steps |

<!-- panel: repos=relais reviewers=backend,lang:rust,tui,unix body-sha=84280d3ae89b -->
