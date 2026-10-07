// Setting relais up in a repository with the person, from inside a tool
// call: which checks accept a change (relais.toml), and whether relais may
// run them (the trust grant in machine.toml). Both questions are put by
// this module through Claude Code's own dialog (`$.ui.ask`), worded from
// what relais itself reports. The model never words them, never sees the
// answer before it is acted on, and never chooses the key that is granted.
// SPEC §5.

import type { Fx } from './fx.ts'
import type { Store } from './store.ts'

const RELAIS_TIMEOUT_MS = 60_000
// A pre-commit hook can run the repository's whole test suite: amont's
// takes about nine minutes here. `$.process.run` waits ten at most.
const COMMIT_TIMEOUT_MS = 600_000
const COLUMN_CAP = 40
const Q1_SHOWN = 6
const POLICY = 'relais.toml'
const COMMIT_SUBJECT = 'chore(relais): add relais.toml'

export const USE_CHECKS = 'Use these checks'
export const NOT_NOW = 'Not now'
export const allowLabel = (count: number) => `Allow ${count} command${count === 1 ? '' : 's'}`

// What a repository wrote, made safe to show: a newline or another control
// character in a script name must not draw a line of its own.
export function escapeDisplay(text: string): string {
  let out = ''
  for (const ch of text) {
    const code = ch.codePointAt(0) ?? 0
    if (ch === '\n') out += '\\n'
    else if (ch === '\r') out += '\\r'
    else if (ch === '\t') out += '\\t'
    else if (ch === '\\') out += '\\\\'
    else if (code < 0x20 || (code >= 0x7f && code < 0xa0)) out += `\\x${code.toString(16).padStart(2, '0')}`
    else out += ch
  }
  return out
}

export function displayArgv(argv: string[]): string {
  return argv.map(arg => (arg === '' || arg.includes(' ') ? `'${escapeDisplay(arg)}'` : escapeDisplay(arg))).join(' ')
}

type Row = { mark?: string; label: string; argv: string[]; source?: string }

// One aligned row per command: a mark column, the label, the command padded
// to the longest one (at most 40), then its source. A longer command is
// never cut: it pushes its source along.
function rows(list: Row[]): string[] {
  const shown = list.map(r => displayArgv(r.argv))
  const width = Math.min(COLUMN_CAP, Math.max(0, ...shown.map(s => s.length)))
  return list.map((r, i) => {
    const command = r.source ? shown[i].padEnd(width) : shown[i]
    const source = r.source ? `   ${escapeDisplay(r.source)}` : ''
    return `${r.mark ?? ' '} ${r.label.padEnd(6)} ${command}${source}`.trimEnd()
  })
}

export type Proposal = {
  setup: { argv: string[]; source: string }[]
  commands: { argv: string[]; source: string }[]
  skipped: { what: string; reason: string }[]
}

export function question1(proposal: Proposal, repo: string, branch: string): string {
  const all: Row[] = [
    ...proposal.setup.map(s => ({ label: 'setup', argv: s.argv, source: s.source })),
    ...proposal.commands.map(c => ({ label: 'check', argv: c.argv, source: c.source })),
  ]
  const listed = rows(all.slice(0, Q1_SHOWN))
  if (all.length > Q1_SHOWN) listed.push(`  +${all.length - Q1_SHOWN} more`)
  const skipped = proposal.skipped.map(s => `Skipped: ${escapeDisplay(s.what)} (${escapeDisplay(s.reason)})`)
  return [
    `relais 1 of 2 · Use these checks to accept changes in ${escapeDisplay(repo)}?`,
    ...listed,
    ...skipped,
    `"${USE_CHECKS}" writes relais.toml and commits only that file,`,
    `on ${escapeDisplay(branch)}.`,
  ].join('\n')
}

export function question1Nothing(repo: string, branch: string, refusal?: string): string {
  return [
    `relais 1 of 2 · No command here proves a change works in ${escapeDisplay(repo)}.`,
    ...(refusal ? [`That one was refused: ${escapeDisplay(refusal)}`] : []),
    'Type one below (for example: make test), or choose Not now.',
    `It goes into relais.toml, committed alone on ${escapeDisplay(branch)}, after step 2.`,
  ].join('\n')
}

export type Shown = {
  grant_key: string
  repository: string
  machine_settings: string
  granted: boolean
  declaration: {
    steps: { profile: string; kind: 'setup' | 'check'; argv: string[] }[]
    models: { tier: string; id: string }[]
    integrations: { name: string; mode: string }[]
  }
  changes?: ('same' | 'new' | 'changed')[]
}

