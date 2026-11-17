# SessionStart on clear shows how to resume — full reviews

**Round 1** (opus-5-5): backend approve-with-changes (46k, 74 s); lang:rust approve-with-changes (65k, 85 s); tui approve-with-changes (37k, 51 s); unix approve-with-changes (49k, 87 s). All findings were accepted except backend#6: `plan-panel --code-review` itself proposed tui.

**Round 2** (opus-5-5): backend, lang:rust, tui and unix ran as a delta, every round-1 finding resolved. Architect (50k, 45 s) and po (39k, 41 s) ran their first round. Applied:
- rust#7 (high): the threaded deadline;
- architect 1–4: format contracts per `harness.cross-tool-reads`, local days, ADR-0075, SPEC §23;
- po 1–5: measure after release, the named live row, a `next:` prompt that asks for the quote, the worst-checkout budget, independent releases.

**Final binding:** backend, lang:rust and po approve-with-changes; tui, unix and architect approve.

**Codex** (gpt, high): approve-with-changes, two high findings, both applied:
- the source of each programme carried into its locator and Handoff;
- takes scanned without the inbox.

Backend re-ran on that body and approved with changes, with no new blocker.

**Carried into implementation:**
1. The inbox Read bullet's grammar includes `[due=…]`.
2. The unit row expects a young take counted as `N lines being filed`, not "not read".
3. The fresh-line text after a `/clear` points at the inbox path and the `session8` rule, never at unseen lines.
4. A deadline unit row with a slow stub: `skipped(deadline)`.
5. A local day read with `date +%F` once per start, with `today` as a parameter.
6. The failure note prints the first failing programme's absolute directory.
7. The live row expects "no `greenbelay/inbox:` line" when the inbox is empty.
8. The rollback levers: a relais patch release that returns `Silent`, and greenbelay's `clear-at-boundary` set to `observe`. A rollback on one `skipped(deadline)` is too strict; use p95 > 1,000 ms or more than 1 % of starts.
9. The version token is a documentation version: lines carry no token.
10. An inbox follow-up: the worktree-task skill points at greenbelay's documented format.
11. Local versus UTC near midnight is noted in t05's Resolution.
12. The gates row stays a precondition.
13. Verification rows for Codex's locator and abandoned-take tests.
14. The po#6 scratch-run stand-in for the live unfiled line goes in the Resolution.
15. The tui#1 gap: the Handoff names the cleared session's `session8`.
