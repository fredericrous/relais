//! Programme maps (SPEC §31): `docs/programmes/<slug>/map.md` plus one
//! TOML-frontmatter ticket file per decision, read and queried, never
//! written; [`claim`] is the one store, kept in the state directory and
//! never in a map. Parsing and analysis are pure; only [`load`] touches the
//! filesystem and git, and nothing here opens the ledger.

pub mod analysis;
pub mod claim;
pub mod frontier;
pub mod load;
pub mod model;
pub mod parse;

/// The name every scoped worker of a programme read carries. The
/// session-start hook reads programmes on its gather thread, and its panic
/// hook keeps a panic on a thread of this name off stderr: the worker's
/// panic is caught at its join, as the gather thread's own is.
pub const WORKER_THREAD_NAME: &str = "relais-programme-worker";

/// Spawn `work` on a scoped thread named [`WORKER_THREAD_NAME`]. Like
/// `Scope::spawn`, it panics when the thread cannot start.
pub(crate) fn spawn_worker<'scope, 'env, T, F>(
    scope: &'scope std::thread::Scope<'scope, 'env>,
    work: F,
) -> std::thread::ScopedJoinHandle<'scope, T>
where
    F: FnOnce() -> T + Send + 'scope,
    T: Send + 'scope,
{
    std::thread::Builder::new()
        .name(WORKER_THREAD_NAME.into())
        .spawn_scoped(scope, work)
        .expect("failed to spawn thread")
}
