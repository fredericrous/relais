//! Whether the machine's Claude Code configuration weakens the sandbox.
//!
//! One rule, applied at the top level, under `sandbox` and under
//! `permissions`: only accepted keys may appear, each only with its stated
//! value. Anything else is a [`Weakening`]. An allowlist, not a denylist:
//! a key this code has never heard of cannot be assumed harmless.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// One reason a configuration file cannot be trusted with the sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Weakening {
    pub source: PathBuf,
    /// The dotted key, e.g. `sandbox.filesystem.allowWrite`.
    pub key: String,
    pub reason: String,
}

impl std::fmt::Display for Weakening {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {}: {}",
            self.source.display(),
            self.key,
            self.reason
        )
    }
}

const NOT_ACCEPTED: &str = "is not an accepted key";

/// Where a finding is being recorded, so the rule functions carry one
/// argument for it rather than a source and a list.
struct Findings<'a> {
    source: &'a Path,
    found: Vec<Weakening>,
}

impl Findings<'_> {
    fn weaken(&mut self, key: &str, reason: &str) {
        self.found.push(Weakening {
            source: self.source.to_path_buf(),
            key: key.to_string(),
            reason: reason.to_string(),
        });
    }

    /// Record `key` unless `accepted`, naming the value it may have.
    fn require(&mut self, key: &str, accepted: bool, stated: &str) {
        if !accepted {
            self.weaken(key, &format!("is accepted only as {stated}"));
        }
    }

    /// The object at `key`, or a finding saying it must be one.
    fn object<'v>(&mut self, key: &str, value: &'v Value) -> Option<&'v Map<String, Value>> {
        let map = value.as_object();
        if map.is_none() {
            self.weaken(key, "must be a JSON object");
        }
        map
    }
}

fn dotted(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}.{key}")
    }
}

fn is_string_array(value: &Value) -> bool {
    value
        .as_array()
        .is_some_and(|items| items.iter().all(Value::is_string))
}

