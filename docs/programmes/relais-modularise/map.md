# relais becomes a workspace of smaller crates, then is renamed once

## Destination

relais (today a workspace with one member, `crates/relais`) is a workspace
of several member crates with enforced boundaries. Each piece that is a
separate tool has its own name, chosen once, and the rename to the family
name happens once. Done when two targets, set at t01 from its baseline, are
met: the build time after a one-line edit, and the size of the largest file.

## Notes

- **Origin:** session of 2026-11-26. The rename to greenlift has been on hold
  since 2026-11-17 ("let's wait for relais"). The person asked whether relais
  does too many things before it is renamed, so each piece is named once.
- **Size:** 229,504 lines in one crate (`crates/relais`). The `mod.rs` files
  of `verify` (7.2k), `admission` (6.4k), `coordinator` (5.3k) and `report`
  (5.1k) are the largest, and include inline tests until t01 counts them
  apart.
- **Module groups seen from the layout:** routing and learning (`route`,
  `router`, `learn`, `money`, `native_pricing`); execution (`runner`,
  `workspace`, `adapter`, `backend`, `native`); verification (`verify`,
  `holdout`, `acceptance`, `contract`); host scheduling (`admission`,
  `coordinator`, `procs`, which overlaps greengate's slot queue, ADR-0014);
  the programme module (`programme`, `cli/programme.rs`); the plugin side
  (`hook`, `install`, `doctor`); state (`ledger`, `storage_full`, `report`).
- **Baseline to record when charting t01:** the build time after a one-line
  edit, and the largest file's line count.
- **Name:** the family name is greenlift (ADR-0076, `harness.bundle-names`).
  A memory note that says greenguide is wrong: decisions#174 rejected it.
- **Whether the tool rename and the vocabulary migration of
  `epic-vocabulary` ship in one window is open.** t04 asks it. `blocked_by`
  cannot name another programme, so t04 reads that programme's t01 answer by
  hand.
- **Governing decisions:** resolve before deciding a ticket:
  `harness.bundle-names` (ADR-0076), `harness.tool-boundaries` (head: decisions ADR-0075),
  `work.programme-map` (ADR-0084).
- **Neighbouring programmes:** relais `docs/programmes/epic-vocabulary`
  (the word programme becomes epic), `homelab-offload` and `phase8-learner`
  touch the same crate.
- **Cost:** t01 and t03 are read-only research, through relais `inspect` or
  an Explore agent.

## Decisions so far

- Git is the source of truth and activity-vision projects it (decided in
  `epic-vocabulary`).
- "Epic" and "story" are the new words for programme and ticket (decided in
  `epic-vocabulary`).
- Baseline and coupling map: a one-line edit rebuilds in 23–29 s, the largest
  production file is `verify/mod.rs`, and `ids`, `lifecycle`, `policy` and
  `money` are the lowest layer ([t01](t01-coupling-map.md)).
- Host scheduling is not a deletion: relais already uses greengate's slot
  queue, and only about 105 lines overlap (a wait loop that needs a cancel
  seam in `wait_in_line`, and `load_average`), so `admission`, `coordinator`
  and `procs` stay relais's own and t02 weighs them as a crate
  ([t03](t03-slot-overlap.md)).
- Crates and order: the production module graph has no cycle (only rustdoc
  links close one), so eight crates are cut bottom-up (`core`, `policy`,
  `admission`, `ledger`, `routing`, `verify`, `report`, then `programme`
  after the epic-vocabulary rename), the rest staying in the binary. No crate
  becomes its own tool; the stories wait for t04's names. Targets: a one-line
  edit in `verify/` or `runner/` rebuilds in ≤ 10 s, and no production file
  exceeds 700 non-blank lines (56 files do today)
  ([t02](t02-seams.md)).
- Wait loop: relais waits for a host slot through greengate's one loop,
  `greenharness-slots` 5.5's `wait_in_line_paced`, where the caller owns the
  pause between polls. `load_average` gives way to `load1`. About 12
  production lines net came out: one loop, little code saved
  ([t05](t05-wait-loop-cancel-seam.md)).

## Handoff

2026-11-27, boundary: t05 closed (the wait-loop seam).

- Done: t01 (#619), t03 (#621), t02 (#639), t05: greenharness/greengate#538
  and its 5.5.0 release (#540), and relais on this change.
- Live state: nothing in flight for this programme. epic-vocabulary's t01
  is still open, and its rename must land before the `programme` crate is
  cut.
- Next, in order: t04 (names, `hitl`: needs the person), then the split
  stories once named. t06 (what slots-off means, `hitl`) is ready. t07
  (a week of slot waits through greengate's loop) is due 2026-12-08.
- Accepted gaps:
  - The rebuild baseline is still t01's busy-host sample; the first split
    story re-measures it on a quiet host.
  - t05's 360 s live pilot was not run. A heartbeat-per-pause test stands in
    for it.
- Lessons:
  - A module graph read with comments included shows cycles that are only
    rustdoc links; drop comment lines before judging layers.
  - A pack refresh written with this Mac's greentopo 3.0.0 fails CI's 2.0.0
    heads-fresh check; regenerate with the pinned version until follow-ups
    f36 lands.

## Not yet specified

- Workspace-split implementation stories, one per crate in t02's order,
  after t04 names them. The first re-measures the rebuild baseline on a quiet
  host; the rustdoc-link cleanup and the `workspace::git_command` move come
  before any cut.
- File-split stories for the 56 production files over 700 lines, each after
  the crate it sits in is cut; t01's job sections for `verify`, `admission`,
  `coordinator` and `report` are where the four largest start. Whether a
  check enforces the 700-line ceiling (a ratchet over the files still above
  it) is open.

## Out of scope

- Splitting relais into separate repositories before the workspace split
  proves the seams.
- Moving any state out of git.
