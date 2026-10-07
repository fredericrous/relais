//! The relais Claude Code plugin, installed from the binary itself.
//!
//! `relais install --claude` writes a directory marketplace under
//! relais's state directory — `.claude-plugin/marketplace.json` and the
//! plugin, embedded in this binary, at `plugins/relais/` — and has Claude
//! Code register and install it: `claude plugin marketplace add <dir>` and
//! `claude plugin install relais@relais-local` the first time,
//! `marketplace update` and `plugin update` once it is installed. Claude
//! Code copies the plugin into its own cache by version, so the plugin's
//! `plugin.json` carries relais's version and every release refreshes the
//! copy. `uninstall` undoes both registrations and removes the directory.
//!
//! The plugin carries the mod, the agent definitions and the `/relais`
//! skill (plan: all-native-mod, Design §1).

use serde::Deserialize;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::Mode;
use crate::procs::{run_with_timeout, Ended};

/// The marketplace relais registers, and the plugin it holds.
pub const MARKETPLACE: &str = "relais-local";
pub const PLUGIN: &str = "relais";
pub const PLUGIN_ID: &str = "relais@relais-local";

/// What one `claude plugin …` call may take: a copy into Claude Code's
/// cache, never a download.
const STEP_TIMEOUT: Duration = Duration::from_secs(120);

/// Every plugin file, as the path inside the plugin and its bytes. Built
/// by `build.rs` from `claude-plugin/`.
const EMBEDDED: &[(&str, &[u8])] = include!(concat!(env!("OUT_DIR"), "/plugin_files.rs"));

/// The version of this relais, which is the version the plugin carries.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The paths of the embedded plugin files, relative to the plugin.
pub fn embedded_paths() -> Vec<&'static str> {
    EMBEDDED.iter().map(|(path, _)| *path).collect()
}

/// An embedded file's text, by its path inside the plugin.
pub fn embedded_text(relative: &str) -> Option<&'static str> {
    EMBEDDED
        .iter()
        .find(|(path, _)| *path == relative)
        .and_then(|(_, bytes)| std::str::from_utf8(bytes).ok())
}

fn marketplace_manifest() -> String {
    let manifest = json!({
        "name": MARKETPLACE,
        "owner": {"name": "relais"},
        "plugins": [{
            "name": PLUGIN,
            "source": format!("./plugins/{PLUGIN}"),
            "description": "relais as a Claude Code plugin: starts relais runs and shows \
                            every agent they dispatch as a native agent",
        }],
    });
    let mut text = serde_json::to_string_pretty(&manifest).expect("a json! value renders");
    text.push('\n');
    text
}

fn plugin_dir(marketplace: &Path) -> PathBuf {
    marketplace.join("plugins").join(PLUGIN)
}

/// Lay the marketplace out in `dir`: the manifest, and the embedded
/// plugin in place of whatever an earlier relais left at `plugins/relais/`.
pub fn write_marketplace(dir: &Path) -> std::io::Result<()> {
    let plugin = plugin_dir(dir);
    match std::fs::remove_dir_all(&plugin) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    for (relative, bytes) in EMBEDDED {
        let path = plugin.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, bytes)?;
    }
    let manifest = dir.join(".claude-plugin").join("marketplace.json");
    std::fs::create_dir_all(manifest.parent().expect("a file in a directory"))?;
    std::fs::write(manifest, marketplace_manifest())
}

/// One `claude plugin …` invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    MarketplaceAdd,
    MarketplaceUpdate,
    Install,
    Update,
    Uninstall,
    MarketplaceRemove,
}