const MARK = { same: ' ', new: '+', changed: '~' } as const

export function question2(shown: Shown, repo: string): { text: string; yes: string } {
  const steps = shown.declaration.steps
  const listed = rows(
    steps.map((s, i) => ({
      mark: shown.changes ? MARK[shown.changes[i] ?? 'new'] : ' ',
      label: s.kind,
      argv: s.argv,
    })),
  )
  const models = shown.declaration.models.map(m => escapeDisplay(m.id)).join(' · ')
  const integrations = shown.declaration.integrations
    .filter(i => i.mode !== 'off')
    .map(i => `${i.name} (${i.mode})`)
    .join(' · ')
  const unchanged =
    shown.changes && shown.changes.every(c => c === 'same') ? ['  commands unchanged; models or integrations changed'] : []
  return {
    text: [
      `relais 2 of 2 · Let relais run these commands in ${escapeDisplay(repo)},`,
      "outside Claude Code's permission prompts?",
      ...listed,
      ...unchanged,
      `  models ${models || 'none'}${integrations ? `     integrations ${integrations}` : ''}`,
      `Saved in ${escapeDisplay(shown.machine_settings)}.`,
      'Any change to relais.toml asks again. They run in a worktree.',
    ].join('\n'),
    yes: allowLabel(steps.length),
  }
}

// The person's answer, or undefined when they dismissed the question,
// chose to chat, or nobody can be asked (`claude -p`): the dialog rejects
// in all three and does not say which.
async function ask(fx: Fx, question: string, options: string[]): Promise<string | undefined> {
  try {
    return await fx.ui.ask(question, options)
  } catch {
    return undefined
  }
}

async function run(fx: Fx, argv: string[], cwd: string, timeoutMs = RELAIS_TIMEOUT_MS) {
  return fx.process.run(argv, { cwd, timeoutMs })
}

const out = (r: any) => String(r?.stdout ?? '').trim()
const err = (r: any) => String(r?.stderr ?? '').trim().slice(0, 2000)

export type Declined = 'not_now' | 'dismissed'

function declined(reason: Declined, repo: string): string {
  if (reason === 'not_now') {
    return `declined (not_now): the person chose Not now. relais was not set up in ${repo}; nothing ran. Tell them, and do not ask again unless they ask for relais themselves.`
  }
  return (
    `declined (dismissed): the person closed the question or chose to chat about it, or nobody can be asked in this session. Nothing ran in ${repo}. ` +
    'Ask them what they want. Where nobody can answer, the same setup by hand is: `relais init --detect --write`, then `relais trust show` and `relais trust grant --key <key> --reviewed-by <name>`.'
  )
}

type Repo = { root: string; name: string; branch: string }

async function repoOf(fx: Fx, cwd: string): Promise<Repo | string> {
  const top = await run(fx, ['git', 'rev-parse', '--show-toplevel'], cwd)
  if (top.exitCode !== 0) return `not set up: ${cwd} is not in a git repository (${err(top)})`
  const root = out(top)
  const branch = await run(fx, ['git', 'symbolic-ref', '--short', '-q', 'HEAD'], root)
  return { root, name: root.split('/').filter(Boolean).pop() ?? root, branch: branch.exitCode === 0 ? out(branch) : '' }
}

// One question at a time per repository: a second call while one is open
// waits for it, so two parallel calls ask once and grant once.
function serialized(store: Store, root: string, work: () => Promise<string>): Promise<string> {
  const open = store.consent.get(root)
  if (open) return open
  const running = work().finally(() => store.consent.delete(root))
  store.consent.set(root, running)
  return running
}

// The trust question alone: for a repository whose relais.toml exists and
// is committed, after `relais run` said `missing_trust_grant`.
export async function trustTool(fx: Fx, store: Store, cwd: string, session: string): Promise<string> {
  const repo = await repoOf(fx, cwd)
  if (typeof repo === 'string') return repo
  if (store.declined.has(repo.root)) return declined('not_now', repo.name)
  return serialized(store, repo.root, () => askTrust(fx, store, repo, session))
}

