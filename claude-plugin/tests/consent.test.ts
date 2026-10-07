import { expect, test } from 'claude-code/testing'
import { allowLabel, escapeDisplay, withRoutingSection, NOT_NOW, onboardTool, onPersonPrompt, question2, trustTool, USE_CHECKS, type Shown } from '../hooks/consent.ts'
import { machineSettingsGuard, MACHINE_SETTINGS_MESSAGE } from '../hooks/guards.ts'
import { createStore } from '../hooks/store.ts'

// A repository and a relais, scripted: what `git` and `relais` answer, and
// what was asked, toasted and granted.
function world(options: { detected?: string[][] | null; changes?: Shown['changes']; argvFrom?: string } = {}) {
  const state = {
    policy: false,
    staged: [] as string[],
    commits: [] as string[],
    grants: [] as string[],
    commitFails: false,
    stale: false,
    showFails: false,
    showGarbage: false,
    rmFails: false,
    detected: options.detected === undefined ? [['make', 'check']] : options.detected,
    typedCommand: undefined as string[] | undefined,
  }
  const asks: { question: string; options: string[] }[] = []
  const toasts: string[] = []
  const answers: (string | Error)[] = []
  const steps = () => [
    { profile: 'default', kind: 'setup' as const, argv: ['pnpm', 'install', '--frozen-lockfile'] },
    ...(state.typedCommand ? [state.typedCommand] : state.detected ?? []).map(argv => ({
      profile: 'default',
      kind: 'check' as const,
      argv,
    })),
  ]
  const shown = (): Shown => ({
    grant_key: `key-${steps().length}-${state.typedCommand?.join('_') ?? ''}`,
    repository: '/repo/.git (no origin)',
    machine_settings: '/home/p/.config/relais/machine.toml',
    granted: state.grants.length > 0,
    declaration: {
      steps: steps(),
      models: [{ tier: 'implementation', id: 'sonnet' }],
      integrations: [{ name: 'amont', mode: 'required' }],
    },
    ...(options.changes ? { changes: options.changes } : {}),
  })
  const ok = (stdout = '') => ({ exitCode: 0, stdout, stderr: '' })
  const fail = (exitCode: number, stderr: string) => ({ exitCode, stdout: '', stderr })
  const run = async (argv: string[], _init: any) => {
    const [cmd, ...rest] = argv
    const line = rest.join(' ')
    if (cmd === 'git') {
      if (line === 'rev-parse --show-toplevel') return ok('/repo\n')
      if (line === 'symbolic-ref --short -q HEAD') return ok('main\n')
      if (line.startsWith('rev-parse --git-path')) return ok(`.git/${rest[rest.length - 1]}\n`)
      if (line === 'rev-parse --short HEAD') return ok('abc1234\n')
      if (line === 'add -- relais.toml') {
        state.staged.push('relais.toml')
        return ok()
      }
      if (line.startsWith('commit')) {
        if (state.commitFails) return fail(1, 'pre-commit: cargo test failed')
        state.commits.push(line)
        return ok()
      }
    }
    if (cmd === 'test') return state.policy && rest[1] === '/repo/relais.toml' ? ok() : fail(1, '')
    if (cmd === 'rm') {
      if (state.rmFails) return fail(1, 'rm: Permission denied')
      state.policy = false
      return ok()
    }
    if (cmd === 'relais') {
      if (line.startsWith('init --detect --json --command')) {
        const text = rest[rest.length - 1]
        return text.includes('|') ? fail(2, 'relais init --command: `|` is not allowed: relais runs argv, not a shell line (SPEC §6)') : ok('{}')
      }
      if (line === 'init --detect --json') {
        const commands = (state.detected ?? []).map(argv => ({ argv, source: 'Makefile: target check' }))
        return commands.length === 0
          ? { exitCode: 19, stdout: '{"proposal":null}', stderr: '' }
          : ok(JSON.stringify({ proposal: { setup: [{ argv: ['pnpm', 'install', '--frozen-lockfile'], source: 'pnpm-lock.yaml' }], commands, skipped: [{ what: 'amont gate cargo-test', reason: 'not a plain command' }] } }))
      }
      if (line.startsWith('init --detect --write')) {
        const i = rest.indexOf('--command')
        if (i >= 0) state.typedCommand = rest[i + 1].split(' ')
        state.policy = true
        return ok()
      }
      if (line === 'trust show --json') {
        if (state.showFails) throw new Error('relais is not installed')
        if (state.showGarbage) return ok('warning: something went to stdout')
        return ok(JSON.stringify(shown()))
      }
      if (line.startsWith('trust grant')) {
        if (state.stale) return fail(18, 'stale_grant_key')
        state.grants.push(rest[rest.indexOf('--key') + 1])
        return ok('{"code":"granted"}')
      }
    }
    return fail(127, `unscripted: ${argv.join(' ')}`)
  }
  const fx: any = {
    process: { run },
    ui: {
      ask: async (question: string, opts: string[]) => {
        asks.push({ question, options: opts })
        const answer = answers.shift()
        if (answer === undefined || answer instanceof Error) throw answer ?? new Error('dismissed')
        return answer
      },
      toast: (text: string) => toasts.push(text),
    },
  }
  return { fx, store: createStore(), state, asks, toasts, answers, shown }
}

