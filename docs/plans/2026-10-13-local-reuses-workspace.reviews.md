# Reviews: local runs reuse the caller's workspace

## Full reviews (reference)

- **backend** round 1 `rework` (50k tokens, 112 s): blocking real index/branch; high lock, ledger backup file, golden contradiction; medium accepted-tree, restore deletion, clone cost. Round 2 `approve-with-changes` (40k, 59 s): 1–6 resolved, 7 not resolved; new 8 golden excludes, 9 in-place retire wiring (`main.rs:7371`, `:7230`), 10 marker vs lock, 11 HEAD-moved tree. Patches for 7–11 are in the round-2 transcript and are carried.
- **language/rust** `approve-with-changes` (45k, 91 s): temp index, single lock, recursive mtime, HEAD (rejected: duplicate of backend, branch is never reset), golden, statvfs noise.
- **unix** `approve-with-changes` (42k, 90 s): lock race, recursive mtime and count, lock-based marker, symlinks/hardlinks/modes in the Linux walk, precedence of flag/env/CI/config, private-volume disk test, temp index path.
- **tui** `approve-with-changes` (41k, 115 s): start message cannot know the resolved mode, block message texts, interrupted-run route, progress during the quiescence wait, CoW-unavailable remedy, `resume --retire` and doctor wording.
