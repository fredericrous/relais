//! A mock backend for the crate's own tests: behavior is a closure over the
//! launch spec, so scenario tests can act as a scripted worker — creating
//! files in the worktree, claiming blockage, crashing, or substituting a
//! different effective model. It advertises exactly what it enforces
//! (nothing), because no adapter may advertise guarantees its backend
//! cannot enforce (SPEC §20).

use std::sync::Arc;

use crate::backend::{
    check_effort, claims_blockage, Backend, BackendError, Capabilities, LaunchResult, LaunchSpec,
    PermissionDenial, PermissionEnforcement, SandboxCapability, UsageReport,
};
use crate::catalog::{EffortSet, Fact};
use crate::policy::EffortId;
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
    pub permission_denials: Vec<PermissionDenial>,
}

pub struct MockBackend {
    pub behavior: Arc<dyn Fn(&LaunchSpec) -> MockOutcome + Send + Sync>,
    /// The CLI-accepted efforts it advertises: one knob for each state
    /// (`Known`, `Unsupported`, `Unknown`). Defaults to the five levels
    /// Claude Code 2.1.284 lists, so a test that never sets it sees a
    /// harness that accepts every effort a policy names.
    pub accepted_efforts: EffortSet,
    /// What its probe reports as the harness version. `test` by default,
    /// which the sandbox preflight cannot compare, so it blocks.
    pub version: String,
}

/// The efforts a default mock accepts.
fn default_accepted_efforts() -> EffortSet {
    Fact::Known(
        ["low", "medium", "high", "xhigh", "max"]
            .into_iter()
            .filter_map(|name| EffortId::parse(name).ok())
            .collect(),
    )
}

impl MockBackend {
    pub fn new(behavior: impl Fn(&LaunchSpec) -> MockOutcome + Send + Sync + 'static) -> Self {
        Self {
            behavior: Arc::new(behavior),
            accepted_efforts: default_accepted_efforts(),
            version: "test".to_string(),
        }
    }

    /// The same backend reporting `version` from its probe.
    pub fn reporting_version(mut self, version: &str) -> Self {
        self.version = version.to_string();
        self
    }

    /// The same backend advertising `accepted` as its CLI-accepted efforts.
    pub fn accepting(mut self, accepted: EffortSet) -> Self {
        self.accepted_efforts = accepted;
        self
    }
}

impl Backend for MockBackend {
    fn name(&self) -> &'static str {
        "mock"
    }

    fn probe(&self) -> Option<Capabilities> {
        Some(Capabilities {
            backend: "mock".into(),
            version: Some(self.version.clone()),
            supports_model: true,
            accepted_efforts: self.accepted_efforts.clone(),
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
        // The same rule the Claude adapter applies: a requested effort is
        // passed on or refused, never dropped.
        if let Some(effort) = &spec.effort {
            check_effort(&self.accepted_efforts, effort, &spec.model, Some("test"))?;
        }
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
            booked_message_ids: Vec::new(),
            unpriced: Vec::new(),
        })
    }
}
