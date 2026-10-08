// relais as a Claude Code plugin: it starts relais runs and shows every
// agent they dispatch as a native one, with the run's timeline live in a
// pane. relais keeps the run, verification and the decisions; this module
// spawns, continues, stops and shows. See ../README.md.

import { dispatchOfDescription, onAgentTool, onTurn } from './agents.ts'
import { HELLO_EVERY_MS, sendHello } from './callbacks.ts'
import type { Fx } from './fx.ts'
import { DENY_MESSAGE, isRelaisNotification, isRelaisType, relaisAddresses } from './guards.ts'
import { childOf, contractPath, onChunk, pump, reloadTimeline, startReplay, startRun, statusOf } from './runs.ts'
import { onboardTool, onPersonPrompt, trustTool, withRoutingSection } from './consent.ts'
import { machineSettingsGuard, routerEnvelopeGuard } from './guards.ts'
import {
  afterStep,
  beforeStep,
  flag,
  onPrompt,
  onSessionEnd,
  onToolResult,
  onTurnComplete,
  refreshState,
  routeSpawn,
  routingCommand,
} from './routing.ts'
import { EDIT_TOOLS } from './router.ts'
import { close, createStore, detach, every, type Store } from './store.ts'
import { FLUSH_MS, openPane, renderPane, timelineLines } from './ui.ts'

const RUN_TOOL = 'mcp__relais__run'
const REPLAY_TOOL = 'mcp__relais__replay'
const STATUS_TOOL = 'mcp__relais__status'
const ONBOARD_TOOL = 'mcp__relais__onboard'
const TRUST_TOOL = 'mcp__relais__trust'
const CONSENT_FAILED = 'declined: relais could not ask the person (internal error). Nothing was written or allowed.'
// The prompts a person makes: typed, through the bridge, or a headless
// run's own. Every other origin (notifications, schedules, peers, plugins)
// is never classified.
const PERSON_ORIGINS = new Set(['composer', 'bridge', 'sdk'])

// Every effect the sibling modules use, spelled on `$` here: `claude plugin
// validate` follows `$` within a file, not across an import.
const effects = ($: any): Fx => ({
  session: { id: () => $.session.id(), model: () => $.session.model() },
  process: {
    run: (argv: string[], init: unknown) => $.process.run(argv, init),
    spawn: (request: unknown) => $.process.spawn(request),
  },
  agent: {
    spawn: (request: unknown) => $.agent.spawn(request),
    list: () => $.agent.list(),
  },
  tool: { call: (input: unknown) => $.tool.call(input) },
  clock: {
    now: () => $.clock.now(),
    every: (ms: number, fn: () => void) => $.clock.every(ms, fn),
    after: (ms: number, fn: () => void) => $.clock.after(ms, fn),
  },
  ui: {
    open: (pane: unknown) => $.ui.open(pane),
    toast: (text: string) => $.ui.toast(text),
    status: (text: string | undefined) => $.ui.status(text),
    resolve: (e: unknown) => $.ui.resolve(e),
    ask: (question: string, options: string[]) => $.ui.ask(question, options),
  },
  prompt: { submit: (args: unknown) => $.prompt.submit(args) },
  pane: {
    read: () => $.state.get({ plugin: 'relais', key: 'pane' }),
    write: (value: unknown) => $.state.set({ plugin: 'relais', key: 'pane' }, value),
  },
  model: { complete: (request: unknown) => $.model.complete(request) },
  kv: {
    get: (key: string) => $.store.get(key),
    set: (key: string, value: unknown) => $.store.set(key, value),
  },
})

// Whether a spawn is relais's own: a pending dispatch, or this plugin's own
// call; the directory it runs in when it is both.
function relaisSpawn(store: Store, e: any, isOurs: boolean) {
  const dispatch = dispatchOfDescription(e.description)
  const pending = dispatch ? store.pending.get(dispatch) : undefined
  return { isRelais: isOurs || pending !== undefined, cwd: isOurs && pending ? pending.cwd : undefined }
}

