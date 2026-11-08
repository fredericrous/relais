// Where the module's `relais` calls run (relais#408). The session's working
// directory can be removed while a run is live (a worktree torn down), and a
// spawn whose cwd is gone fails with ENOENT before relais starts: every hello
// then fails, and relais reads the run's worker as lost. A call that names its
// session or run and reads no repository runs from HOME instead. A call that
// needs a repository keeps its directory, and runs again from HOME only when
// that directory is gone.

import type { Fx } from './fx.ts'

// `{ cwd: HOME }`, or nothing when the host has no HOME (the host's default).
export async function homeCwd(fx: Fx): Promise<{ cwd?: string }> {
  const home = await fx.env.home()
  return typeof home === 'string' && home !== '' ? { cwd: home } : {}
}

const missingDirectory = (reason: unknown) => /\bENOENT\b/.test(String(reason))

// `fx.process.run`, run once more from HOME when it failed with ENOENT from a
// directory other than HOME. Any other failure is the caller's, unchanged.
export async function runFallingBackHome(fx: Fx, argv: string[], init: Record<string, unknown>) {
  try {
    return await fx.process.run(argv, init)
  } catch (reason) {
    if (!missingDirectory(reason)) throw reason
    const home = await homeCwd(fx)
    if (home.cwd === undefined || home.cwd === init.cwd) throw reason
    return fx.process.run(argv, { ...init, ...home })
  }
}
