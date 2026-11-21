+++
type = "task"
mode = "afk"
status = "open"
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
