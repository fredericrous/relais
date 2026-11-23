---
canonical: amont:docs/plans/2026-11-23-slot-journal-records-holders.md
phases: [4]
status: done
---
Part of [The slot journal records holders and hold time](https://github.com/greenharness/greengate/blob/main/docs/plans/2026-11-23-slot-journal-records-holders.md).

## Code review

- relais: backend, lang:rust (unix, tui left out: no shell or screen change)

## Implementation review

- implementation-review (opus): approve-with-changes, then approve. Suite on CI; only the slot crate moved in the lock.
- backend (opus): approve-with-changes, then approve. Fixed: manifest floor 4.13, release-line session, weight and jobs pinned.
- lang:rust (opus): approve-with-changes, then approve. Fixed: floor, comma-terminated patterns. Deliberate: the `line` name kept.
