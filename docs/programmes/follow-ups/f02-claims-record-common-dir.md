+++
type = "task"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais"]
+++
# Claims record the git common dir

## Question

Should a claim record its repository's git common dir, so `relais programme session` finds the map after the task worktree that claimed it was removed, without `--dir`?

## Look at

`crates/relais/src/programme/claim/rules.rs` (`ClaimRecord.repo` is a display path), `cli/programme.rs` `session_map_dir`, and `docs/plans/2026-11-20-pane-shows-programme-and-clear.md` Decision log.

## Answer looks like

A decision, and if yes a new optional field written by `claim` / `next --claim`, read first by `session_map_dir`, with a test on a removed worktree.

## Resolution

2026-11-20, by hand (a code read plus one integration test, no relais run):
**no new field.** The question's premise does not hold on main.

- A claim's `repo` already names the main checkout. Since #430
  (2026-11-10), `Programme::load` maps its directory through
  `load::main_checkout` (`programme/load.rs:122`), and `load_at` is only
  given main checkouts. `programme_state` hands that `repo_dir` to
  `claim_work` and `reserve_handoff`. A `claim --dir <task worktree>`
  therefore records `<main checkout>`, which removing the worktree leaves
  in place.
- Recording the common dir would add nothing. For a normal checkout it is
  `<main checkout>/.git`, so it moves or disappears together with the path
  already recorded.
- The plan's Decision log line ("a removed worktree with no `--dir` …
  still reads 'the repository is gone'") was written against a claim whose
  `repo` a test had rewritten to a worktree path. A real claim does not
  produce that path.
- Pinned by `a_claim_from_a_removed_worktree_still_finds_the_map`
  (`tests/programme_session.rs`): it claims from a linked worktree, runs
  `git worktree remove --force`, then gets the map from `session` with a
  `--dir` outside any repository. It passed on unchanged main (4f720fd1).
- "The repository is gone" still means the main checkout itself moved. That
  is covered by the test
  `text_mode_with_a_gone_repository_names_a_checkout_for_both_choices`,
  and `--dir` is still the fix for it.
- Left as is: `session_map_dir`'s hop from a recorded worktree to its main
  checkout now serves only hand-edited or pre-#430 claims. It is cheap, so
  it stays.
