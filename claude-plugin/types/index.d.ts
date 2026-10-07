// The one value the plugin keeps in `$.state`: what the pane draws. The
// stream handler writes it (one write per 100 ms tick); the `ui.render`
// hook only reads it.

export type RelaisPaneState = {
  // One model per run, as hooks/timeline.ts builds them.
  runs: unknown[]
  // The clock when it was written, in ms: elapsed times are drawn from it.
  now: number
}

declare module 'claude-code' {
  interface PluginState {
    relais: { pane: RelaisPaneState }
  }
}
