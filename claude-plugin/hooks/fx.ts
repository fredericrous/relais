// The effects this module reaches, as register.ts builds them from each
// hook's `$`. `claude plugin validate` follows `$` only inside one file, so
// every call on `$` is spelled there, and the sibling modules take this
// object instead. Tests of the pure modules pass their own.

type Call = (...args: any[]) => any

export type Fx = {
  session: { id: Call }
  process: { run: Call; spawn: Call }
  agent: { spawn: Call; list: Call }
  tool: { call: Call }
  clock: { now: Call; every: Call; after: Call }
  ui: { open: Call; toast: Call; status: Call; resolve: Call }
  prompt: { submit: Call }
  // The pane's one `$.state` value: written by the stream handler's flush,
  // read by the `ui.render` hook.
  pane: { read: Call; write: Call }
}
