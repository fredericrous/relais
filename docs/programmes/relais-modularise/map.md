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
  `harness.bundle-names` (ADR-0076), `harness.tool-boundaries` (ADR-0064),
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

## Not yet specified

- Workspace-split implementation stories, one per crate, after t02.
- Their order against the `epic-vocabulary` rename implementation: both
  rewrite `crates/relais/src/programme/` and `cli/programme.rs`, so one lands
  before the other starts. t02 owns the answer.

## Out of scope

- Splitting relais into separate repositories before the workspace split
  proves the seams.
- Moving any state out of git.
