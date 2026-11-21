+++
type = "task"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais"]
+++
# Stream the hash and store absolute paths in home drift records

## Question

Should relais's home fingerprint stream its hash and cap the size it reads for `extra_paths`, and keep the absolute path in `home_drift` records, applying the `~/` form only when printing?

## Look at

`crates/relais/src/homeprint.rs` (snapshot reads whole files), `runner/home_drift.rs` (`drifts_of_run`, stored path), the architect and unix carried findings on #512 in `docs/plans/2026-11-20-home-fingerprint-warns.md`. greenbelay#306 chose size + mtime for files over 1 MiB (`home_print.rs` `HASH_LIMIT`).

## Answer looks like

A PR: a streaming or capped read with a test on a large file, records holding absolute paths, `~/` applied at print time; or a decision not to, with the reason.

## Resolution

2026-11-21: **cap the read, do not stream.** A regular file over
`HASH_LIMIT` (1 MiB, the same limit greenbelay#306 chose) is never opened.
It is recorded as `Entry::Large { modified_ns, size, mode }` from one
`symlink_metadata`. A file at or under the limit is read whole, as before,
so the read seam stays a `Vec<u8>` and memory stays bounded at 1 MiB per
file. Streaming alone would bound memory, but it would still read a
multi-gigabyte file in `extra_paths` on every run start and end. The cap
bounds both memory and time. Cost: a large file that is touched with the
same content shows as `modified`. greenbelay accepted the same cost.

**Absolute paths in records.** `homeprint::diff` stores
`path.display()` (absolute) and no longer takes `home`. `homeprint::report`
takes `home: Option<&Path>` and applies `~/…` (unix only) when it prints.
The CLI passes the printing process's home (`homeprint::print_home`).
A record an older relais wrote already holds `~/…`, and `strip_prefix`
leaves it unchanged, so old runs print as before.

Tests: `a_file_over_the_hash_limit_is_never_read_and_its_change_still_shows`
(the read seam panics if it is asked for the large file),
`modified_added_and_removed_files_are_stored_absolute_and_printed_with_a_tilde`,
`a_record_stored_with_a_tilde_prints_unchanged`,
`with_no_home_a_report_prints_the_stored_path`; the runner fixtures now
assert the absolute stored path on every OS.
