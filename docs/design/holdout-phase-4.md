# Design: what holdouts catch and leak, counted (ADR-0045, phase 4)

ADR-0042: every harness part names the failure it prevents and where that
failure is counted. Phase 4 makes the two holdout counts real.

## Record each holdout outcome

A ledger migration (schema 44, expand-only):

```sql
CREATE TABLE holdout_outcomes (
  run_id TEXT NOT NULL REFERENCES runs(id),
  attempt_index INTEGER NOT NULL,
  name TEXT NOT NULL,
  outcome TEXT NOT NULL,          -- passed | failed | did_not_build
  profile_green INTEGER NOT NULL, -- 1 when every profile check passed on that candidate
  PRIMARY KEY (run_id, attempt_index, name)
);
CREATE TABLE holdout_exposures (
  run_id TEXT NOT NULL REFERENCES runs(id),
  dispatch_id TEXT NOT NULL,
  tool TEXT NOT NULL,             -- the tool name, e.g. Read, Bash, Grep
  marker TEXT NOT NULL,           -- which marker matched: store | state_dir | machine_toml | overlay | config
  PRIMARY KEY (run_id, dispatch_id, tool, marker)
);
```

`verify_candidate` writes one `holdout_outcomes` row per holdout it ran,
with `profile_green` true when the verification's profile checks all passed
(no failed check and no profile gap).

## Count exposures from the workers' own transcripts

When a run ends (at the terminal transition, every state), read the transcript
of each of the run's worker dispatches (the native adapter's `transcript_of`,
`<projects>/*/<session>/subagents/agent-<id>.jsonl`) and scan every
`tool_use` block's `input` (all string values, recursively) for the markers:
- `store`: the resolved store URL, and its path without the scheme;
- `state_dir`: the absolute `<state>/holdouts` root;
- `machine_toml`: the machine.toml path and the file name `machine.toml`;
- `overlay`: each resolved overlay path;
- `config`: each declaration's config path inside the store.

Each (dispatch, tool, marker) hit is one row. Only the marker name is
stored, never the matched string. A missing or unreadable transcript is not
an exposure, and it is recorded as a run event `holdout_exposure_unread {dispatch}`.
The scan is a pure function over transcript text and the markers (unit-tested
on literal transcripts); the engine supplies the text.

## `relais report`

`relais report` and `--json` (bump `REPORT_SCHEMA_VERSION` to 14, key
`holdouts`) gain, over the window's runs:
- `holdout_catches`: runs with at least one `holdout_outcomes` row where
  `profile_green = 1` and `outcome != passed`. This is the failure ADR-0045
  prevents: acceptance on evidence the worker controlled.
- `holdout_exposures`: runs with at least one `holdout_exposures` row, with
  counts by marker.
- `holdout_runs`: runs that named at least one holdout, so the two numbers
  have a base.
In the text report, one line per count, within 80 columns.

SPEC §10 gains the ADR-0042 line: "Holdouts prevent acceptance on evidence the
worker wrote or weakened; counted as `holdout_catches` in `relais report`; how
far hiding by omission holds is counted as `holdout_exposures`."

## Carried

- A `kind: verify` of a candidate whose profile checks pass and whose holdout
  fails is not accepted, dispatches no worker, and adds one catch.
- `--rerun-verify`, `--successor-contract` and `review-parts` intents each reach
  `intent_of_run` (a successor of each kind inherits the frozen definition).
- A chain of three runs (a successor of a successor) is judged by the first
  run's frozen definition.

## Tests

- A table test of the scan, over literal transcript lines, for each marker. A
  transcript that names none of them gives no row.
- A run with a failing holdout and green checks → `holdout_catches` = 1. A
  passing one → 0.
- A fixture run whose worker transcript holds a `Read` of the machine.toml path →
  `holdout_exposures` = 1 with marker `machine_toml`. This is the positive control.
- `relais report --json` shows the three counts under `holdouts`.
- The carried cases above.
