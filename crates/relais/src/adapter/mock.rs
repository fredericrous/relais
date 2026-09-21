//! A mock backend for the crate's own tests: behavior is a closure over the
//! launch spec, so scenario tests can act as a scripted worker — creating
//! files in the worktree, claiming blockage, crashing, or substituting a
//! different effective model. It advertises exactly what it enforces
//! (nothing), because no adapter may advertise guarantees its backend
//! cannot enforce (SPEC §20).

use std::sync::Arc;

use crate::backend::{
    claims_blockage, Backend, BackendError, Capabilities, LaunchResult, LaunchSpec,
    PermissionEnforcement, SandboxCapability, UsageReport,
};
use crate::procs::Ended;

#[derive(Debug, Clone, Default)]
pub struct MockOutcome {
    /// None = the process died without a terminal result (interrupted).
    pub result_text: Option<String>,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub effective_model: Option<String>,
    pub usage: Option<UsageReport>,
    pub session_id: Option<String>,
    /// Tools the scripted harness "refused" the worker.
    pub permission_denials: Vec<String>,
}

pub struct MockBackend {
    pub behavior: Arc<dyn Fn(&LaunchSpec) -> MockOutcome + Send + Sync>,
}

impl MockBackend {
    pub fn new(behavior: impl Fn(&LaunchSpec) -> MockOutcome + Send + Sync + 'static) -> Self {
        Self {
            behavior: Arc::new(behavior),
        }
    }
}

impl Backend for MockBackend {
    fn name(&self) -> &'static str {
        "mock"
    }

    fn probe(&self) -> Option<Capabilities> {
        Some(Capabilities {
            backend: "mock".into(),
            version: Some("test".into()),
            supports_model: true,
            supports_effort: true,
            supports_max_turns: true,
            supports_output_format_json: false,
            supports_budget: false,
            supports_disallowed_tools: true,
            supports_settings: true,
            permission_enforcement: PermissionEnforcement::Observed,
            sandbox: SandboxCapability::WorktreeOnly,
        })
    }

    fn launch(&self, spec: &LaunchSpec) -> Result<LaunchResult, BackendError> {
        let outcome = (self.behavior)(spec);
        // A cancellation outranks whatever the script said: the runner
        // reads a cancelled dispatch before it reads a missing result.
        let cancelled = spec
            .cancel
            .as_ref()
            .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::SeqCst));
        let ended = match (cancelled, outcome.timed_out, outcome.exit_code) {
            (true, _, _) => Ended::Cancelled,
            (false, true, _) => Ended::TimedOut,
            (false, false, Some(code)) => Ended::Exited(code),
            (false, false, None) => Ended::Signalled,
        };
        Ok(LaunchResult {
            dispatch_id: spec.dispatch_id.clone(),
            ended,
            stdout: outcome.result_text.clone().unwrap_or_default(),
            stderr: String::new(),
            result_text: if outcome.timed_out {
                None
            } else {
                outcome.result_text.clone()
            },
            session_id: outcome.session_id,
            effective_model: outcome.effective_model.or(Some(spec.model.clone())),
            usage: outcome.usage.unwrap_or(UsageReport::unknown()),
            worker_claims_blockage: outcome.result_text.as_deref().is_some_and(claims_blockage),
            permission_denials: outcome.permission_denials,
            failure_detail: None,
        })
    }
}
