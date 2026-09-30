//! The `--settings` JSON a sandboxed worker is launched with.

use std::path::Path;

use serde_json::{json, Value};

use super::Floor;
use crate::policy::expand_home;

/// What the settings are built from. `writable` and `network` are the
/// `[sandbox]` entries as written in machine.toml (validated already);
/// `scratch` is the worker's own scratch directory.
pub struct SettingsInputs<'a> {
    pub home: &'a Path,
    pub floor: &'a Floor,
    pub scratch: &'a Path,
    pub writable: &'a [String],
    pub network: &'a [String],
}

/// The sandbox and permission settings for one worker. There is no `Bash`
/// allow rule, ever: Bash runs only because the sandbox allows it
/// (`autoAllowBashIfSandboxed`), never because a rule names it.
pub fn build_settings(inputs: &SettingsInputs) -> Value {
    let floor = inputs.floor;
    let text = |path: &Path| path.to_string_lossy().into_owned();

    let mut writable = vec![text(inputs.scratch)];
    // `validate()` refused any entry that does not expand, so there is
    // nothing left over to drop here.
    writable.extend(
        inputs
            .writable
            .iter()
            .filter_map(|entry| expand_home(entry, inputs.home))
            .map(|path| text(&path)),
    );

    let deny_read: Vec<String> = floor
        .dirs
        .iter()
        .chain(&floor.files)
        .map(|path| text(path))
        .chain(floor.globs.iter().cloned())
        .collect();

    let mut deny = Vec::new();
    for tool in ["Read", "Edit"] {
        deny.extend(floor.dirs.iter().map(|dir| rule(tool, dir, "/**")));
        deny.extend(floor.files.iter().map(|file| rule(tool, file, "")));
        deny.extend(
            floor
                .globs
                .iter()
                .map(|glob| rule(tool, Path::new(glob), "")),
        );
    }

    let files: Vec<Value> = floor
        .files
        .iter()
        .map(|path| json!({"path": text(path), "mode": "deny"}))
        .collect();
    let env_vars: Vec<Value> = floor
        .env_names
        .iter()
        .map(|name| json!({"name": name, "mode": "deny"}))
        .collect();

    json!({
        "sandbox": {
            "enabled": true,
            "autoAllowBashIfSandboxed": true,
            "allowUnsandboxedCommands": false,
            "failIfUnavailable": true,
            "excludedCommands": [],
            // The floor is denied for WRITES too: a broad `writable` entry
            // (`~/.config`) would otherwise let sandboxed Bash rewrite a
            // floor file (machine.toml, authorized_keys). The narrower
            // deny wins over a broader allow.
            "filesystem": {"allowWrite": writable, "denyRead": deny_read, "denyWrite": deny_read},
            "network": {"allowedDomains": inputs.network, "strictAllowlist": true},
            "credentials": {"files": files, "envVars": env_vars},
        },
        "permissions": {
            "allow": ["Read", "Edit", "Write", "Grep", "Glob"],
            "deny": deny,
        },
    })
}

/// A permission rule for an absolute path: Claude Code spells those with a
/// second leading slash, `//abs/path`.
fn rule(tool: &str, path: &Path, suffix: &str) -> String {
    format!("{tool}(/{}{suffix})", path.display())
}

// Unix only: the fixtures are Unix absolute paths (`/h/.ssh`), which are
// not absolute on Windows, and the OS sandbox these paths feed exists on
// macOS and Linux only.
#[cfg(all(test, unix))]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn floor() -> Floor {
        Floor {
            dirs: vec![PathBuf::from("/h/.ssh")],
            files: vec![PathBuf::from("/h/.netrc")],
            globs: vec!["/s/ledger.sqlite*".to_string()],
            env_names: vec!["ANTHROPIC_API_KEY".to_string()],
        }
    }

    fn built() -> Value {
        build_settings(&SettingsInputs {
            home: Path::new("/h"),
            floor: &floor(),
            scratch: Path::new("/scratch"),
            writable: &["~/cache".to_string(), "/opt/out".to_string()],
            network: &["api.anthropic.com".to_string()],
        })
    }

    fn strings(value: &Value) -> Vec<&str> {
        value
            .as_array()
            .expect("array")
            .iter()
            .map(|v| v.as_str().expect("string"))
            .collect()
    }

    #[test]
    fn the_sandbox_block_carries_the_hard_booleans_and_the_network() {
        let settings = built();
        let sandbox = &settings["sandbox"];
        assert_eq!(sandbox["enabled"], true);
        assert_eq!(sandbox["autoAllowBashIfSandboxed"], true);
        assert_eq!(sandbox["allowUnsandboxedCommands"], false);
        assert_eq!(sandbox["failIfUnavailable"], true);
        assert_eq!(sandbox["excludedCommands"], json!([]));
        assert_eq!(sandbox["network"]["strictAllowlist"], true);
        assert_eq!(
            sandbox["network"]["allowedDomains"],
            json!(["api.anthropic.com"])
        );
        assert_eq!(
            strings(&sandbox["filesystem"]["allowWrite"]),
            ["/scratch", "/h/cache", "/opt/out"]
        );
    }

    #[test]
    fn the_floor_is_denied_for_reading_credentials_and_both_tools() {
        let settings = built();
        assert_eq!(
            strings(&settings["sandbox"]["filesystem"]["denyRead"]),
            ["/h/.ssh", "/h/.netrc", "/s/ledger.sqlite*"]
        );
        assert_eq!(
            strings(&settings["sandbox"]["filesystem"]["denyWrite"]),
            ["/h/.ssh", "/h/.netrc", "/s/ledger.sqlite*"],
            "the floor is write-protected too, whatever `writable` allows"
        );
        assert_eq!(
            settings["sandbox"]["credentials"],
            json!({
                "files": [{"path": "/h/.netrc", "mode": "deny"}],
                "envVars": [{"name": "ANTHROPIC_API_KEY", "mode": "deny"}],
            })
        );
        assert_eq!(
            strings(&settings["permissions"]["deny"]),
            [
                "Read(//h/.ssh/**)",
                "Read(//h/.netrc)",
                "Read(//s/ledger.sqlite*)",
                "Edit(//h/.ssh/**)",
                "Edit(//h/.netrc)",
                "Edit(//s/ledger.sqlite*)",
            ]
        );
    }

    #[test]
    fn no_bash_rule_is_ever_allowed() {
        let settings = built();
        let allow = strings(&settings["permissions"]["allow"]);
        assert_eq!(allow, ["Read", "Edit", "Write", "Grep", "Glob"]);
        assert!(!allow.iter().any(|rule| rule.starts_with("Bash")));
    }
}