/// Nothing set: null, or an empty object or array.
fn is_blank(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Object(map) => map.is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn judge_settings(found: &mut Findings, prefix: &str, doc: &Value) {
    let Some(map) = found.object(if prefix.is_empty() { "$" } else { prefix }, doc) else {
        return;
    };
    for (name, value) in map {
        let key = dotted(prefix, name);
        match name.as_str() {
            "$schema" | "model" | "cleanupPeriodDays" | "companyAnnouncements" => {}
            "disableAllHooks" => found.require(&key, *value == Value::Bool(true), "true"),
            "sandbox" => judge_sandbox(found, &key, value),
            "permissions" => judge_permissions(found, &key, value),
            _ => found.weaken(&key, NOT_ACCEPTED),
        }
    }
}

fn judge_sandbox(found: &mut Findings, prefix: &str, value: &Value) {
    let Some(map) = found.object(prefix, value) else {
        return;
    };
    for (name, value) in map {
        let key = dotted(prefix, name);
        match name.as_str() {
            "enabled" | "failIfUnavailable" => {
                found.require(&key, *value == Value::Bool(true), "true");
            }
            "allowUnsandboxedCommands" => {
                found.require(&key, *value == Value::Bool(false), "false");
            }
            "autoAllowBashIfSandboxed" => found.require(&key, value.is_boolean(), "a boolean"),
            "filesystem" => judge_filesystem(found, &key, value),
            "network" => judge_network(found, &key, value),
            "credentials" => judge_credentials(found, &key, value),
            _ => found.weaken(&key, NOT_ACCEPTED),
        }
    }
}

fn judge_filesystem(found: &mut Findings, prefix: &str, value: &Value) {
    let Some(map) = found.object(prefix, value) else {
        return;
    };
    for (name, value) in map {
        let key = dotted(prefix, name);
        match name.as_str() {
            "denyRead" | "denyWrite" => {
                found.require(&key, is_string_array(value), "an array of strings");
            }
            _ => found.weaken(&key, NOT_ACCEPTED),
        }
    }
}

fn judge_network(found: &mut Findings, prefix: &str, value: &Value) {
    let Some(map) = found.object(prefix, value) else {
        return;
    };
    for (name, value) in map {
        let key = dotted(prefix, name);
        match name.as_str() {
            "deniedDomains" => {}
            "strictAllowlist" => found.require(&key, *value == Value::Bool(true), "true"),
            _ => found.weaken(&key, NOT_ACCEPTED),
        }
    }
}

fn judge_credentials(found: &mut Findings, prefix: &str, value: &Value) {
    let Some(map) = found.object(prefix, value) else {
        return;
    };
    for (name, value) in map {
        let key = dotted(prefix, name);
        match name.as_str() {
            "files" | "envVars" => judge_credential_entries(found, &key, value),
            _ => found.weaken(&key, NOT_ACCEPTED),
        }
    }
}

/// Every entry must be an object that denies: `mode` is `"deny"` and it
/// carries nothing but `mode`, `path` and `name`. A `mask` entry hands
/// the worker a stand-in it may go on to use.
fn judge_credential_entries(found: &mut Findings, prefix: &str, value: &Value) {
    let Some(entries) = value.as_array() else {
        found.weaken(prefix, "must be an array");
        return;
    };
    for (index, entry) in entries.iter().enumerate() {
        let denies = entry.as_object().is_some_and(|map| {
            map.get("mode") == Some(&Value::from("deny"))
                && map
                    .keys()
                    .all(|k| matches!(k.as_str(), "mode" | "path" | "name"))
        });
        found.require(&format!("{prefix}[{index}]"), denies, "a deny entry");
    }
}

fn judge_permissions(found: &mut Findings, prefix: &str, value: &Value) {
    let Some(map) = found.object(prefix, value) else {
        return;
    };
    for (name, value) in map {
        let key = dotted(prefix, name);
        match name.as_str() {
            "deny" => found.require(&key, value.is_array(), "an array"),
            "disableBypassPermissionsMode" => {}
            "defaultMode" => found.require(
                &key,
                matches!(value.as_str(), Some("default" | "dontAsk")),
                "\"default\" or \"dontAsk\"",
            ),
            _ => found.weaken(&key, NOT_ACCEPTED),
        }
    }
}

/// Judge the managed settings files (`settings`, each with the path it was
/// read from) and the managed MCP file's contents.
pub fn judge_managed(
    settings: &[(PathBuf, Value)],
    mcp: Option<&Value>,
) -> Result<(), Vec<Weakening>> {
    let mut all = Vec::new();
    for (source, doc) in settings {
        let mut found = Findings {
            source,
            found: Vec::new(),
        };
        judge_settings(&mut found, "", doc);
        all.extend(found.found);
    }
    if let Some(mcp) = mcp {
        let has_server = mcp
            .get("mcpServers")
            .and_then(Value::as_object)
            .is_some_and(|servers| !servers.is_empty());
        if has_server {
            let mut found = Findings {
                source: Path::new(MANAGED_MCP),
                found: Vec::new(),
            };
            found.weaken("mcpServers", "a managed MCP server reaches the worker");
            all.extend(found.found);
        }
    }
    if all.is_empty() {
        Ok(())
    } else {
        Err(all)
    }
}

const USER_CONFIG: &str = ".claude.json";
const MANAGED_MCP: &str = "managed-mcp.json";

/// Judge `~/.claude.json`. Not an allowlist — the file holds many
/// legitimate keys — and `mcpServers` does not count, because a worker
/// launched with `--strict-mcp-config` loads none of them. Only a
/// `sandbox`, `permissions`, `hooks` or `env` object (top level, or under
/// `projects.<path>`) is judged, by the rule above, and any
/// `projects.<path>.allowedTools` entry is a weakening.
pub fn judge_user_config(claude_json: &Value) -> Vec<Weakening> {
    let mut found = Findings {
        source: Path::new(USER_CONFIG),
        found: Vec::new(),
    };
    judge_user_scope(&mut found, "", claude_json);
    if let Some(projects) = claude_json.get("projects").and_then(Value::as_object) {
        for (path, project) in projects {
            let prefix = format!("projects.{path}");
            judge_user_scope(&mut found, &prefix, project);
            if project.get("allowedTools").is_some_and(|t| !is_blank(t)) {
                found.weaken(
                    &dotted(&prefix, "allowedTools"),
                    "pre-approves tools for the worker",
                );
            }
        }
    }
    found.found
}

fn judge_user_scope(found: &mut Findings, prefix: &str, scope: &Value) {
    for name in ["sandbox", "permissions", "hooks", "env"] {
        let Some(value) = scope.get(name).filter(|v| !is_blank(v)) else {
            continue;
        };
        let key = dotted(prefix, name);
        match name {
            "sandbox" => judge_sandbox(found, &key, value),
            "permissions" => judge_permissions(found, &key, value),
            _ => found.weaken(&key, NOT_ACCEPTED),
        }
    }
}

/// The managed configuration files under `root` (and `extra_root`, a test
/// root that ADDS to the real one and never replaces it): the settings
/// file, its `managed-settings.d/*.json` drop-ins sorted by name, and the
/// managed MCP file. Only those that exist.
pub fn managed_sources(root: &Path, extra_root: Option<&Path>) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for dir in std::iter::once(root).chain(extra_root) {
        found.push(dir.join("managed-settings.json"));
        found.extend(drop_ins(&dir.join("managed-settings.d")));
        found.push(dir.join(MANAGED_MCP));
    }
    found.retain(|path| path.is_file());
    found
}

