// What keeps the model out of relais's agents. Pure predicates; the hooks
// that use them are written to fail closed.

export const isRelaisType = (type: unknown): boolean =>
  typeof type === 'string' && type.startsWith('relais:')

type Listed = { id: string; name?: string; type?: string; spawnedBy?: string }

// The ids and names SendMessage may address a relais agent by.
export function relaisAddresses(agents: readonly Listed[], plugin: string): Set<string> {
  const addresses = new Set<string>()
  for (const a of agents) {
    if (!isRelaisType(a.type) && a.spawnedBy !== plugin) continue
    addresses.add(a.id)
    if (a.name) addresses.add(a.name)
  }
  return addresses
}

// A `task-notification` names its agent in `<task-id>`. Only a notification
// that starts with the tag and names a relais agent is ours; a prompt that
// merely mentions an id is somebody's words and is kept.
export function isRelaisNotification(text: string, agentIds: ReadonlySet<string>): boolean {
  if (!text.startsWith('<task-notification>')) return false
  const named = /<task-id>\s*([^<\s]+)\s*<\/task-id>/.exec(text)
  return named !== null && agentIds.has(named[1])
}

export const DENY_MESSAGE =
  'This agent belongs to a relais run; relais drives it. Ask relais (the relais tool), not the agent.'

export const MACHINE_SETTINGS_MESSAGE =
  "machine.toml holds relais's trust grants, and only the person grants them. Call mcp__relais__trust with the repository's cwd: it shows them the exact commands and asks."

// Where relais keeps machine.toml: `$RELAIS_CONFIG_DIR`, else
// `~/.config/relais` (crates/relais/src/paths.rs).
export function machineSettingsPath(env: { home?: string; configDir?: string }): string | undefined {
  if (env.configDir) return `${env.configDir.replace(/\/+$/, '')}/machine.toml`
  if (env.home) return `${env.home.replace(/\/+$/, '')}/.config/relais/machine.toml`
  return undefined
}

export const ROUTER_ENVELOPE_MESSAGE =
  'Session routing is the person\'s to turn on or off: `relais install --claude` or /relais-routing records the envelope, and /relais-routing off removes it. The model does not run router-envelope or relais install --claude.'

// Why the model's Bash call may not record or remove the routing envelope,
// or undefined: `relais native router-envelope`, and `relais install …
// --claude` (which records the envelope). A reminder, not a boundary: in a
// bypass-permissions session a deliberate write is the stated limit.
export function routerEnvelopeGuard(tool: string, input: Record<string, unknown>): string | undefined {
  if (tool !== 'Bash') return undefined
  const command = String(input.command ?? '')
  if (/\brouter-envelope\b/.test(command)) return ROUTER_ENVELOPE_MESSAGE
  if (/\brelais\s+install\b[^|;&\n]*--claude\b/.test(command)) return ROUTER_ENVELOPE_MESSAGE
  return undefined
}

// Why the model's Write, Edit or Bash call may not touch the grants, or
// undefined. Write and Edit are matched on the exact path. Bash is a shell
// string, so its match is a reminder for the model, not a boundary: SPEC §5
// puts bypass-permissions sessions outside this threat model.
export function machineSettingsGuard(
  tool: string,
  input: Record<string, unknown>,
  env: { home?: string; configDir?: string },
): string | undefined {
  const path = machineSettingsPath(env)
  const named = (text: string) =>
    (path !== undefined && text.includes(path)) || text.includes('.config/relais/machine.toml')
  if (tool === 'Bash') {
    const command = String(input.command ?? '')
    return /\brelais\s+trust\s+grant\b/.test(command) || named(command) ? MACHINE_SETTINGS_MESSAGE : undefined
  }
  const file = String(input.file_path ?? '')
  return file !== '' && (file === path || named(file)) ? MACHINE_SETTINGS_MESSAGE : undefined
}

export const DECIDE_MESSAGE =
  'Record a decision with mcp__relais__decide, and an outcome with mcp__relais__feedback, not relais decide or relais feedback in Bash: the tools set who answered and prove the call was their own (ADR-0004). The person answers from their own terminal.'

// Why the model's Bash call should go through the decide or feedback tool, or
// undefined: `relais decide` or `relais feedback` as a command (at the start, or after `;`,
// `&`, `|`, `(` or `$(`). Text that only mentions it — a grep, an echo —
// passes. A reminder, not a boundary, as the others are: relais itself
// records such a call as `cli`, never as the plugin's.
export function decideGuard(tool: string, input: Record<string, unknown>): string | undefined {
  if (tool !== 'Bash') return undefined
  const command = String(input.command ?? '')
  return /(?:^|[;&|(\n])\s*(?:\S*\/)?relais\s+(?:decide|feedback)\b/.test(command) ? DECIDE_MESSAGE : undefined
}

export const LEARNING_ENVELOPE_MESSAGE =
  "Automatic promotion of learned routing is the person's to turn on or off: the person turns it on or off with /relais-learning (or `relais install --claude`). The model does not run learning-envelope or write the [learning] block of the machine settings."

const LEARNING_BLOCK = /\[learning\]|\bauto_promote\b/

// Why the model's call may not record or remove the learning envelope, or
// undefined: `relais native learning-envelope` in Bash, and a write of the
// `[learning]` block into machine.toml. A reminder, as the routing one is.
export function learningEnvelopeGuard(
  tool: string,
  input: Record<string, unknown>,
  env: { home?: string; configDir?: string },
): string | undefined {
  const path = machineSettingsPath(env)
  const isMachine = (text: string) =>
    (path !== undefined && text.includes(path)) || text.includes('.config/relais/machine.toml') || /(^|\/)machine\.toml\b/.test(text)
  if (tool === 'Bash') {
    const command = String(input.command ?? '')
    if (/\blearning-envelope\b/.test(command)) return LEARNING_ENVELOPE_MESSAGE
    return isMachine(command) && LEARNING_BLOCK.test(command) ? LEARNING_ENVELOPE_MESSAGE : undefined
  }
  const file = String(input.file_path ?? '')
  if (file === '' || !isMachine(file)) return undefined
  const edits = Array.isArray(input.edits) ? input.edits.map((x: any) => x?.new_string) : []
  const written = [input.content, input.new_string, input.new_source, ...edits].map(x => String(x ?? '')).join('\n')
  return LEARNING_BLOCK.test(written) ? LEARNING_ENVELOPE_MESSAGE : undefined
}
