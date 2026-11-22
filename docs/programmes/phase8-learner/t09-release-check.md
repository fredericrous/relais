+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
due = "2026-11-23"
+++
# Confirm v0.77.0 shipped and is installed

## Question

The entry plan's Phases 1–3 merged in relais#533 and its release branch
`chore/release-v0.77.0` was landing when this programme was charted. Did
the release PR merge, did the `v0.77.0` tag's Release workflow conclude
`success` with its assets, and does `relais --version` on this machine read
0.77.0 (the plugin reinstall is the person's step)?

## Look at

- `gh pr list --repo fredericrous/relais --state merged --search "release: v0.77.0"`,
  `gh run list --repo fredericrous/relais --workflow Release --limit 3`
  and the run's `conclusion`, `gh release view v0.77.0`, `relais --version`.

## Answer looks like

The merge sha, the Release run's conclusion, the asset list, and the
installed version; if any is missing, the step that supplies it, and the
stop-row check waits on it.