export function register(on: any) {
  const store = createStore()

  // The timer that writes the pane's state and sends the verdicts as prompts.
  // Made in `session.start`: a prompt cannot be submitted from under a tool hook.
  const ensurePump = (fx: Fx) => {
    if (store.hasPump) return
    store.hasPump = true
    every(fx, store, FLUSH_MS, () => detach(pump(fx, store)))
  }

  on('session.start', async ($: any, e: any, next: any) => {
    const fx = effects($)
    store.sessions.add(await fx.session.id())
    // The router's state is read in the background; the first prompt waits
    // for it (routing.ts). Until then, and on any failure, nothing is routed.
    if (typeof e.cwd === 'string') store.router.cwd = e.cwd
    detach(refreshState(fx, store))
    await $.tool.register({
      name: 'run',
      description:
        'Use this instead of editing files yourself for any bounded implementation or inspection task (one objective, a write scope, acceptance a command can verify): relais picks the model, works in an isolated worktree with native agents, verifies the result with the repository\'s checks and reports the outcome. Works in any repository; one not set up yet is set up with the person on the first run. Returns at once; the outcome arrives as a message.',
      inputSchema: {
        type: 'object',
        properties: {
          task: {
            type: 'object',
            description:
              'The task contract: {schema_version: 1, kind: "change" | "inspect", objective, base_ref: "HEAD", write_scope: [globs], read_hints: [paths], acceptance: [criteria a command can verify], verification_profile: "default", review: "optional"}. A path to a contract .json file is also accepted.',
          },
          cwd: { type: 'string', description: 'The repository to work in (absolute path).' },
        },
        required: ['task', 'cwd'],
      },
    })
    await $.tool.register({
      name: 'onboard',
      description:
        'Set relais up in a repository that has no relais.toml (a run reported no_policy): relais proposes the checks from the repository, and the person is asked to use them and to allow relais to run them. Returns ready, declined or not set up.',
      inputSchema: {
        type: 'object',
        properties: { cwd: { type: 'string', description: 'The repository (absolute path).' } },
        required: ['cwd'],
      },
    })
    await $.tool.register({
      name: 'trust',
      description:
        "Ask the person to allow the commands of a repository's relais.toml (a run reported missing_trust_grant). The person sees the exact commands. Returns ready or declined.",
      inputSchema: {
        type: 'object',
        properties: { cwd: { type: 'string', description: 'The repository (absolute path).' } },
        required: ['cwd'],
      },
    })
    await $.tool.register({
      name: 'replay',
      description:
        "Replay a task's accepted relais run under a candidate recipe, in a scratch checkout, with native agents: one arm's result, recorded as a trial. Spends real money. Returns at once; the outcome arrives as a message.",
      inputSchema: {
        type: 'object',
        properties: {
          task: { type: 'string', description: 'The id of the task whose accepted run to replay.' },
          recipe: { type: 'string', description: 'Path to the candidate relais.toml.' },
          cwd: { type: 'string', description: 'The repository to work in (absolute path).' },
        },
        required: ['task', 'recipe', 'cwd'],
      },
    })
    await $.tool.register({
      name: 'status',
      description:
        "The phases, decisions, cost and latest output of relais runs: one run by id, or all of this session's.",
      inputSchema: {
        type: 'object',
        properties: { run: { type: 'string', description: 'A run id; all runs when left out.' } },
      },
    })
    await $.command.register({
      name: 'relais-status',
      description: "Open the relais pane: this session's runs, live.",
    })
    await $.command.register({
      name: 'relais-routing',
      description: "Route the session model by the task (asks you; shows what it allows). '/relais-routing off' turns it off.",
    })
    await $.command.register({
      name: 'relais-flag',
      description: 'Mark the current task as wrong: relais moves it to a stronger model from the next request.',
    })
    ensurePump(fx)
    detach(sendHello(fx, store))
    every(fx, store, HELLO_EVERY_MS, () => detach(sendHello(fx, store)))
    return next(e)
  })

  on('session.end', async ($: any, e: any, next: any) => {
    // The router's open tasks and observations go first, at every reason.
    try {
      const fx = effects($)
      await onSessionEnd(fx, store, String(e.reason), await fx.clock.now())
    } catch {
      // Fail open: the session ends whatever the router could send.
    }
    // /clear and /resume go on in this module under a new session id.
    if (e.reason !== 'clear' && e.reason !== 'resume') close(store)
    return next(e)
  }).catch(($: any, e: any, next: any) => {
    if (e.reason !== 'clear' && e.reason !== 'resume') close(store)
    return next(e)
  })

  on('tool.call', { tool: RUN_TOOL }, async ($: any, e: any) => {
    if (typeof e.cwd !== 'string') return { deny: 'relais run needs a cwd: the repository, as an absolute path.' }
    const fx = effects($)
    const task = await contractPath(fx, e.task, e.cwd)
    if (task.deny) return { deny: task.deny }
    ensurePump(fx)
    return { result: await startRun(fx, store, { task: task.path, cwd: e.cwd }) }
  })

  // Both ask the person from inside the tool call; anything that throws is
  // a refusal, never a pass.
  on('tool.call', { tool: ONBOARD_TOOL }, async ($: any, e: any) => {
    if (typeof e.cwd !== 'string') return { deny: 'relais onboard needs a cwd (absolute path).' }
    const fx = effects($)
    return { result: await onboardTool(fx, store, e.cwd, await fx.session.id()) }
  }).catch(() => ({ result: CONSENT_FAILED }))

  on('tool.call', { tool: TRUST_TOOL }, async ($: any, e: any) => {
    if (typeof e.cwd !== 'string') return { deny: 'relais trust needs a cwd (absolute path).' }
    const fx = effects($)
    return { result: await trustTool(fx, store, e.cwd, await fx.session.id()) }
  }).catch(() => ({ result: CONSENT_FAILED }))

  // machine.toml holds the grants: the model does not write it, and is
  // told so when a Bash command names it (a reminder, not a wall: SPEC §5).
  // The same for the router's envelope: it is the person's, recorded by
  // `relais install --claude` or /relais-routing, never by the model.
  on('tool.check', { tool: ['Write', 'Edit', 'MultiEdit', 'NotebookEdit', 'Bash'] }, async ($: any, e: any, next: any) => {
    const decided = await next(e)
    const home = await $.env.get('HOME')
    const configDir = await $.env.get('RELAIS_CONFIG_DIR')
    const input = e.input ?? {}
    const refusal = machineSettingsGuard(e.tool, input, { home, configDir }) ?? routerEnvelopeGuard(e.tool, input)
    return refusal ? { decision: 'deny', reason: refusal } : decided
  })

  on('tool.call', { tool: REPLAY_TOOL }, async ($: any, e: any) => {
    if (typeof e.task !== 'string' || typeof e.recipe !== 'string' || typeof e.cwd !== 'string') {
      return { deny: 'relais replay needs a task, a recipe and a cwd (all text).' }
    }
    const fx = effects($)
    ensurePump(fx)
    return { result: await startReplay(fx, store, { task: e.task, recipe: e.recipe, cwd: e.cwd }) }
  })

  on('tool.call', { tool: STATUS_TOOL }, async ($: any, e: any) => ({
    result: await statusOf(effects($), store, typeof e.run === 'string' ? e.run : undefined),
  }))

  on('command.run', { command: 'relais-status' }, async ($: any) => {
    const fx = effects($)
    if (Object.keys(store.models).length === 0) await reloadTimeline(fx, store)
    const opened = await openPane(fx, store)
    if (opened.isPlaced) return { text: 'relais pane opened.' }
    const lines = timelineLines(store, await fx.clock.now())
    return { text: lines.length > 0 ? lines.join('\n') : 'No relais run is known in this session.' }
  })

  // The child's output passes through here, and what relais asks for in it
  // (spawn, continue, stop) is done from inside this hook: a call made from a
  // detached loop would skip this plugin's own `agent.spawn` hook, which is
  // what sets each agent's directory. Chunks are forwarded unchanged.
  on('process.spawn', async function* ($: any, e: any, next: any) {
    const child = childOf(store, e)
    if (!child) return yield* next(e)
    const fx = effects($)
    const carry = { out: '', err: '' }
    const stream = next(e)
    let step = await stream.next()
    while (!step.done) {
      await onChunk(fx, store, child, carry, step.value)
      yield step.value
      step = await stream.next()
    }
    return step.value
  })

  // Runs in the directory relais chose for the dispatch: the attempt's
  // worktree, the review directory, the repo. Only this plugin's own spawn
  // of a pending dispatch is touched.
  //
  // Every other spawn is the session router's (routing.ts): classified and,
  // within the envelope, given a model. relais's own (a pending dispatch,
  // or this plugin's own call) are never routed.
  on('agent.spawn', async ($: any, e: any, next: any) => {
    const own = relaisSpawn(store, e, next.origin.plugin === $.plugin.name)
    if (own.cwd !== undefined) return next({ ...e, cwd: own.cwd })
    if (own.isRelais) return next(e)
    return routeSpawn(effects($), store, e, next, next.budget.remainingMs)
  }).catch(($: any, e: any, next: any) => {
    // After `next` the subagent exists: calling it again would start a
    // second one. Claude Code keeps the result `next` resolved to.
    if (next.called) throw next.error ?? new Error('relais: agent.spawn failed after the spawn')
    const own = relaisSpawn(store, e, next.origin.plugin === $.plugin.name)
    return own.cwd !== undefined ? next({ ...e, cwd: own.cwd }) : next(e)
  })

  // The session router: the turn's first request pins the decision, every
  // request is rewritten only when the router decided for its loop and the
  // mode is on; each response's usage is recorded. Fails open to `next(e)`.
  on('turn.step', async function* ($: any, e: any, next: any) {
    const fx = effects($)
    let routed = e
    try {
      routed = await beforeStep(fx, store, e, next.budget.remainingMs)
    } catch {
      routed = e
    }
    const result = yield* next(routed)
    try {
      afterStep(store, e, result, await fx.clock.now())
    } catch {
      // Usage is a record, never a reason to fail the step.
    }
    return result
  }).catch(async function* (_$: any, e: any, next: any) {
    return yield* next(e)
  })

  // Evidence for recovery: edits, and the checks' results. The tool's own
  // answer passes unchanged.
  on('tool.call', { tool: ['Bash', ...EDIT_TOOLS] }, async ($: any, e: any, next: any) => {
    const result = await next(e)
    try {
      const fx = effects($)
      onToolResult(fx, store, e, result, await fx.clock.now())
    } catch {
      // Fail open.
    }
    return result
  }).catch(($: any, e: any, next: any) => next(e))

  // A relais agent's tool calls, as the pane's live lines for its step.
  // Every other call passes untouched; the call itself is never changed.
  on('tool.call', async ($: any, e: any, next: any) => {
    if (typeof e.agentId === 'string' && store.agentDispatch.has(e.agentId)) {
      try {
        onAgentTool(store, e, await $.clock.now())
      } catch {
        // Fail open: a line not shown is never a reason to fail the call.
      }
    }
    return next(e)
  }).catch(($: any, e: any, next: any) => {
    // After `next` the tool has run: calling it again would run it twice.
    if (next.called) throw next.error ?? new Error('relais: tool.call failed after the call')
    return next(e)
  })

  on('turn.complete', async ($: any, e: any, next: any) => {
    const fx = effects($)
    onTurn(fx, store, e)
    try {
      onTurnComplete(store, e, await fx.clock.now())
    } catch {
      // Fail open.
    }
    return next(e)
  })

  // Hidden from the model; fails closed. While this module resumes its own
  // agent the type is let through, or Claude Code refuses the resume.
  on('agent.offer', (_$: any, e: any, next: any) =>
    isRelaisType(e.agent) && store.resuming === 0 ? { isOffered: false } : next(e),
  ).catch((_$: any, e: any, next: any) => (isRelaisType(e.agent) ? { isOffered: false } : next(e)))

  // The model may not message or stop a relais agent; this module may.
  on('tool.call', { tool: ['SendMessage', 'TaskStop'] }, async ($: any, e: any, next: any) => {
    if (next.origin.plugin === $.plugin.name) return next(e)
    const target = String(e.tool === 'SendMessage' ? e.to : e.task_id)
    const addresses = relaisAddresses(await $.agent.list(), $.plugin.name)
    return addresses.has(target) || store.agentDispatch.has(target) ? { deny: DENY_MESSAGE } : next(e)
  }).catch(($: any, e: any, next: any) => {
    const target = String(e.tool === 'SendMessage' ? e.to : e.task_id)
    return store.agentDispatch.has(target) ? { deny: DENY_MESSAGE } : next(e)
  })

  // relais's agents' completion notices are for relais; any other prompt,
  // one that only mentions an id included, is kept.
  on('prompt.submit', async ($: any, e: any, next: any) => {
    if (e.origin?.kind !== 'task-notification') {
      // Whether this plugin submitted it is read before onPersonPrompt
      // consumes the mark.
      const isOwn = typeof e.text === 'string' && store.ownPrompts.has(e.text)
      onPersonPrompt(store, e.text)
      if (!isOwn && PERSON_ORIGINS.has(e.origin?.kind) && typeof e.text === 'string') {
        try {
          await onPrompt(effects($), store, e.text)
        } catch {
          // Fail open: the prompt goes on unrouted.
        }
      }
      return next(e)
    }
    const ids = new Set(store.agentDispatch.keys())
    for (const id of relaisAddresses(await $.agent.list(), $.plugin.name)) ids.add(id)
    return isRelaisNotification(e.text, ids) ? { drop: 'relais agent notification' } : next(e)
  })

  // The routing rule as a section of the system prompt: a note attached to
  // the person's prompt reaches the model but is read as a hook's aside.
  on('prompt.compose', async (_$: any, e: any, next: any) => withRoutingSection(await next(e)))

  on('command.run', { command: 'relais-routing' }, async ($: any, e: any) => ({
    text: await routingCommand(effects($), store, typeof e?.args === 'string' ? e.args : ''),
  })).catch(() => ({ text: 'relais could not change session routing (internal error). Nothing was written.' }))

  on('command.run', { command: 'relais-flag' }, async ($: any) => ({
    text: await flag(effects($), store),
  })).catch(() => ({ text: 'relais could not flag the task (internal error).' }))

  on('ui.render', { component: 'Pane', requestId: 'relais' }, ($: any, e: any) =>
    renderPane(effects($), e),
  )

  // After /clear, /resume or /branch the module's buffers may be gone or stale.
  on('classic.SessionStart', async ($: any, e: any, next: any) => {
    if (e.source === 'clear' || e.source === 'resume' || e.source === 'fork') {
      await reloadTimeline(effects($), store)
    }
    return next(e)
  })
}