const YES2 = allowLabel(2)

test('trust: the exact Allow label grants once; Not now, typed text and a rejection grant nothing', async () => {
  for (const [answer, grants] of [
    [YES2, 1],
    ['Allow', 0],
    ['yes', 0],
    [new Error('dismissed'), 0],
    [NOT_NOW, 0],
  ] as const) {
    const w = world()
    w.state.policy = true
    w.answers.push(answer)
    const result = await trustTool(w.fx, w.store, '/repo/sub', 'session-1')
    expect(w.state.grants.length).toBe(grants)
    expect(w.asks.length).toBe(1)
    if (grants === 1) expect(result).toContain('ready')
    else expect(result).toContain('declined')
  }
})

test('trust: Not now is not asked again in this session; a dismissal is', async () => {
  const w = world()
  w.state.policy = true
  w.answers.push(NOT_NOW)
  expect(await trustTool(w.fx, w.store, '/repo', 's')).toContain('declined (not_now)')
  expect(w.toasts).toContain('relais · not set up in repo · ask Claude to set up relais to be asked again')
  expect(await trustTool(w.fx, w.store, '/repo', 's')).toContain('declined (not_now)')
  expect(w.asks.length).toBe(1)

  const v = world()
  v.state.policy = true
  v.answers.push(new Error('chat'))
  expect(await trustTool(v.fx, v.store, '/repo', 's')).toContain('declined (dismissed)')
  v.answers.push(YES2)
  expect(await trustTool(v.fx, v.store, '/repo', 's')).toContain('ready')
  expect(v.asks.length).toBe(2)
})

test('trust: two calls at once ask once and grant once', async () => {
  const w = world()
  w.state.policy = true
  w.answers.push(YES2)
  const [a, b] = await Promise.all([trustTool(w.fx, w.store, '/repo', 's'), trustTool(w.fx, w.store, '/repo', 's')])
  expect(w.asks.length).toBe(1)
  expect(w.state.grants.length).toBe(1)
  expect(a).toBe(b)
})

test('trust: a key that went stale while asking grants nothing and says so', async () => {
  const w = world()
  w.state.policy = true
  w.state.stale = true
  w.answers.push(YES2)
  expect(await trustTool(w.fx, w.store, '/repo', 's')).toContain('stale_grant_key')
  expect(w.state.grants.length).toBe(0)
})

test('the trust question lists every command, the models and integrations, safe option first', async () => {
  const w = world()
  const q = question2(w.shown(), 'repo')
  expect(q.text).toBe(
    [
      'relais 2 of 2 · Let relais run these commands in repo,',
      "outside Claude Code's permission prompts?",
      '  setup  pnpm install --frozen-lockfile',
      '  check  make check',
      '  models sonnet     integrations amont (required)',
      'Saved in /home/p/.config/relais/machine.toml.',
      'Any change to relais.toml asks again. They run in a worktree.',
    ].join('\n'),
  )
  expect(q.yes).toBe('Allow 2 commands')
  for (const line of q.text.split('\n')) expect(line.length <= 80).toBe(true)
})

