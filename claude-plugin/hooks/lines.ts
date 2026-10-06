// `$.process.spawn` hands over text in chunks, not lines: a line may span
// two chunks and one chunk may hold several lines.

// A child that never writes a newline must not grow the carry for ever.
const MAX_CARRY = 16 * 1024 * 1024

export type Split = { lines: string[]; carry: string }

// The complete lines of `carry + chunk`, and the unfinished rest.
export function splitLines(carry: string, chunk: string): Split {
  const text = carry + chunk
  const parts = text.split('\n')
  const rest = parts.pop() ?? ''
  const lines = parts.map(line => (line.endsWith('\r') ? line.slice(0, -1) : line))
  return { lines, carry: rest.length > MAX_CARRY ? '' : rest }
}

export type ProtocolLine = { relais: string; [key: string]: unknown }

// A protocol line is a JSON object with a `relais` key; anything else
// (a stray print, half a line, a number) is not ours and is ignored.
export function parseProtocolLine(line: string): ProtocolLine | undefined {
  const text = line.trim()
  if (!text.startsWith('{')) return undefined
  try {
    const value = JSON.parse(text)
    const isObject = typeof value === 'object' && value !== null && !Array.isArray(value)
    return isObject && typeof value.relais === 'string' ? value : undefined
  } catch {
    return undefined
  }
}
