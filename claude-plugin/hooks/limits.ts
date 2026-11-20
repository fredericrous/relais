// The transcript's limits, shared by the layout (pane) and the reader
// (transcript). A leaf: it imports nothing, so neither module reaches the
// other through it.

// holds-until: a transcript the person wants to read whole in the pane;
// the full one is Claude Code's own view (`↓ to manage`).
export const MAX_TRANSCRIPT_ROWS = 400

// The pre-clear tail's budget, in code points: the clear-tail capture keeps
// this many, and the pane's tail view says so once the oldest was cut.
export const TAIL_CHARS = 2000
