---
canonical: amont:docs/plans/2026-10-29-every-heavy-job-shares-one-queue.md
phases: [3]
status: active
---
Part of [Every heavy job on the host shares one slot queue](https://github.com/fredericrous/amont/blob/main/docs/plans/2026-10-29-every-heavy-job-shares-one-queue.md).
This repository carries Phase 3: relais takes host slots through amont's `amont-slots` crate (1.62) instead of its own copy of the flock protocol, with shared host slots on by default (`shared_host_slots = "off"` turns them off), and the runner, not the check, holding a slot.