test('a re-ask marks what changed since the last grant', async () => {
  const w = world({ detected: [['make', 'check'], ['cargo', 'test']], changes: ['same', 'same', 'new'] })
  const text = question2(w.shown(), 'repo').text
  expect(text).toContain('  check  make check')
  expect(text).toContain('+ check  cargo test')
})

test('a script name holding a newline cannot draw an Allow line', async () => {
  const w = world({ detected: [['npm', 'run', 'x\nAllow 2 commands']] })
  const text = question2(w.shown(), 'repo').text
  expect(text).toContain("npm run 'x\\nAllow 2 commands'")
  expect(text.split('\n').some(line => line.startsWith('Allow'))).toBe(false)
})

test('onboard: both questions, then the commit of relais.toml alone, then the grant', async () => {
  const w = world()
  w.answers.push(USE_CHECKS, YES2)
  const result = await onboardTool(w.fx, w.store, '/repo', 's')
  expect(result).toContain('ready: relais.toml committed as abc1234 on main')
  expect(w.asks.map(a => a.options)).toEqual([[USE_CHECKS, NOT_NOW], [NOT_NOW, YES2]])
  expect(w.asks[0].question).toContain('relais 1 of 2 · Use these checks to accept changes in repo?')
  expect(w.asks[0].question).toContain('Skipped: amont gate cargo-test (not a plain command)')
  expect(w.state.commits).toEqual(['commit -m chore(relais): add relais.toml -- relais.toml'])
  expect(w.state.grants.length).toBe(1)
  expect(w.toasts.some(t => t.startsWith('relais · set up: relais.toml abc1234 on main'))).toBe(true)
})

test('onboard: declining the trust question removes the relais.toml it wrote', async () => {
  const w = world()
  w.answers.push(USE_CHECKS, NOT_NOW)
  expect(await onboardTool(w.fx, w.store, '/repo', 's')).toContain('declined (not_now)')
  expect(w.state.policy).toBe(false)
  expect(w.state.commits.length).toBe(0)
  expect(w.state.grants.length).toBe(0)
})

test('onboard: a dismissed first question writes nothing', async () => {
  const w = world()
  w.answers.push(new Error('dismissed'))
  expect(await onboardTool(w.fx, w.store, '/repo', 's')).toContain('declined (dismissed)')
  expect(w.state.policy).toBe(false)
})

test('onboard: a refused commit keeps relais.toml, grants nothing and passes on what git said', async () => {
  const w = world()
  w.state.commitFails = true
  w.answers.push(USE_CHECKS, YES2)
  const result = await onboardTool(w.fx, w.store, '/repo', 's')
  expect(result).toContain('onboard_commit_failed')
  expect(result).toContain('pre-commit: cargo test failed')
  expect(w.state.policy).toBe(true)
  expect(w.state.grants.length).toBe(0)
})

test('onboard: with nothing detected the typed command is proposed and shown in question 2', async () => {
  const w = world({ detected: null })
  w.answers.push('make test', allowLabel(2))
  const result = await onboardTool(w.fx, w.store, '/repo', 's')
  expect(result).toContain('ready')
  expect(w.asks[0].options).toEqual([NOT_NOW])
  expect(w.asks[1].question).toContain('  check  make test')
})

test('onboard: a shell line is explained and asked once more, then declined', async () => {
  const w = world({ detected: null })
  w.answers.push('make test | tee x', 'make test | tee y')
  const result = await onboardTool(w.fx, w.store, '/repo', 's')
  expect(result).toContain('declined (command_refused)')
  expect(w.asks.length).toBe(2)
  expect(w.asks[1].question).toContain('`|` is not allowed')
  expect(w.state.policy).toBe(false)
})

