+++
type = "task"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais"]
due = "2026-11-23"
+++
# The stop-row count a day after label policy 2 went live

## Question

Does `router_exploration_stops` still hold 0 rows a day after v0.77.0 is
installed, or did a row appear, and under which policy was it counted? A
row means the `holds-until:` on `ROUTER_LABEL_POLICY_VERSION` has fired and
the `label_policy_version` column is owed now (see the ticket on
`router_activations`).

## Look at

- First the preconditions: `relais --version` reads 0.77.0 and a Claude
  Code session has started since it was installed (`router-state` writes
  the stop at session start). Without both, the answer is "no evidence",
  not 0.
- `sqlite3 -readonly ~/.local/state/relais/ledger.sqlite "select * from router_exploration_stops"`
  (the installed binary writes it at session start through `router-state`).

## Answer looks like

The row count and, for any row, its counts and the policy in force when it
was written; `0` closes the ticket with no further action.

## Resolution

2026-11-23 00:30 CET (2026-11-22 23:30Z). **0 rows; closed, no further
action.** The check ran about 1 h after install, not a day after. The
pooled margin below shows a day could not have changed the answer.

- **Preconditions held.**
  - `relais --version` reads 0.77.0 (`/usr/local/bin/relais`, linked
    22:17Z). The plugin's hooks call `relais` from `PATH`, so the
    installed binary serves `router-state`.
  - One session started after the install (tasks at 22:26Z).
  - `relais native router-state` on a probe session reported `mode: on`
    and `epsilon: 0.1`, so the stop rule is evaluated at session start.
    It is skipped only at ε 0. The probe call wrote no row either.
- **Count.** `router_exploration_stops` holds 0 rows. No row was ever
  written under either policy.
- **Margin, all 68 strata pooled.** Folded read-only with the
  `router_exploration_tasks` query, `strong()` and `stop_check`'s rule.
  23 strata have draws.

  | Arm | Tasks | Corrected | Rate |
  | --- | --- | --- | --- |
  | Drawn | 125 | 8 | 6.4% |
  | Undrawn | 1,197 | 89 | 7.4% |

  The gap is −1.0 points against the 15-point margin. Even 25 more drawn
  tasks, all corrected, give +14.6 points: under the margin. The arm drew
  about 15–23 tasks a day over 11-14 to 11-21, counting all outcomes, not
  only strong ones. So no stop could fire in the first day.
- **Draws.** The last explored draw was 2026-11-22 02:33Z. Since then, 40
  tasks ran with 0 draws, which is plausible at the arm's ~3% rate
  (P ≈ 0.3), not a sign that ε is off: the probe reads 0.1.

The `holds-until:` on `ROUTER_LABEL_POLICY_VERSION` has not fired, so
the `label_policy_version` column stays owed only on the first stop row.
t07 (2026-11-29) re-reads the question stratum.
