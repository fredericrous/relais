//! What a run is configured with, and where its directories live.

use super::{
    context, verify, workspace, Backend, Duration, Gate, Ledger, MachineSettings, Path, PathBuf,
    RepoPolicy, RoutePredictor, RunId, TaskContract, Wire,
};

pub struct RunConfig<'a> {
    pub repo_dir: &'a Path,
    /// Where this run works (ADR-0008). `Isolated` is today's behaviour.
    pub workspace: crate::workspace::reuse::Workspace,
    pub contract: &'a TaskContract,
    pub repo_policy: &'a RepoPolicy,
    pub machine: &'a MachineSettings,
    pub ledger: &'a Ledger,
    /// Where run and dispatch identifiers come from (clock, process id,
    /// sequence), built once at a boundary and passed in.
    pub ids: &'a crate::ids::IdSource,
    pub backend: &'a dyn Backend,
    /// Git, as a port: every repository fact a run reads goes through
    /// it, so a test supplies one and production supplies
    /// `workspace::SystemGit`.
    pub git: &'a dyn workspace::Git,
    /// The check inventory (`amont`), as a port.
    pub hooks: &'a dyn verify::HookInventory,
    /// Whether an amont gate covers a candidate (`amont attest
    /// covered`), as a port.
    pub attest: &'a dyn verify::HookAttest,
    pub artifacts_dir: PathBuf,
    /// aval resolution, injectable so runs are testable without the real
    /// corpus; production wiring passes a `context::AvalCli`.
    pub aval_resolver: &'a dyn context::DecisionResolver,
    pub predictor: Option<&'a dyn RoutePredictor>,
    /// Managed dispatch (SPEC §23): every launch is admitted, heartbeat
    /// and settled through this gate. `None` = unmanaged execution;
    /// `relais run` always sets one.
    pub gate: Option<&'a (dyn Gate + Sync)>,
    /// The interactive session this run belongs to, for fair scheduling
    /// and attribution.
    pub session_id: String,
    /// Lease heartbeat period while a worker runs.
    pub heartbeat_every: Duration,
    /// This run's task identity, already resolved and confirmed on
    /// record by the caller (the CLI's `--revise`, or the contract's own
    /// declared task): `None` derives a fresh one at preflight, as
    /// before. Only consulted for a root run — a child always inherits
    /// its parent's task regardless of this field.
    pub task_override: Option<&'a crate::ids::TaskId>,
    /// Why this run exists, when it is not ordinary work. Written with
    /// the run row itself, NOT stamped afterwards: a replay stamped after
    /// `execute` returned left money spent under a run the ledger showed
    /// as ordinary whenever `execute` errored or the process was killed,
    /// and no trial to account for it. A run's purpose holds for the
    /// whole run or it is not a purpose.
    pub purpose: Option<crate::lifecycle::RunPurpose>,
    /// The id this run executes as, when the caller minted it ahead of
    /// `execute` — a live trial row names its own run and is written
    /// before any worker is dispatched. `None` mints one here.
    pub run_id: Option<RunId>,
    /// Where this run's protocol lines go (SPEC §29): its events, and its
    /// `done`. The process's own stdout under `relais run --protocol`.
    pub wire: Wire,
    /// Where the worker's program design comes from.
    pub design: DesignSource<'a>,
    /// Where the commands this repository last ran under are kept, and
    /// whether this run updates them: `relais run` passes a recording
    /// store, so only `run` consumes a pending command change (ADR-0007).
    /// A replay never reads it: it neither says nor records a change.
    pub declarations: &'a dyn crate::trust::DeclarationStore,
}

/// Where a run's program design comes from: the path its contract names,
/// or a design the caller already read. Routing (`RouteInputs::has_design`)
/// and the context package read the same answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesignSource<'a> {
    /// The contract's own `design`, read at the base revision; none when
    /// the contract names none.
    Contract,
    /// A design handed in beside the contract: `relais dataset replay
    /// --design`, whose recorded contract (and so its hash) names none.
    Given(&'a context::DesignInput),
}

impl DesignSource<'_> {
    pub(super) fn is_present(self, contract: &TaskContract) -> bool {
        match self {
            Self::Contract => contract.design.is_some(),
            Self::Given(_) => true,
        }
    }
}

/// Where a run's git worktrees live: a SIBLING of the artifact
/// directory, never inside it (audit B6).
///
/// The worker's cwd is its task worktree, and it has Bash. With the
/// worktree under `runs/<id>/worktree`, everything the run records —
/// `receipt.json`, the candidate patches, the check logs — sat exactly
/// one `../` away, and `relais run` printed that receipt path as the
/// run's answer. A worker could therefore write the document the user is
/// told to read. Moving the worktrees out puts the record where no
/// relative path from the worker's tree names it by construction; the
/// ledger copy of the receipt was always honest, and now the file is
/// too. Both roots stay under the state directory, so nothing escapes
/// `RELAIS_STATE_DIR`.
pub fn worktree_root(artifacts_dir: &Path) -> PathBuf {
    state_sibling(artifacts_dir, crate::paths::WORKTREES_DIR)
}

/// Where a run's throwaway verification worktrees live. Separate from
/// the task worktree's parent as well as from the artifacts: a straggling
/// descendant of the worker must not be able to reach the immutable copy
/// being verified through a relative path either (SPEC §10: no concurrent
/// worker may modify the verified candidate).
pub fn verify_root(artifacts_dir: &Path) -> PathBuf {
    state_sibling(artifacts_dir, "verify")
}

pub(super) fn state_sibling(artifacts_dir: &Path, name: &str) -> PathBuf {
    artifacts_dir.parent().unwrap_or(artifacts_dir).join(name)
}

/// The `worktree_retired` transition's detail: the ref and patch the
/// final tree went to (`null` when a named candidate already held it)
/// and the bytes the directory took.
pub(super) fn retirement_detail(
    worktree: &Path,
    retirement: &workspace::Retirement,
) -> serde_json::Value {
    serde_json::json!({
        "worktree": worktree.to_string_lossy(),
        "reference": retirement.exported.as_ref().map(|e| e.reference.clone()),
        "patch": retirement
            .exported
            .as_ref()
            .map(|e| e.patch_path.to_string_lossy().into_owned()),
        "bytes_reclaimed": retirement.bytes_reclaimed,
    })
}
