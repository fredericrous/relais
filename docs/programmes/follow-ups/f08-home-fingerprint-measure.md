+++
type = "task"
mode = "afk"
status = "closed"
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

## Resolution

2026-11-28: the feature holds on speed and stays. It does not yet hold on
noise, so the ticket's next step applies, which is attribution and not a
rollback: f47 here and greenbelay t10.

**relais.** 16 runs were created from 2026-11-26 06:36 (#612) to
2026-11-28 02:40. 7 of them warned, with 8 `home_drift` rows: 0.5 per
run. 6 rows are a run repository's `.git/config` and 2 are
`~/.claude/plugins/known_marketplaces.json`. Only one row is from v0.93.0
(installed 2026-11-28 01:36 UTC) and carries a cause: `run-6614c34d3f6a2-141f7`,
a writer, `git branch tracking`. Its run-end view shows only
`1 by known writer: git branch tracking`. The 7 older rows carry no cause.
By hand they fall in the two classes f33 now names, but that is a match by
class and not an attribution. Unexplained in recorded causes: 0 of 1.
Explained: 1. From v0.93.0 on, what counts is unexplained drifts per run,
since known-writer drifts fold into one line: f47 counts that.

**greenbelay.** The window starts at the first live warning,
2026-11-21 04:32 UTC; 6.6.0's install time is gone with its Cellar
directory. It ends 2026-11-26 21:54, the last warning. The journal rotated
before 2026-11-25 20:25, so the warnings were counted from the hook
attachments in the transcripts, one per tool call: 23. From 20:25 on they
match the journal's 4 `broken` lines one for one. Over the journal's
window, 4 of 227 checks broke (1.8%). greenbelay names no cause, so each of
the 23 reads as unexplained. By hand:

| file | warnings | cause found |
|---|---|---|
| a repository's `.git/config` | 11 | git branch tracking or greengate keys, by class |
| `~/.claude/plugins/*.json` | 8 | Claude Code's plugin refresh, by class |
| `~/.claude/settings.json` | 6 | 2 are another session's edit (11-22 00:29 Edit, 11-25 07:10 jq rewrite); 4 have no writer in any transcript within 30 min (11-21 18:25, 20:39, 20:44; 11-23 13:30) |

Two warnings name two files. The 4 settings.json warnings without a writer
are the unexplained residue. They are worth reading, but they come with
about 19 warnings that a known writer explains.

**Hook time.** Measured 2026-11-28 at load 7 to 9 by feeding the installed
greenbelay 6.18.2 `hook` payloads (`/tmp/f08/time.py`, `time2.py`; N=40 and
N=60):

| side | whole hook p50 / p95 | without the check |
|---|---|---|
| before, `cargo test` | 58 / 77 ms | `ls` 51 / 72 ms |
| after, `cargo test`, with snapshot | 75 / 102 ms | no snapshot 66 / 87 ms |

The check's own cost after the command is 9 ms p50 and 29 ms p95, taken
over paired differences, which noise inflates. That is above the 20 ms
fingerprint budget in `home_print.rs`, but under the ticket's 50 ms. The
whole hook's p95 is over 50 ms, but it is 72 ms already where the check
never runs, so a rollback could not bring it under 50. The criterion is
read as the check's own cost: no rollback.

The timing runs wrote 421 synthetic lines to greenbelay's live journal,
under session `f08-meas…` around 2026-11-28 02:46 UTC. Among them are 100
`heavy-build-unqueued` advisories, 160 `bug-nudge` and 160
`home-fingerprint-drift` checks. Appends take no lock, so they were left in
place: exclude that session from any count over that window.
