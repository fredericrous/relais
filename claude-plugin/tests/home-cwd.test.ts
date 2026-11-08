import { expect, test } from 'claude-code/testing'
import { runFallingBackHome } from '../hooks/home-cwd.ts'

const fx = (fail: (init: any) => Error | undefined, home = '/Users/p') => {
  const calls: any[] = []
  return {
    calls,
    fx: {
      env: { home: async () => home, stateDir: async () => undefined },
      process: {
        run: async (argv: string[], init: any) => {
          calls.push(init)
          const error = fail(init)
          if (error) throw error
          return { exitCode: 0, stdout: '', stderr: '' }
        },
      },
    } as any,
  }
}

test('a call whose repository directory is gone is run again from HOME', async () => {
  const world = fx(init => (init.cwd === '/repo-gone' ? new Error('spawn relais ENOENT') : undefined))
  const result = await runFallingBackHome(world.fx, ['relais', 'native', 'router-state'], { cwd: '/repo-gone', timeoutMs: 1 })
  expect(result.exitCode).toBe(0)
  expect(world.calls.map(c => c.cwd)).toEqual(['/repo-gone', '/Users/p'])
})

test('another failure is not retried, and a call already at HOME is not retried', async () => {
  const other = fx(() => new Error('timed out'))
  await expect(runFallingBackHome(other.fx, ['relais'], { cwd: '/repo', timeoutMs: 1 })).rejects.toThrow('timed out')
  expect(other.calls.length).toBe(1)
  const atHome = fx(() => new Error('spawn relais ENOENT'))
  await expect(runFallingBackHome(atHome.fx, ['relais'], { cwd: '/Users/p', timeoutMs: 1 })).rejects.toThrow('ENOENT')
  expect(atHome.calls.length).toBe(1)
})