fn drop_ins(dir: &Path) -> Vec<PathBuf> {
    // An unreadable or absent directory has no drop-ins to judge.
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp_dir;
    use serde_json::json;

    fn accepted() -> Value {
        json!({
            "$schema": "https://json.schemastore.org/claude-code-settings.json",
            "model": "sonnet",
            "cleanupPeriodDays": 30,
            "companyAnnouncements": ["hello"],
            "disableAllHooks": true,
            "sandbox": {
                "enabled": true,
                "failIfUnavailable": true,
                "allowUnsandboxedCommands": false,
                "autoAllowBashIfSandboxed": false,
                "filesystem": {"denyRead": ["/a"], "denyWrite": ["/b"]},
                "network": {"deniedDomains": ["x.example"], "strictAllowlist": true},
                "credentials": {
                    "files": [{"path": "/c", "mode": "deny"}],
                    "envVars": [{"name": "N", "mode": "deny"}]
                }
            },
            "permissions": {
                "deny": ["Bash(rm:*)"],
                "disableBypassPermissionsMode": "disable",
                "defaultMode": "dontAsk"
            }
        })
    }

    /// `accepted()` with `path` (a dotted path of object keys) set to `value`.
    fn with(path: &str, value: Value) -> Value {
        let mut doc = accepted();
        let mut at = &mut doc;
        let mut keys = path.split('.').peekable();
        while let Some(key) = keys.next() {
            if keys.peek().is_none() {
                at[key] = value;
                break;
            }
            at = &mut at[key];
        }
        doc
    }

    fn judged(doc: Value) -> Vec<Weakening> {
        match judge_managed(&[(PathBuf::from("managed-settings.json"), doc)], None) {
            Ok(()) => Vec::new(),
            Err(found) => found,
        }
    }

    fn only_key(doc: Value) -> String {
        let found = judged(doc);
        assert_eq!(found.len(), 1, "exactly one weakening: {found:?}");
        assert_eq!(found[0].source, PathBuf::from("managed-settings.json"));
        found[0].key.clone()
    }

    #[test]
    fn an_all_accepted_fixture_passes() {
        assert_eq!(judged(accepted()), Vec::new());
    }

    #[test]
    fn each_weakening_is_one_named_finding() {
        let cases = [
            ("hooks", json!({"hooks": {"Stop": []}})),
            (
                "env",
                json!({"env": {"CLAUDE_CODE_SUBPROCESS_ENV_SCRUB": "0"}}),
            ),
            ("apiKeyHelper", json!({"apiKeyHelper": "/bin/x"})),
            ("somethingNew", json!({"somethingNew": 1})),
        ];
        for (expected, extra) in cases {
            let mut doc = accepted();
            doc.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            assert_eq!(only_key(doc), expected);
        }
        let by_path = [
            ("disableAllHooks", json!(false)),
            ("sandbox.enabled", json!(false)),
            ("sandbox.failIfUnavailable", json!(false)),
            ("sandbox.allowUnsandboxedCommands", json!(true)),
            ("sandbox.excludedCommands", json!(["git"])),
            ("sandbox.filesystem.allowRead", json!(["/"])),
            ("sandbox.filesystem.allowWrite", json!(["/"])),
            ("sandbox.filesystem.disabled", json!(true)),
            ("sandbox.enableWeakerNestedSandbox", json!(true)),
            ("sandbox.network.allowUnixSockets", json!(["/s"])),
            ("sandbox.network.allowedDomains", json!(["a.b"])),
            ("sandbox.network.strictAllowlist", json!(false)),
            ("permissions.allow", json!(["Bash"])),
            ("permissions.ask", json!(["Bash"])),
            ("permissions.additionalDirectories", json!(["/"])),
            ("permissions.defaultMode", json!("bypassPermissions")),
            ("permissions.deny", json!("Bash")),
        ];
        for (path, value) in by_path {
            assert_eq!(only_key(with(path, value)), path);
        }
    }

    #[test]
    fn a_masking_credential_entry_is_a_weakening() {
        let doc = with(
            "sandbox.credentials.files",
            json!([{"path": "/c", "mode": "mask"}]),
        );
        assert_eq!(only_key(doc), "sandbox.credentials.files[0]");
        let doc = with(
            "sandbox.credentials.envVars",
            json!([{"name": "N", "mode": "deny"}, {"name": "M", "mode": "mask"}]),
        );
        assert_eq!(only_key(doc), "sandbox.credentials.envVars[1]");
    }

    #[test]
    fn a_managed_mcp_server_is_a_weakening_and_an_empty_file_is_not() {
        let with_server = json!({"mcpServers": {"x": {"command": "x"}}});
        let found = judge_managed(&[], Some(&with_server)).expect_err("a server weakens");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].key, "mcpServers");
        assert_eq!(judge_managed(&[], Some(&json!({"mcpServers": {}}))), Ok(()));
    }

    #[test]
    fn user_config_ignores_mcp_servers_but_judges_the_rest() {
        let quiet = json!({
            "numStartups": 3,
            "mcpServers": {"x": {"command": "x"}},
            "projects": {"/p": {"mcpServers": {"y": {}}, "allowedTools": [], "hooks": {}}}
        });
        assert_eq!(judge_user_config(&quiet), Vec::new());

        let loud = json!({
            "projects": {"/p": {
                "allowedTools": ["Bash(git:*)"],
                "hooks": {"Stop": []},
                "sandbox": {"enabled": false}
            }},
            "env": {"X": "1"}
        });
        let mut keys: Vec<String> = judge_user_config(&loud)
            .into_iter()
            .map(|w| w.key)
            .collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "env",
                "projects./p.allowedTools",
                "projects./p.hooks",
                "projects./p.sandbox.enabled"
            ]
        );
    }

    #[test]
    fn an_extra_root_adds_to_the_real_root_never_replaces_it() {
        let real = temp_dir("managed-real");
        let extra = temp_dir("managed-extra");
        std::fs::write(real.join("managed-settings.json"), "{}").expect("write");
        std::fs::create_dir(real.join("managed-settings.d")).expect("mkdir");
        std::fs::write(real.join("managed-settings.d/20-b.json"), "{}").expect("write");
        std::fs::write(real.join("managed-settings.d/10-a.json"), "{}").expect("write");
        std::fs::write(real.join("managed-settings.d/note.txt"), "").expect("write");
        std::fs::write(extra.join("managed-mcp.json"), "{}").expect("write");

        assert_eq!(
            managed_sources(&real, Some(&extra)),
            vec![
                real.join("managed-settings.json"),
                real.join("managed-settings.d/10-a.json"),
                real.join("managed-settings.d/20-b.json"),
                extra.join("managed-mcp.json"),
            ]
        );
        assert_eq!(managed_sources(&real, None).len(), 3);
    }
}