impl Step {
    /// The arguments after `claude`.
    pub fn args(self, marketplace: &Path) -> Vec<String> {
        let words = |words: &[&str]| words.iter().map(|word| word.to_string()).collect();
        match self {
            Step::MarketplaceAdd => {
                let mut args: Vec<String> = words(&["plugin", "marketplace", "add"]);
                args.push(marketplace.to_string_lossy().into_owned());
                args
            }
            Step::MarketplaceUpdate => words(&["plugin", "marketplace", "update", MARKETPLACE]),
            Step::Install => words(&["plugin", "install", PLUGIN_ID]),
            Step::Update => words(&["plugin", "update", PLUGIN_ID]),
            Step::Uninstall => words(&["plugin", "uninstall", PLUGIN_ID]),
            Step::MarketplaceRemove => words(&["plugin", "marketplace", "remove", MARKETPLACE]),
        }
    }

    /// Whether Claude Code's "not found" is an answer to accept: an
    /// uninstall or a removal of something that is already gone is the
    /// state it was asked for.
    fn tolerates_absence(self) -> bool {
        match self {
            Step::Uninstall | Step::MarketplaceRemove => true,
            Step::MarketplaceAdd | Step::MarketplaceUpdate | Step::Install | Step::Update => false,
        }
    }
}

/// How a step that ran ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepEnd {
    Done,
    /// Claude Code said there was nothing to remove.
    AlreadyAbsent,
}

/// Why a call to `claude`, or the marketplace directory, did not work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginError {
    /// No `claude` to run.
    NoClaude(String),
    /// `claude` did not run, or did not answer.
    NotRun { args: String, detail: String },
    /// `claude` ran and refused; its stderr is the reason.
    Refused {
        args: String,
        ended: String,
        stderr: String,
    },
    /// `claude plugin list --json` answered with something else.
    Unreadable(String),
    /// The marketplace directory could not be written or removed.
    Directory { path: PathBuf, detail: String },
}

impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PluginError::NoClaude(detail) => write!(f, "no Claude Code to run: {detail}"),
            PluginError::NotRun { args, detail } => {
                write!(f, "`claude {args}` did not run: {detail}")
            }
            PluginError::Refused {
                args,
                ended,
                stderr,
            } => match stderr.trim() {
                "" => write!(f, "`claude {args}` {ended} and printed nothing on stderr"),
                stderr => write!(f, "`claude {args}` {ended}:\n{stderr}"),
            },
            PluginError::Unreadable(detail) => {
                write!(
                    f,
                    "`claude plugin list --json` printed no plugin list: {detail}"
                )
            }
            PluginError::Directory { path, detail } => {
                write!(f, "{}: {detail}", path.display())
            }
        }
    }
}

impl std::error::Error for PluginError {}

/// What `claude plugin list --json` says about the relais plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installed {
    No,
    Yes { version: String, enabled: bool },
}

#[derive(Deserialize)]
struct Listed {
    id: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    enabled: bool,
}

/// The relais plugin's entry in a `plugin list --json` document. When
/// several scopes list it, an enabled entry is the one that counts.
pub fn parse_installed(list_json: &str) -> Result<Installed, PluginError> {
    let listed: Vec<Listed> =
        serde_json::from_str(list_json).map_err(|e| PluginError::Unreadable(e.to_string()))?;
    let mut ours = listed.into_iter().filter(|entry| entry.id == PLUGIN_ID);
    let Some(first) = ours.next() else {
        return Ok(Installed::No);
    };
    let chosen = std::iter::once(first)
        .chain(ours)
        .max_by_key(|entry| entry.enabled)
        .expect("at least the first entry");
    Ok(Installed::Yes {
        version: chosen.version,
        enabled: chosen.enabled,
    })
}

/// The Claude Code binary relais talks to.
pub struct Claude {
    binary: PathBuf,
}

impl Claude {
    /// `RELAIS_CLAUDE_BIN`, else `claude` on PATH.
    pub fn discover() -> Result<Self, PluginError> {
        crate::adapter::claude::ClaudeBackend::discover()
            .map(|backend| Self {
                binary: backend.binary().to_path_buf(),
            })
            .map_err(|e| PluginError::NoClaude(e.to_string()))
    }

