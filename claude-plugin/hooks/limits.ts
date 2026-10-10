// The transcript's limits, shared by the layout (pane) and the reader
// (transcript). A leaf: it imports nothing, so neither module reaches the
// other through it.

// holds-until: a transcript the person wants to read whole in the pane;
// the full one is Claude Code's own view (`↓ to manage`).
export const MAX_TRANSCRIPT_ROWS = 400
