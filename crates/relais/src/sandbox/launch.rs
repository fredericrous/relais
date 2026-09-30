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
    /// The short path a worker's Claude Code temp dir is pointed at: a
    /// symlink to `scratch` (see [`TmpLink`]), or `scratch` itself.
    pub tmp_link: &'a Path,
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
        tmp_link: inputs.tmp_link.to_path_buf(),
    }
}

/// The short path a worker's Claude Code temp dir is pointed at:
/// `<root>/rl-<8 hex of sha256(seed)>` on unix, the scratch itself elsewhere
/// (no symlink to make there). The same seed names the same link: [`TmpLink`]
/// replaces an earlier one only if it dangles or names the same scratch.
pub fn short_tmp_link(root: &Path, seed: &str, scratch: &Path) -> PathBuf {
    if !cfg!(unix) {
        return scratch.to_path_buf();
    }
    let hex = crate::ids::sha256_hex(seed.as_bytes());
    root.join(format!("rl-{}", &hex[..8]))
}

/// The symlink `tmp_link -> scratch` of one launch, removed when this value
/// drops: whichever way the dispatch ends, the link does not outlive it.
/// Holds nothing where the link is the scratch itself.
#[derive(Debug)]
pub struct TmpLink(Option<PathBuf>);

impl TmpLink {
    pub fn create(launch: &SandboxLaunch) -> std::io::Result<Self> {
        if launch.tmp_link == launch.scratch_dir {
            return Ok(Self(None));
        }
        match symlink(&launch.scratch_dir, &launch.tmp_link) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                replace_stale_link(&launch.tmp_link, &launch.scratch_dir)?;
                symlink(&launch.scratch_dir, &launch.tmp_link)?;
            }
            other => other?,
        }
        Ok(Self(Some(launch.tmp_link.clone())))
    }
}

/// Removes a link left by a relais that was killed before its `Drop` ran:
/// one that dangles, or one that already names this launch's own scratch
/// (a resumed or redispatched attempt). A live link to another scratch may
/// be a concurrent worker's `CLAUDE_CODE_TMPDIR` and is never repointed;
/// that, and anything that is not a symlink, fails naming the path.
fn replace_stale_link(link: &Path, scratch: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(link)?.is_symlink() {
        let target = std::fs::read_link(link)?;
        if target == scratch || !link.exists() {
            return std::fs::remove_file(link);
        }
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!(
                "{} is a live link to {}, not this attempt's scratch",
                link.display(),
                target.display()
            ),
        ));
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!("{} exists and is not a symlink", link.display()),
    ))
}

impl Drop for TmpLink {
    fn drop(&mut self) {
        if let Some(link) = &self.0 {
            // Best effort: a link that stays is litter under /tmp that the
            // next create of the same attempt replaces, and that blocks any
            // other attempt from taking the name rather than repointing it.
            std::fs::remove_file(link).ok();
        }
    }
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

// Never reached: off unix `tmp_link` is the scratch, and `create` returns first.
#[cfg(not(unix))]
fn symlink(_target: &Path, _link: &Path) -> std::io::Result<()> {
    Err(std::io::ErrorKind::Unsupported.into())
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
            tmp_link: Path::new("/tmp/rl-0a1b2c3d"),
        });
        assert_eq!(launch.scratch_dir, Path::new("/scratch"));
        assert_eq!(launch.tmp_link, Path::new("/tmp/rl-0a1b2c3d"));
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

    #[test]
    fn the_link_exists_while_the_guard_does_and_not_after() {
        let root = crate::test_support::short_temp_dir("tmp-link");
        let scratch = root.join("scratch");
        std::fs::create_dir(&scratch).unwrap();
        let launch = SandboxLaunch {
            settings: serde_json::json!({}),
            scratch_dir: scratch.clone(),
            tmp_link: root.join("link"),
        };
        let guard = TmpLink::create(&launch).expect("link");
        assert_eq!(
            std::fs::canonicalize(&launch.tmp_link).unwrap(),
            std::fs::canonicalize(&scratch).unwrap()
        );
        drop(guard);
        assert!(std::fs::symlink_metadata(&launch.tmp_link).is_err());
        assert!(scratch.is_dir(), "the scratch is not the link's to remove");
    }

    #[test]
    fn a_stale_link_is_replaced() {
        let root = crate::test_support::short_temp_dir("tmp-link-stale");
        let scratch = root.join("scratch");
        std::fs::create_dir(&scratch).unwrap();
        let launch = SandboxLaunch {
            settings: serde_json::json!({}),
            scratch_dir: scratch.clone(),
            tmp_link: root.join("link"),
        };
        symlink(&root.join("long-gone"), &launch.tmp_link).unwrap();
        let guard = TmpLink::create(&launch).expect("the stale link is replaced");
        assert_eq!(
            std::fs::canonicalize(&launch.tmp_link).unwrap(),
            std::fs::canonicalize(&scratch).unwrap()
        );
        drop(guard);
        assert!(std::fs::symlink_metadata(&launch.tmp_link).is_err());
    }

    #[test]
    fn a_link_to_this_scratch_is_replaced_and_a_live_link_elsewhere_is_not() {
        let root = crate::test_support::short_temp_dir("tmp-link-live");
        let scratch = root.join("scratch");
        let other = root.join("other");
        std::fs::create_dir(&scratch).unwrap();
        std::fs::create_dir(&other).unwrap();
        let launch = SandboxLaunch {
            settings: serde_json::json!({}),
            scratch_dir: scratch.clone(),
            tmp_link: root.join("link"),
        };
        // A resumed attempt: the killed run's link names this same scratch.
        symlink(&scratch, &launch.tmp_link).unwrap();
        drop(TmpLink::create(&launch).expect("our own leftover is replaced"));
        // Another worker's live link is left pointing where it did.
        symlink(&other, &launch.tmp_link).unwrap();
        let err = TmpLink::create(&launch).expect_err("never repointed");
        assert!(err.to_string().contains("live link"), "{err}");
        assert_eq!(std::fs::read_link(&launch.tmp_link).unwrap(), other);
    }

    #[test]
    fn a_regular_file_at_the_link_path_fails_naming_it_and_is_left() {
        let root = crate::test_support::short_temp_dir("tmp-link-file");
        let scratch = root.join("scratch");
        std::fs::create_dir(&scratch).unwrap();
        let launch = SandboxLaunch {
            settings: serde_json::json!({}),
            scratch_dir: scratch,
            tmp_link: root.join("link"),
        };
        std::fs::write(&launch.tmp_link, b"mine").unwrap();
        let err = TmpLink::create(&launch).expect_err("not ours to replace");
        assert!(err.to_string().contains("link"), "{err}");
        assert!(err
            .to_string()
            .contains(&launch.tmp_link.display().to_string()));
        assert_eq!(std::fs::read(&launch.tmp_link).unwrap(), b"mine");
    }
}