    fn run(&self, args: &[String]) -> Result<String, PluginError> {
        let shown = args.join(" ");
        let mut command = std::process::Command::new(&self.binary);
        command.args(args);
        let end = run_with_timeout(command, STEP_TIMEOUT, None, None).map_err(|e| {
            PluginError::NotRun {
                args: shown.clone(),
                detail: e.to_string(),
            }
        })?;
        match end.ended {
            Ended::Exited(0) => Ok(end.stdout),
            ended => Err(PluginError::Refused {
                args: shown,
                ended: ended.describe(),
                stderr: end.stderr,
            }),
        }
    }

    /// Whether the relais plugin is installed, and at what version.
    pub fn installed(&self) -> Result<Installed, PluginError> {
        let listing = self.run(&["plugin".into(), "list".into(), "--json".into()])?;
        parse_installed(&listing)
    }

    fn step(&self, step: Step, marketplace: &Path) -> Result<StepEnd, PluginError> {
        match self.run(&step.args(marketplace)) {
            Ok(_) => Ok(StepEnd::Done),
            Err(PluginError::Refused { stderr, .. })
                if step.tolerates_absence() && stderr.to_lowercase().contains("not found") =>
            {
                Ok(StepEnd::AlreadyAbsent)
            }
            Err(e) => Err(e),
        }
    }
}

/// The steps an install takes: a first one registers and installs, one
/// over an installed plugin refreshes both.
fn install_steps(installed: &Installed) -> Vec<Step> {
    match installed {
        Installed::No => vec![Step::MarketplaceAdd, Step::Install],
        Installed::Yes { .. } => vec![Step::MarketplaceUpdate, Step::Update],
    }
}

/// What an install or uninstall of the plugin planned and did. In preview
/// nothing ran; otherwise `done` holds the steps that ended, in order, and
/// `failure` the one that did not, after which nothing else ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginReport {
    pub mode: Mode,
    pub directory: PathBuf,
    pub steps: Vec<Step>,
    pub done: Vec<(Step, StepEnd)>,
    pub failure: Option<PluginError>,
}

impl PluginReport {
    fn new(mode: Mode, directory: &Path) -> Self {
        Self {
            mode,
            directory: directory.to_path_buf(),
            steps: Vec::new(),
            done: Vec::new(),
            failure: None,
        }
    }

    fn failed(mut self, failure: PluginError) -> Self {
        self.failure = Some(failure);
        self
    }

    /// Run `steps` in order against `claude`, stopping at the first failure.
    fn run_steps(mut self, claude: &Claude) -> Self {
        for step in self.steps.clone() {
            match claude.step(step, &self.directory) {
                Ok(end) => self.done.push((step, end)),
                Err(e) => return self.failed(e),
            }
        }
        self
    }

    /// The lines `install`/`uninstall` print for the plugin, beside the
    /// file actions: the directory an install writes, then each step, run
    /// or to be run.
    pub fn render(&self) -> String {
        let mut out = format!("plugin {PLUGIN_ID}\n");
        let registers = self
            .steps
            .iter()
            .any(|step| matches!(step, Step::MarketplaceAdd | Step::MarketplaceUpdate));
        if registers {
            out.push_str(&format!("  write   {}\n", self.directory.display()));
        }
        for step in &self.steps {
            let command = format!("claude {}", step.args(&self.directory).join(" "));
            match self.done.iter().find(|(done, _)| done == step) {
                Some((_, StepEnd::Done)) => out.push_str(&format!("  ran     {command}\n")),
                Some((_, StepEnd::AlreadyAbsent)) => {
                    out.push_str(&format!("  absent  {command} (already gone)\n"))
                }
                None => out.push_str(&format!("  run     {command}\n")),
            }
        }
        out
    }
}

