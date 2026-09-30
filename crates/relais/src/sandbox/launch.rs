//! A worker's sandboxed launch, and which mode a machine runs its workers in.

use std::path::{Path, PathBuf};

use super::{build_settings, credential_floor, FloorInputs, SettingsInputs};
use crate::backend::SandboxLaunch;
use crate::policy::{expand_home, SandboxSettings};

/// How a machine confines its workers: by permission allowlist alone, or
/// by the OS sandbox as well (SPEC §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerMode {
    Allowlist,
    Sandbox,
}

impl WorkerMode {
    /// The mode `[sandbox]` in machine.toml asks for.
    pub fn of(settings: &SandboxSettings) -> Self {
        if settings.enabled {
            Self::Sandbox
        } else {
            Self::Allowlist
        }
    }
}

/// What one sandboxed launch is computed from. `env` is relais's OWN
/// environment; `launch_env_names` is what the worker's launch environment
/// carries.
pub struct LaunchInputs<'a> {
    pub settings: &'a SandboxSettings,
    pub home: &'a Path,
    pub config_dir: &'a Path,
    pub ledger_path: &'a Path,
    pub env: &'a dyn Fn(&str) -> Option<String>,
    pub launch_env_names: &'a [String],
    pub scratch: &'a Path,
}

/// The sandbox launch for one worker: the credential floor, `[sandbox]`
/// and the scratch directory, folded into the one settings document.
pub fn worker_launch(inputs: &LaunchInputs) -> SandboxLaunch {
    // `validate()` refused any entry that does not expand, so nothing is
    // dropped here.
    let extra_deny: Vec<PathBuf> = inputs
        .settings
        .deny_read
        .iter()
        .filter_map(|entry| expand_home(entry, inputs.home))
        .collect();
    let floor = credential_floor(&FloorInputs {
        home: inputs.home,
        config_dir: inputs.config_dir,
        ledger_path: inputs.ledger_path,
        env: inputs.env,
        launch_env_names: inputs.launch_env_names,
        extra_deny: &extra_deny,
    });
    let settings = build_settings(&SettingsInputs {
        home: inputs.home,
        floor: &floor,
        scratch: inputs.scratch,
        writable: &inputs.settings.writable,
        network: &inputs.settings.network,
    });
    SandboxLaunch {
        settings,
        scratch_dir: inputs.scratch.to_path_buf(),
    }
}

// Unix only: the fixtures are Unix absolute paths.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn the_mode_follows_the_machine_setting() {
        let mut settings = SandboxSettings::default();
        assert_eq!(WorkerMode::of(&settings), WorkerMode::Allowlist);
        settings.enabled = true;
        assert_eq!(WorkerMode::of(&settings), WorkerMode::Sandbox);
    }

    #[test]
    fn the_launch_is_build_settings_over_the_floor_and_the_scratch() {
        let settings = SandboxSettings {
            enabled: true,
            writable: vec!["/opt/out".into()],
            network: vec!["api.anthropic.com".into()],
            deny_read: vec!["~/private".into()],
        };
        let no_env = |_: &str| None;
        let names = ["ANTHROPIC_API_KEY".to_string()];
        let launch = worker_launch(&LaunchInputs {
            settings: &settings,
            home: Path::new("/nonexistent-home"),
            config_dir: Path::new("/nonexistent-home/.config/relais"),
            ledger_path: Path::new("/nonexistent-home/.local/state/relais/ledger.sqlite"),
            env: &no_env,
            launch_env_names: &names,
            scratch: Path::new("/scratch"),
        });
        assert_eq!(launch.scratch_dir, Path::new("/scratch"));
        let settings = &launch.settings;
        let allow_write = settings["sandbox"]["filesystem"]["allowWrite"].to_string();
        assert!(allow_write.contains("/scratch") && allow_write.contains("/opt/out"));
        let deny_read = settings["sandbox"]["filesystem"]["denyRead"].to_string();
        assert!(deny_read.contains("/nonexistent-home/.ssh"), "{deny_read}");
        assert!(
            deny_read.contains("/nonexistent-home/private"),
            "the machine's own deny_read is in the floor: {deny_read}"
        );
        assert_eq!(
            settings["sandbox"]["credentials"]["envVars"],
            serde_json::json!([{"name": "ANTHROPIC_API_KEY", "mode": "deny"}])
        );
        assert_eq!(
            settings["sandbox"]["network"]["allowedDomains"],
            serde_json::json!(["api.anthropic.com"])
        );
    }
}
