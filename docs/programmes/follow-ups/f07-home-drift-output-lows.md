+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
+++
# Home drift output: three small fixes

## Question

Do the three low output findings carried from #512 still hold, and are they worth fixing: `drifts_of_run` problem lines on stderr without the `relais run:` prefix; the `is_valid_extra_path` doc not naming `~\` on Windows; the no-home error line at about 130 columns?

## Look at

`crates/relais/src/runner/home_drift.rs`, `policy/mod.rs` (`is_valid_extra_path`), the tui and unix lines of the Implementation review in `docs/plans/2026-11-20-home-fingerprint-warns.md`.

## Answer looks like

One small PR fixing each that still holds, or a line per finding saying why it stays.
