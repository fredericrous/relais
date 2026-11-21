---
canonical: decisions:docs/plans/2026-11-21-harness-change-names-cause.md
phases: [1]
status: done
---
Part of [A harness change names the cause it targets](https://github.com/fredericrous/decisions/blob/main/docs/plans/2026-11-21-harness-change-names-cause.md).

## Code review

- relais: backend, lang:python, unix (tui left out: plain TSV and JSON output, no interactive terminal)

## Implementation review

- implementation-review: approve-with-changes → Delta approve-with-changes; 4 rule findings fixed (banner, `tmp`, stdout ok line, bare except), fixtures added; plan text (exit 4, temp-file ledger) fixed in the canonical plan; deliberate: broad helper `except` (backend asked for it). ~113k, ~3 min.
- backend: approve-with-changes → Delta approve; DB errors exit 1, bad timestamps warned, one read snapshot, unreviewed path tested. ~105k, ~2 min.
- lang:python: approve-with-changes → Delta approve-with-changes; 7 fixed; deliberate: file I/O stays in `count` (one-file script), explicit `begin` (verified on Python 3.15), self-test read handle left to the temp dir. ~70k, ~1 min.
- unix: approve-with-changes → Delta approve; exclusive modes, `allow_abbrev=False`, Ctrl-C 130, help from the docstring. ~69k, ~1 min.