async function askTrust(fx: Fx, store: Store, repo: Repo, session: string): Promise<string> {
  const show = await run(fx, ['relais', 'trust', 'show', '--json'], repo.root)
  if (show.exitCode !== 0) return `not set up: relais trust show failed: ${err(show)}`
  const shown: Shown = JSON.parse(out(show))
  if (shown.granted) return `ready: ${repo.name} is already allowed. Call mcp__relais__run.`
  const q = question2(shown, repo.name)
  const answer = await ask(fx, q.text, [NOT_NOW, q.yes])
  if (answer !== q.yes) return refuse(fx, store, repo, answer === NOT_NOW ? 'not_now' : 'dismissed')
  const granted = await grant(fx, repo, shown.grant_key, session)
  if (granted !== 'ok') return granted
  fx.ui.toast(`relais · ${repo.name} allowed · starting run`)
  return `ready: the person allowed ${shown.declaration.steps.length} command(s) in ${repo.name}. Call mcp__relais__run again.`
}

async function grant(fx: Fx, repo: Repo, key: string, session: string): Promise<'ok' | string> {
  const result = await run(
    fx,
    ['relais', 'trust', 'grant', '--key', key, '--reviewed-by', `the person in Claude Code session ${session}`, '--source', 'plugin', '--json'],
    repo.root,
  )
  if (result.exitCode === 18) {
    return 'not set up (stale_grant_key): relais.toml changed while the person was being asked. Nothing was granted. Call mcp__relais__trust again so they see the current commands.'
  }
  if (result.exitCode !== 0) return `not set up: relais trust grant failed: ${err(result)}`
  return 'ok'
}

function refuse(fx: Fx, store: Store, repo: Repo, reason: Declined): string {
  if (reason === 'not_now') store.declined.add(repo.root)
  fx.ui.toast(`relais · not set up in ${repo.name} · /relais to be asked again`)
  return declined(reason, repo.name)
}

// Both questions, then the slow part: write, commit, grant. The person is
// never left waiting between the questions for a pre-commit hook.
export async function onboardTool(fx: Fx, store: Store, cwd: string, session: string): Promise<string> {
  const repo = await repoOf(fx, cwd)
  if (typeof repo === 'string') return repo
  if (store.declined.has(repo.root)) return declined('not_now', repo.name)
  return serialized(store, repo.root, () => onboard(fx, store, repo, session))
}

async function onboard(fx: Fx, store: Store, repo: Repo, session: string): Promise<string> {
  const exists = await run(fx, ['test', '-e', `${repo.root}/${POLICY}`], repo.root)
  if (exists.exitCode === 0) {
    return `ready: ${repo.name} already has relais.toml. Call mcp__relais__run; if it reports missing_trust_grant, call mcp__relais__trust.`
  }
  if (!repo.branch) {
    return `not set up (onboard_commit_failed): ${repo.name} is on a detached HEAD, so relais.toml has no branch to be committed on. Nothing was written.`
  }
  const busy = await operationInProgress(fx, repo.root)
  if (busy) {
    return `not set up (onboard_commit_failed): a ${busy} is in progress in ${repo.name}. Nothing was written; finish it, then call mcp__relais__onboard again.`
  }

  const detect = await run(fx, ['relais', 'init', '--detect', '--json'], repo.root)
  if (detect.exitCode !== 0 && detect.exitCode !== 19) return `not set up: relais init --detect failed: ${err(detect)}`
  const proposal: Proposal | null = JSON.parse(out(detect)).proposal

  let command: string | undefined
  if (proposal && proposal.commands.length > 0) {
    const answer = await ask(fx, question1(proposal, repo.name, repo.branch), [USE_CHECKS, NOT_NOW])
    if (answer !== USE_CHECKS) return refuse(fx, store, repo, answer === NOT_NOW ? 'not_now' : 'dismissed')
  } else {
    const typed = await askCommand(fx, repo)
    if (typed === NOT_NOW) return refuse(fx, store, repo, 'not_now')
    if (typed === undefined) return refuse(fx, store, repo, 'dismissed')
    if (typeof typed !== 'string') {
      fx.ui.toast(`relais · not set up in ${repo.name} · /relais to be asked again`)
      return `declined (command_refused): the command the person typed was refused twice (${typed.refused}). Nothing was written in ${repo.name}. Tell them relais runs one plain command, without shell operators.`
    }
    command = typed
  }

  const write = await run(
    fx,
    ['relais', 'init', '--detect', '--write', ...(command ? ['--command', command] : [])],
    repo.root,
  )
  if (write.exitCode !== 0) return `not set up: relais init --detect --write failed: ${err(write)}`
  const removeWritten = () => run(fx, ['rm', '-f', '--', `${repo.root}/${POLICY}`], repo.root)

  const show = await run(fx, ['relais', 'trust', 'show', '--json'], repo.root)
  if (show.exitCode !== 0) {
    await removeWritten()
    return `not set up: relais trust show failed: ${err(show)}`
  }
  const shown: Shown = JSON.parse(out(show))
  const q = question2(shown, repo.name)
  const answer = await ask(fx, q.text, [NOT_NOW, q.yes])
  if (answer !== q.yes) {
    await removeWritten()
    return refuse(fx, store, repo, answer === NOT_NOW ? 'not_now' : 'dismissed')
  }

  fx.ui.toast('relais · committing relais.toml (hooks may take minutes)…')
  const committed = await commitPolicy(fx, repo)
  if (committed !== 'ok') return committed
  const sha = out(await run(fx, ['git', 'rev-parse', '--short', 'HEAD'], repo.root))
  const granted = await grant(fx, repo, shown.grant_key, session)
  if (granted !== 'ok') return granted
  fx.ui.toast(`relais · set up: relais.toml ${sha} on ${repo.branch}, grant saved · starting run`)
  return `ready: relais.toml committed as ${sha} on ${repo.branch}, and the person allowed its ${shown.declaration.steps.length} command(s). Call mcp__relais__run again.`
}

