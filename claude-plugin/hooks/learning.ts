// The learning loop's plugin side (plan 2026-10-10-learning-loop, Phase 2):
// the detached `relais learn cycle --if-due`, the one pane line a new
// promotion, rollback or error earns, and /relais-learning. The cycle is
// relais's own work in the background: not a run, so no Child, no
// `--protocol`, no announcement and no outcome message.

import { note } from './agents.ts'
import { NOT_NOW } from './consent.ts'
import type { Fx } from './fx.ts'
import type { Store } from './store.ts'

const LAST_TIMEOUT_MS = 5_000
const COMMAND_TIMEOUT_MS = 10_000
const NOTED_KEY = 'learning.noted'

export const LEARNING_YES = 'Allow automatic promotion'

// Where relais keeps its state: `$RELAIS_STATE_DIR`, else
// `~/.local/state/relais` (crates/relais/src/paths.rs).
export function stateDir(env: { home?: unknown; stateDir?: unknown }): string | undefined {
  if (typeof env.stateDir === 'string' && env.stateDir !== '') return env.stateDir.replace(/\/+$/, '')
  if (typeof env.home === 'string' && env.home !== '') return `${env.home.replace(/\/+$/, '')}/.local/state/relais`
  return undefined
}

// Starts `relais learn cycle --if-due` and lets it go: not a Child, and any
// failure is swallowed. Callers detach it.
export async function startCycle(fx: Fx, store: Store): Promise<void> {
  try {
    const dir = stateDir({ home: await fx.env.home(), stateDir: await fx.env.stateDir() })
    if (dir === undefined) return
    const session = await fx.session.id()
    const request = {
      argv: ['relais', 'learn', 'cycle', '--if-due', '--log', `${dir}/learn-cycle.log`],
      ...(store.router.cwd ? { cwd: store.router.cwd } : {}),
      env: { RELAIS_HOST: 'claude-code-mod', RELAIS_SESSION_ID: session },
    }
    const stream = fx.process.spawn(request)
    void (async () => {
      try {
        let step = await stream.next()
        while (!step.done) step = await stream.next()
      } catch {
        // Fail open: a cycle that did not start is tried again at the next session.
      }
    })()
  } catch {
    // Fail open.
  }
}

const SAYS: Record<string, string> = {
  promoted:
    'relais learned: a new routing artifact was promoted automatically. See `relais report`; undo it with `relais rollback`.',
  rolled_back: 'relais learned: the active routing artifact was rolled back. See `relais report`.',
  error: 'relais learned: the last learning cycle ended in an error. See `relais report`.',
}

// One pane line when the latest cycle row is a promotion, a rollback or an
// error this plugin has not noted yet. Fails open.
export async function noteLastCycle(fx: Fx, store: Store): Promise<void> {
  try {
    const result = await fx.process.run(['relais', 'learn', 'last', '--json'], {
      ...(store.router.cwd ? { cwd: store.router.cwd } : {}),
      timeoutMs: LAST_TIMEOUT_MS,
    })
    if (result?.exitCode !== 0) return
    const row = (JSON.parse(String(result.stdout ?? '')) as { last?: { id?: unknown; action?: unknown } | null }).last
    if (!row || typeof row.id !== 'number' || typeof row.action !== 'string') return
    const text = SAYS[row.action]
    if (!text) return
    const seen = await fx.kv.get(NOTED_KEY)
    const before = typeof seen === 'number' ? seen : typeof seen?.value === 'number' ? seen.value : undefined
    if (before !== undefined && row.id <= before) return
    await fx.kv.set(NOTED_KEY, row.id)
    if (Object.keys(store.models).length > 0) await note(fx, store, undefined, text)
    else fx.ui.toast(text)
  } catch {
    // Fail open: a line not shown is never a reason to fail the session.
  }
}

export function learningQuestion(): string {
  return [
    'relais · Allow automatic promotion of learned routing?',
    '- once a day, from a session start or end, relais trains on your recorded runs;',
    '- an artifact that passes every evaluation gate becomes active without asking;',
    '- it is rolled back on a revert, a confirmed regression, or acceptance below its floor.',
    'Turn it off with /relais-learning off. Undo one promotion with relais rollback.',
  ].join('\n')
}

// `/relais-learning on|off`: records or removes the envelope.
export async function learningCommand(fx: Fx, store: Store, args = ''): Promise<string> {
  const session = await fx.session.id()
  const by = `the person in Claude Code session ${session}`
  const run = (argv: string[]) =>
    fx.process.run(argv, { ...(store.router.cwd ? { cwd: store.router.cwd } : {}), timeoutMs: COMMAND_TIMEOUT_MS })
  const failed = (result: any) => String(result?.stderr ?? '').trim().slice(0, 600)
  const arg = args.trim()
  if (arg === 'off') {
    const result = await run(['relais', 'native', 'learning-envelope', '--off', '--by', by, '--source', 'plugin-ask'])
    if (result?.exitCode !== 0) return `relais could not turn automatic promotion off (exit ${result?.exitCode}): ${failed(result)}.`
    return 'Automatic promotion is off: a passing artifact waits for `relais promote`, until you run /relais-learning on.'
  }
  if (arg !== 'on' && arg !== '') return 'Usage: /relais-learning on | off'
  let answer: string | undefined
  try {
    answer = await fx.ui.ask(learningQuestion(), [NOT_NOW, LEARNING_YES])
  } catch {
    answer = undefined
  }
  if (answer !== LEARNING_YES) return 'Automatic promotion was not granted. Nothing was written.'
  const result = await run(['relais', 'native', 'learning-envelope', '--by', by, '--source', 'plugin-ask'])
  if (result?.exitCode !== 0) return `relais could not record the envelope (exit ${result?.exitCode}): ${failed(result)}. Nothing was granted.`
  return 'Automatic promotion granted. Turn it off with /relais-learning off.'
}
