// The one value the plugin keeps in `$.state`: what the pane draws. The
// stream handler writes it (one write per 100 ms tick); the `ui.render`
// hook only reads it.

export type RelaisPaneState = {
  // One model per run, as hooks/timeline.ts builds them.
  runs: unknown[]
  // The clock when it was written, in ms: elapsed times are drawn from it.
  now: number
  // What the pane shows: the runs, or one agent's transcript (the render
  // derives everything from this, never from the engine's `props.view`).
  shown: RelaisPaneShown
}

// A transcript row, laid out at the pane's width and sanitised before it is stored.
export type RelaisTranscriptRow = { text: string; dim?: boolean; color?: string }

export type RelaisPaneShown =
  | { kind: 'runs' }
  | {
      kind: 'agent'
      agentId: string
      run: string
      transcript:
        | { kind: 'loading' }
        // `note` is the dim deny or slow line a later read added over the rows.
        | { kind: 'rows'; rows: RelaisTranscriptRow[]; note?: string }
        | { kind: 'deny'; reason: string }
        | { kind: 'slow' }
    }

declare module 'claude-code' {
  interface PluginState {
    relais: { pane: RelaisPaneState }
  }
}
