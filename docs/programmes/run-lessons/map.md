# relais runs learn from their siblings

## Destination

Two channels are live in relais. A review finding from one run, and a
workaround the orchestrating session found, reach every later run in the
same repository whose write scope or read hints overlap the files the lesson
is about, whatever session dispatched it. Each lesson carries its evidence
and an invalidation rule, and stops being given once the files it is about
change. A count of findings and failures repeated across sibling runs,
taken before (t01) and after, shows the effect.

## Notes

- **Origin:** session of 2026-11-19, comparing relais with
  Chachamaru127/claude-code-harness, whose Breezing lead injects reviewer
  gotchas from one worker into the next worker's brief (kept in memory,
  lost at session end). relais passes nothing between runs today.
- **What exists:** review findings live only in each run's review result
  file (`review_parts.result_path`) and go back to that run's own worker,
  once (`runner/machine.rs:946`). The context package (SPEC §7) carries the
  contract, aval guidance and failure evidence. SPEC §18 already specifies
  "an index linking … previously verified findings" with source
  fingerprints, applicability and invalidation dependencies; no code
  implements it.
- **The person's choices (2026-11-19):**
  - destination: built and measured, not only specified;
  - a sibling is any later run in the same repository whose write scope or
    read hints overlap the lesson's files, any session; a lesson is
    invalidated when those files change;
  - a workaround is recorded explicitly by the orchestrating session (a
    relais MCP tool and CLI, with its evidence), with a hook that nudges
    when a fix looks like one; never mined silently.
- **Governing decisions:** resolve before deciding a ticket: aval's
  precedence as stated in SPEC §7 (an applicable decision outranks any
  remembered claim, SPEC §18), `routing.task-type-research` (ADR-0006),
  relais ADR-0013 (review only under `review: required`).
- **Neighbouring work:** relais's routing learning (SPEC §16–17, Laya)
  learns which model to pick, not what to tell it; greenbelay's corpus and
  weekly retro turn repeated session mistakes into rules. A lesson that
  keeps recurring may belong there instead (t07).

## Decisions so far

- [How a lesson knows the files it is about have changed](t04-invalidation.md): pin to blob ids of named files, prefiltered by one diff per run; 30-day backstop
- [How a wrong lesson is retired and a lasting one promoted](t07-wrong-lessons.md): retire by command, by contrary outcomes or a false-finding verdict; relais only proposes promotion, greenbelay or an ADR owns the rule
- [What a relais review finding holds that a sibling could use](t02-finding-shape.md): only severity is parsed and no disposition is kept; a fixed line grammar, typed findings and a findings table with fixed/kept come first
- [How often a sibling run repeats a finding or a failure today](t01-repeat-baseline.md): reviewer prose repeats 1/162, but code-rule violations repeat 89/162; failure repeats are a floor until workarounds are recorded
- [Which review findings become lessons for siblings](t03-which-findings.md): rule-cited findings the repair fixed
- [What the session records when it found a workaround](t05-workaround-record.md): symptom, fix, pinned paths, evidence; nudged after a repair
- [Where a lesson appears in a sibling run](t06-where-lessons-go.md): worker block of about 8 KiB, and the reviewer reports a repeat
- [When a lesson is a promotion candidate](t09-promotion-thresholds.md): 3 sessions, 7 days, one inbox line

## Handoff

2026-11-19, charting session: the map, research t01, t02, t04, t07 and the
person's answers to t03, t05, t06, t09 are on branch
`docs/programme-run-lessons`. Next: [Where lessons are stored](t10-lesson-storage.md),
then one build plan in plan mode (review panel) covering t02's grammar and
findings table, the lessons store, `relais lesson add|retire`, delivery to
worker and reviewer, and the nudge hook. The after-count reruns
`t01-repeat-baseline.py` 14 days after that release.

## Not yet specified

- Whether findings from reviews outside relais (implementation-review at
  worktree-task F4b, the plan review panel) feed the same channel.
- How a lesson's effect is attributed in the after-count when routing, the
  model and the base also changed in the same period.

## Out of scope

- [Lessons across repositories](t08-cross-repo-lessons.md): the person chose
  same-repository siblings.
