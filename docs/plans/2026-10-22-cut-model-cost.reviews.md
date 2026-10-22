# Cut model cost: full reviews

- **backend R1** (opus-5-5, 52k, 113 s): rework on C. Blocking: ADR-0006 replay vs live. High: day cap misread; static model bypasses ADR-0001; advisor cost unmodelled; promotion needs a trust grant. Medium: volume, B cap, revert rule. All accepted; ADR-0001 handled by keeping the router as owner.
- **backend R2** (opus-5-5, 62k, 192 s): approve-with-changes. High accepted: gate C.4 on the decision; probe Haiku main too; 5/7 corrected to 7/7 (no escalations). Rejected: 31-day denominator (October is 22 days so far). Carried: holdout control, `{runs, more}` reply shape, `PLUGIN_SKIP_OK`, rollback owner, escalation price.
- **backend final** (opus-5-5, 32k, 60 s): high $3 vs $5 mismatch (removed with the caps). Carried: gate base, revert should delete `advisorModel` not set `model`, compare to the holdout over the same 7 days, `waiting: N` count.
- **codex** (gpt-6-astra, high effort): rework. Blocking: live-trial admission refuses a research-tier change (`live_trial.rs:112`, `candidate.rs:656`), so C became replays. High: the paired comparison needs 20 tasks (`comparison.rs:45`). Medium carried: pagination for `status`.
- **backend after codex** (opus-5-5, 46k, 93 s): high accepted: the person judges rejected replays; check the $20 budget after each replay; verify the 7 trials count under the arm. Carried: stop rule at the first 15; trust grants for other repositories before replaying.
- **backend binding** (opus-5-5, 20k, 18 s): approve-with-changes. Carried: a replay can overshoot $20 by one escalation; treat as a soft limit of one replay.