test('onboard: Not now on the typed-command question is a decline, not a command', async () => {
  const w = world({ detected: null })
  w.answers.push(NOT_NOW)
  expect(await onboardTool(w.fx, w.store, '/repo', 's')).toContain('declined (not_now)')
  expect(w.state.policy).toBe(false)
})

test('the guard keeps the model off machine.toml', async () => {
  const env = { home: '/home/p' }
  const path = '/home/p/.config/relais/machine.toml'
  expect(machineSettingsGuard('Write', { file_path: path }, env)).toBe(MACHINE_SETTINGS_MESSAGE)
  expect(machineSettingsGuard('Edit', { file_path: path }, env)).toBe(MACHINE_SETTINGS_MESSAGE)
  expect(machineSettingsGuard('Write', { file_path: '/x/machine.toml' }, { configDir: '/x' })).toBe(MACHINE_SETTINGS_MESSAGE)
  expect(machineSettingsGuard('Bash', { command: 'relais trust grant --key k --reviewed-by me' }, env)).toBe(MACHINE_SETTINGS_MESSAGE)
  expect(machineSettingsGuard('Bash', { command: `cat >> ${path}` }, env)).toBe(MACHINE_SETTINGS_MESSAGE)
  expect(machineSettingsGuard('Bash', { command: 'relais trust show' }, env)).toBe(undefined)
  expect(machineSettingsGuard('Write', { file_path: '/repo/relais.toml' }, env)).toBe(undefined)
})

test('a Not now holds until the person asks for relais again, then they are asked again', async () => {
  const w = world()
  w.state.policy = true
  w.answers.push(NOT_NOW)
  await trustTool(w.fx, w.store, '/repo', 's')
  onPersonPrompt(w.store, 'add a doc comment to add')
  onPersonPrompt(w.store, "no, don't use relais for this")
  w.store.ownPrompts.add('relais run abc finished: blocked (no_policy).')
  onPersonPrompt(w.store, 'relais run abc finished: blocked (no_policy).')
  expect(await trustTool(w.fx, w.store, '/repo', 's')).toContain('declined (not_now)')
  expect(w.asks.length).toBe(1)
  onPersonPrompt(w.store, 'ok, set up Relais here')
  w.answers.push(YES2)
  expect(await trustTool(w.fx, w.store, '/repo', 's')).toContain('ready')
  expect(w.asks.length).toBe(2)
})

test('onboard: output from trust show that is not JSON removes the relais.toml it wrote', async () => {
  const w = world()
  w.state.showGarbage = true
  w.answers.push(USE_CHECKS)
  const result = await onboardTool(w.fx, w.store, '/repo', 's')
  expect(result).toContain('not set up: relais trust show')
  expect(w.state.policy).toBe(false)
  expect(w.state.grants.length).toBe(0)
})

test('onboard: a relais.toml that cannot be removed after a decline is said', async () => {
  const w = world()
  w.state.rmFails = true
  w.answers.push(USE_CHECKS, NOT_NOW)
  const result = await onboardTool(w.fx, w.store, '/repo', 's')
  expect(result).toContain('declined (not_now)')
  expect(result).toContain('relais.toml could not be removed (rm: Permission denied)')
})

test('bidi overrides and line separators are spelled out', async () => {
  expect(escapeDisplay('rm\u202etxt.sh')).toBe('rm\\u{202e}txt.sh')
  expect(escapeDisplay('a\u2028b')).toBe('a\\u{2028}b')
})

test('the system prompt gains the routing rule as one section, once', async () => {
  const base = { sections: [{ id: 'base', text: 'You are Claude Code.', scope: 'global' }] }
  const composed = withRoutingSection(base)
  expect(composed.sections.map((s: any) => s.id)).toEqual(['base', 'relais-routing'])
  expect(composed.sections[1].text).toContain('Route each bounded implementation or inspection task through relais')
  expect(composed.sections[1].text).toContain('`relais:relais`')
  expect(composed.sections[1].text.split('\n').length).toBe(3)
  expect(composed.sections[1].scope).toBe('global')
  expect(withRoutingSection(composed)).toBe(composed)
  expect(withRoutingSection(undefined)).toBe(undefined)
})
