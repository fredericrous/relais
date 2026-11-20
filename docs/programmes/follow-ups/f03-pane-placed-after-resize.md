+++
type = "task"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais"]
+++
# The pre-clear status hint misses a resize

## Question

Should the plugin learn that its pane stopped being placed after a terminal resize, so the status line says `before /clear saved · open the relais pane` when the pane is hidden?

## Look at

`claude-plugin/hooks/ui.ts` (`store.isPanePlaced`, set only by `openPane`) and whether the mod API tells a plugin when a pane is unplaced; live check 2026-11-20: at 100 columns the pane hid and the status kept `▶ demo t08-count` only.

## Answer looks like

Either the plugin reads placement at render time and the hint appears after a resize (checked in tmux at 180 → 100 columns), or a note that the API gives no such signal.

## Resolution

2026-11-20, by hand (live checks in tmux, no relais run): **no change.** The
ticket's premise did not reproduce on Claude Code 2.1.329, and the API
gives no unplace signal to read.

- A placed pane is not hidden by a narrowing resize. Below 110 columns the
  engine moves it from the dock (fullscreen, beside the transcript) to the
  inline seat above the prompt, framed and drawn. `statusText` leaves the
  tail hint out because the tail is on screen, which is correct.
- `$.ui.panes()` kept `isPlaced: true` through 180 → 100 → 180, read every
  500 ms by a scratch probe mod with an unasked pane, both one row and 40
  rows tall (14 rows shown inline at 100). The `UiPane` doc promises only
  false → true: a pane opened unasked below the floor turns placed when the
  person opens it or the terminal widens. So an `isPanePlaced` poll would
  never change after a resize.
- The relais pane itself, through `--plugin-dir claude-plugin` at 739932af
  in a scratch repo holding a demo programme: the claimed ticket's window
  and the `before /clear` tail were docked at 180. At 100 both were drawn
  inline, both right after the resize and after a later turn that ran Bash
  (no `hook skipped` line). At 180 the pane docked again. The status line
  showed `▶ demo f03-pane-placed-after-resize` throughout.
- The plan's "live, narrow" row (`docs/plans/2026-11-20-pane-shows-programme-and-clear.md`)
  is therefore not reproduced by these steps. The merged plan is left as
  written.
- Out of scope: a placed pane that is a background tab (`isShown` false)
  also keeps the tail off screen without the hint. File it if it is ever
  seen.
