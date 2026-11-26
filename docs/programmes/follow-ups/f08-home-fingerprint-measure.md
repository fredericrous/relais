+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais", "greenbelay"]
due = "2026-11-28"
+++
# Measure the home fingerprint warnings

## Question

The home fingerprint exists to catch what the LLM changes on its own, which
is about 1 to 2% of the drifts; about 98% are changes the person asked for.
Seven days after each phase is installed, how many warnings were
unexplained, meaning no tool call in that run touched the path and no known
tool wrote it, and how many were explained; and is the hook fast enough?

## Look at

The "Measure after release" section of `docs/plans/2026-11-20-home-fingerprint-warns.md`
(its tool-caused versus person-edit split and its 1-in-3 rollback rule
are superseded by this ticket). relais: `home_drift` evidence rows since the
v0.73.0 install; interim count 2026-11-26: 46 rows, 17 `.git/config` (greengate
trust keys, silenced by #612), 20 `~/.claude/plugins/*.json`, 9
`~/.claude/settings.json`; the rows carry only path and change, so
"explained" needs attribution (f33). greenbelay: `home-fingerprint-drift`
journal lines from 7 days after the first greenbelay release carrying #306 is
installed: that release is greenbelay v6.6.0 (tag eb07f717 pushed 2026-11-21,
#307). The Mac still ran 6.5.0 at 02:50 that day, so the window counts from
`brew info greenbelay`'s install time of 6.6.0 and ends 2026-11-28 at the
earliest. Count only rows after #612 (merged 2026-11-26 06:37) for relais.

## Answer looks like

Per tool, a count of unexplained and explained warnings, the warnings per run,
and greenbelay's hook p95. The feature holds if unexplained drifts are rare
and each is worth reading, and warnings per run are low enough that one
stands out, and the p95 is at most 50 ms. Otherwise: add an allowlist or the
attribution of f33 before any rollback; roll back (relais's `enabled` default
to false, greenbelay's assertion removed) only if the p95 exceeds 50 ms.
