import { expect, test } from 'claude-code/testing'
import { parseProtocolLine, splitLines } from '../hooks/lines.ts'
import { line, settle, spawnLine, startedRun } from './support.ts'

test('the splitter carries the unfinished remainder between chunks', () => {
  const first = splitLines('', '{"relais":"sp')
  expect(first).toEqual({ lines: [], carry: '{"relais":"sp' })
  const second = splitLines(first.carry, 'awn"}\n{"relais":"done"}\nrest')
  expect(second).toEqual({ lines: ['{"relais":"spawn"}', '{"relais":"done"}'], carry: 'rest' })
  expect(splitLines('', 'a\r\nb\r\n')).toEqual({ lines: ['a', 'b'], carry: '' })
})

test('a line that is not a JSON object with a relais key is ignored', () => {
  expect(parseProtocolLine('warning: something')).toBe(undefined)
  expect(parseProtocolLine('{"half":')).toBe(undefined)
  expect(parseProtocolLine('{"kind":"event"}')).toBe(undefined)
  expect(parseProtocolLine('{"relais":3}')).toBe(undefined)
  expect(parseProtocolLine('[{"relais":"spawn"}]')).toBe(undefined)
  expect(parseProtocolLine('42')).toBe(undefined)
  expect(parseProtocolLine('{"relais":"done","run":"r"}')).toEqual({ relais: 'done', run: 'r' })
})

test('a request split across two chunks is reassembled', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  const text = spawnLine('d1')
  const cut = Math.floor(text.length / 2)
  engine.stream.push('stdout', text.slice(0, cut))
  await settle(engine)
  expect(engine.calls.spawned.length).toBe(0)
  engine.stream.push('stdout', text.slice(cut))
  await settle(engine)
  expect(engine.calls.spawned.length).toBe(1)
  expect(engine.calls.spawned[0].cwd).toBe('/work/d1')
})

test('a garbage line is ignored and the lines around it are not', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  engine.stream.push('stdout', 'a stray print\n' + spawnLine('d1') + '{"not":"ours"}\n' + spawnLine('d2'))
  await settle(engine)
  expect(engine.calls.spawned.map((s: any) => s.description)).toEqual(['relais d1', 'relais d2'])
})

test('a 64 KB line arrives in pieces and is reassembled', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  const prompt = 'x'.repeat(64 * 1024)
  const text = line({ relais: 'spawn', dispatch: 'd1', prompt, subagent_type: 'relais:relais-worker-sonnet-medium', cwd: '/work/d1' })
  for (let at = 0; at < text.length; at += 9000) engine.stream.push('stdout', text.slice(at, at + 9000))
  await settle(engine)
  expect(engine.calls.spawned.length).toBe(1)
  expect(engine.calls.spawned[0].prompt.length).toBe(64 * 1024)
})
