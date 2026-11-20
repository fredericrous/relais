+++
type = "task"
mode = "afk"
status = "open"
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