// The typed command for a repository where nothing was detected: relais
// parses it; a refusal is explained and asked once more.
async function askCommand(fx: Fx, repo: Repo): Promise<string | undefined | { refused: string }> {
  let refusal: string | undefined
  for (let tries = 0; tries < 2; tries++) {
    const answer = await ask(fx, question1Nothing(repo.name, repo.branch, refusal), [NOT_NOW])
    if (answer === undefined || answer === NOT_NOW) return answer
    const text = answer.trim()
    if (text === '' || text.includes('\n')) {
      refusal = 'type one command on one line'
      continue
    }
    const check = await run(fx, ['relais', 'init', '--detect', '--json', '--command', text], repo.root)
    if (check.exitCode === 0) return text
    refusal = err(check).replace(/^relais[^:]*:\s*/, '')
  }
  return { refused: refusal ?? 'refused' }
}

async function operationInProgress(fx: Fx, root: string): Promise<string | undefined> {
  for (const [path, name] of [
    ['rebase-merge', 'rebase'],
    ['rebase-apply', 'rebase'],
    ['MERGE_HEAD', 'merge'],
    ['CHERRY_PICK_HEAD', 'cherry-pick'],
  ]) {
    const where = await run(fx, ['git', 'rev-parse', '--git-path', path], root)
    if (where.exitCode !== 0) continue
    const file = out(where)
    const absolute = file.startsWith('/') ? file : `${root}/${file}`
    if ((await run(fx, ['test', '-e', absolute], root)).exitCode === 0) return name
  }
  return undefined
}

// Commits relais.toml alone: `git add` then a commit limited to that path,
// so whatever the person had staged stays staged and out of it.
async function commitPolicy(fx: Fx, repo: Repo): Promise<'ok' | string> {
  const added = await run(fx, ['git', 'add', '--', POLICY], repo.root)
  if (added.exitCode !== 0) return commitFailed(fx, repo, err(added))
  const commit = await run(fx, ['git', 'commit', '-m', COMMIT_SUBJECT, '--', POLICY], repo.root, COMMIT_TIMEOUT_MS)
  if (commit.exitCode !== 0) return commitFailed(fx, repo, [err(commit), out(commit)].filter(Boolean).join('\n'))
  return 'ok'
}

async function commitFailed(fx: Fx, repo: Repo, output: string): Promise<string> {
  const lock = await run(fx, ['git', 'rev-parse', '--git-path', 'index.lock'], repo.root)
  const lockPath = out(lock)
  const absolute = lockPath.startsWith('/') ? lockPath : `${repo.root}/${lockPath}`
  const locked = lock.exitCode === 0 && (await run(fx, ['test', '-e', absolute], repo.root)).exitCode === 0
  return [
    `not set up (onboard_commit_failed): committing relais.toml in ${repo.name} failed. relais.toml is written and staged; no grant was written.`,
    locked ? `${absolute} exists (a git process may still be running, or it was killed); it was not removed.` : '',
    output ? `git said:\n${output.slice(0, 2000)}` : '',
    'Tell the person to commit relais.toml, then call mcp__relais__run (only the trust question will be asked).',
  ]
    .filter(Boolean)
    .join('\n')
}
