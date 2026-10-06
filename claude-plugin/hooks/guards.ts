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