/// Install, or with `Mode::Preview` plan installing, the plugin whose
/// marketplace lives in `directory`.
pub fn install(mode: Mode, directory: &Path) -> PluginReport {
    let report = PluginReport::new(mode, directory);
    let claude = match Claude::discover() {
        Ok(claude) => claude,
        Err(e) => return report.failed(e),
    };
    let steps = match claude.installed() {
        Ok(installed) => install_steps(&installed),
        Err(e) => return report.failed(e),
    };
    let report = PluginReport { steps, ..report };
    match mode {
        Mode::Preview => report,
        Mode::Apply => match write_marketplace(directory) {
            Ok(()) => report.run_steps(&claude),
            Err(e) => {
                let failure = PluginError::Directory {
                    path: directory.to_path_buf(),
                    detail: e.to_string(),
                };
                report.failed(failure)
            }
        },
    }
}

/// Uninstall, or plan uninstalling, the plugin and remove its marketplace
/// directory. Each step is accepted when Claude Code says it is already
/// gone.
pub fn uninstall(mode: Mode, directory: &Path) -> PluginReport {
    let report = PluginReport {
        steps: vec![Step::Uninstall, Step::MarketplaceRemove],
        ..PluginReport::new(mode, directory)
    };
    match mode {
        Mode::Preview => report,
        Mode::Apply => {
            let claude = match Claude::discover() {
                Ok(claude) => claude,
                Err(e) => return report.failed(e),
            };
            let report = report.run_steps(&claude);
            if report.failure.is_some() {
                return report;
            }
            match std::fs::remove_dir_all(directory) {
                Ok(()) => report,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => report,
                Err(e) => {
                    let failure = PluginError::Directory {
                        path: directory.to_path_buf(),
                        detail: e.to_string(),
                    };
                    report.failed(failure)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../claude-plugin")
    }

    /// The files the plugin ships, read off the disk by the rules the
    /// plan states: everything under `claude-plugin/` except its tests,
    /// its generated types and the tsconfig.
    fn shipped_on_disk(dir: &Path, found: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).expect("read the plugin directory") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                shipped_on_disk(&path, found);
                continue;
            }
            let relative = path
                .strip_prefix(plugin_root())
                .expect("under the plugin")
                .to_string_lossy()
                .replace('\\', "/");
            let development_only = relative.starts_with("tests/")
                || relative.starts_with(".claude-plugin/types/")
                || relative == "tsconfig.json";
            if !development_only {
                found.push(relative);
            }
        }
    }

    #[test]
    fn the_binary_embeds_exactly_the_plugin_files_on_disk() {
        let mut on_disk = Vec::new();
        shipped_on_disk(&plugin_root(), &mut on_disk);
        on_disk.sort();
        let mut embedded = embedded_paths();
        embedded.sort();
        assert_eq!(embedded, on_disk);
        for (relative, bytes) in EMBEDDED {
            assert_eq!(
                std::fs::read(plugin_root().join(relative)).expect("read"),
                *bytes,
                "{relative} is embedded as it is on disk"
            );
        }
        for needed in [
            ".claude-plugin/plugin.json",
            "hooks/hooks.json",
            "skills/relais/SKILL.md",
        ] {
            assert!(embedded.contains(&needed), "{needed}");
        }
        for development_only in ["tests/", ".claude-plugin/types/", "tsconfig.json"] {
            assert!(
                embedded
                    .iter()
                    .all(|path| !path.starts_with(development_only)),
                "{development_only} is not shipped"
            );
        }
    }

    #[test]
    fn the_plugin_carries_the_version_of_this_relais() {
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(plugin_root().join(".claude-plugin/plugin.json")).expect("read"),
        )
        .expect("plugin.json is json");
        assert_eq!(manifest["version"], version());
        assert_eq!(manifest["name"], PLUGIN);
    }

    #[test]
    fn the_marketplace_names_the_plugin_it_holds() {
        let dir = crate::test_support::temp_dir("marketplace");
        write_marketplace(&dir).expect("write");
        let manifest: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join(".claude-plugin/marketplace.json")).expect("read"),
        )
        .expect("json");
        assert_eq!(manifest["name"], MARKETPLACE);
        assert_eq!(manifest["owner"]["name"], "relais");
        assert_eq!(manifest["plugins"][0]["name"], PLUGIN);
        assert_eq!(manifest["plugins"][0]["source"], "./plugins/relais");
        for relative in embedded_paths() {
            assert!(
                dir.join("plugins/relais").join(relative).is_file(),
                "{relative}"
            );
        }
    }

    #[test]
    fn a_rewritten_marketplace_drops_what_an_earlier_relais_shipped() {
        let dir = crate::test_support::temp_dir("marketplace-rewrite");
        let stale = dir.join("plugins/relais/hooks/retired.ts");
        std::fs::create_dir_all(stale.parent().expect("parent")).expect("mkdir");
        std::fs::write(&stale, "old").expect("write");
        write_marketplace(&dir).expect("write");
        assert!(!stale.exists());
    }

    #[test]
    fn each_step_is_one_claude_plugin_command() {
        let dir = Path::new("/m");
        let args = |step: Step| step.args(dir).join(" ");
        assert_eq!(args(Step::MarketplaceAdd), "plugin marketplace add /m");
        assert_eq!(
            args(Step::MarketplaceUpdate),
            "plugin marketplace update relais-local"
        );
        assert_eq!(args(Step::Install), "plugin install relais@relais-local");
        assert_eq!(args(Step::Update), "plugin update relais@relais-local");
        assert_eq!(
            args(Step::Uninstall),
            "plugin uninstall relais@relais-local"
        );
        assert_eq!(
            args(Step::MarketplaceRemove),
            "plugin marketplace remove relais-local"
        );
    }

    #[test]
    fn a_first_install_registers_and_installs_and_a_later_one_updates() {
        assert_eq!(
            install_steps(&Installed::No),
            vec![Step::MarketplaceAdd, Step::Install]
        );
        let installed = Installed::Yes {
            version: "0.1.0".into(),
            enabled: false,
        };
        assert_eq!(
            install_steps(&installed),
            vec![Step::MarketplaceUpdate, Step::Update]
        );
    }

    #[test]
    fn the_list_is_read_for_the_relais_plugin_only() {
        let list = r#"[
            {"id": "other@somewhere", "version": "9.9.9", "scope": "user", "enabled": true},
            {"id": "relais@relais-local", "version": "0.9.0", "scope": "user", "enabled": true,
             "installPath": "/c", "readFromFolder": true}
        ]"#;
        assert_eq!(
            parse_installed(list),
            Ok(Installed::Yes {
                version: "0.9.0".into(),
                enabled: true
            })
        );
        assert_eq!(parse_installed("[]"), Ok(Installed::No));
        assert!(matches!(
            parse_installed("not json"),
            Err(PluginError::Unreadable(_))
        ));
    }

    #[test]
    fn an_enabled_entry_wins_over_a_disabled_one_in_another_scope() {
        let list = r#"[
            {"id": "relais@relais-local", "version": "0.8.0", "enabled": false},
            {"id": "relais@relais-local", "version": "0.9.0", "enabled": true}
        ]"#;
        assert_eq!(
            parse_installed(list),
            Ok(Installed::Yes {
                version: "0.9.0".into(),
                enabled: true
            })
        );
    }

    #[test]
    fn a_refusal_carries_claudes_stderr() {
        let refusal = PluginError::Refused {
            args: "plugin install relais@relais-local".into(),
            ended: "exited 1".into(),
            stderr: "marketplace is broken\n".into(),
        };
        let shown = refusal.to_string();
        assert!(
            shown.contains("plugin install relais@relais-local"),
            "{shown}"
        );
        assert!(shown.contains("marketplace is broken"), "{shown}");
    }
}
