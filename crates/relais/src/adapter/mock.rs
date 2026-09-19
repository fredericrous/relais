//! A mock backend for tests and dry runs: behavior is a closure over the
//! launch spec, so scenario tests can act as a scripted worker — creating
//! files in the worktree, claiming blockage, crashing, or substituting a
//! different effective model. It advertises exactly what it enforces
//! (nothing), because no adapter may advertise guarantees its backend
//! cannot enforce (SPEC §20).

use std::sync::Arc;

use super::{
    Backend, Capabilities, LaunchResult, LaunchSpec, PermissionEnforcement, SandboxCapability,
    UsageReport,
};

#[derive(Debug, Clone, Default)]
pub struct MockOutcome {
    /// None = the process died without a terminal result (interrupted).
    pub result_text: Option<String>,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub effective_model: Option<String>,
    pub usage: Option<UsageReport>,
    pub session_id: Option<String>,
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
            permission_enforcement: PermissionEnforcement::Observed,
            sandbox: SandboxCapability::WorktreeOnly,
        })
    }

    fn launch(&self, spec: &LaunchSpec) -> Result<LaunchResult, super::BackendError> {
        let outcome = (self.behavior)(spec);
        Ok(LaunchResult {
            dispatch_id: spec.dispatch_id.clone(),
            exit_code: outcome.exit_code,
            stdout: outcome.result_text.clone().unwrap_or_default(),
            stderr: String::new(),
            timed_out: outcome.timed_out,
            result_text: if outcome.timed_out {
                None
            } else {
                outcome.result_text.clone()
            },
            session_id: outcome.session_id,
            effective_model: outcome.effective_model.or(Some(spec.model.clone())),
            usage: outcome.usage.unwrap_or(UsageReport::unknown()),
            worker_claims_blockage: outcome
                .result_text
                .as_deref()
                .is_some_and(super::claims_blockage),
            cancelled: spec
                .cancel
                .as_ref()
                .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::SeqCst)),
        })
    }
}
