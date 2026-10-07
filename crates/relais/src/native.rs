//! How relais names the native agents it asks the plugin to spawn, and
//! what the plugin sets on the `relais run` it starts. Pure.

use crate::protocol::AgentKind;

/// The agent definition a native dispatch runs as:
/// `relais-<kind>-<model>-<effort>`, with the effort `default` when the
/// route chose none.
pub fn agent_type(kind: AgentKind, model: &str, effort: Option<&str>) -> String {
    format!(
        "relais-{}-{model}-{}",
        kind_word(kind),
        effort.unwrap_or("default")
    )
}

/// The worker's agent definition, for what installs only workers.
pub fn worker_agent_type(model: &str, effort: Option<&str>) -> String {
    agent_type(AgentKind::Worker, model, effort)
}

/// How a kind of agent is named in its definition and in a refusal.
pub fn kind_word(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::Worker => "worker",
        AgentKind::Reviewer => "reviewer",
        AgentKind::Planner => "planner",
    }
}

/// The name the relais plugin gives an agent definition it ships.
pub fn plugin_agent_type(agent_type: &str) -> String {
    format!("relais:{agent_type}")
}

/// What the plugin sets on the `relais run` it starts.
pub const MOD_HOST: &str = "claude-code-mod";

/// The Claude Code versions the plugin is tested against, as `claude
/// --version` prints them: from the first version up to, not including, the
/// second.
pub const SUPPORTED_CLAUDE_CODE: &str = ">= 2.1.291, < 2.2.0";
const CLAUDE_CODE_FROM: (u64, u64, u64) = (2, 1, 291);
const CLAUDE_CODE_BELOW: (u64, u64, u64) = (2, 2, 0);

/// How recent the session's last hello must be for a run to start, and to
/// carry on: the plugin says hello every 30 s.
pub const HELLO_FRESH: std::time::Duration = std::time::Duration::from_secs(60);

/// The `major.minor.patch` in the text `claude --version` printed.
fn claude_code_version(printed: &str) -> Option<(u64, u64, u64)> {
    printed.split_whitespace().find_map(|word| {
        let mut parts = word.split('.').map(str::parse::<u64>);
        match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some(Ok(major)), Some(Ok(minor)), Some(Ok(patch)), None) => {
                Some((major, minor, patch))
            }
            _ => None,
        }
    })
}

/// Why this Claude Code is outside the plugin's supported range, if it is.
/// A version that cannot be read is outside it too: nothing says it works.
pub fn unsupported_claude_code(printed: Option<&str>) -> Option<String> {
    let installed = printed.map(str::trim).filter(|text| !text.is_empty());
    let supported = installed
        .and_then(claude_code_version)
        .is_some_and(|version| version >= CLAUDE_CODE_FROM && version < CLAUDE_CODE_BELOW);
    (!supported).then(|| {
        format!(
            "the relais plugin supports Claude Code {SUPPORTED_CLAUDE_CODE}; installed: {}",
            installed.unwrap_or("unknown (`claude --version` printed nothing)")
        )
    })
}

/// What a `relais run` knows about how it was started.
#[derive(Debug, Clone, Copy)]
pub struct RunOrigin<'a> {
    pub protocol: bool,
    /// The value of `RELAIS_HOST`.
    pub host: Option<&'a str>,
}

/// Why a `relais run` started outside the relais plugin is refused, if it
/// is. A guard against mistakes, not a security boundary.
pub fn origin_refusal(origin: &RunOrigin<'_>) -> Option<String> {
    let from_the_plugin = origin.protocol && origin.host == Some(MOD_HOST);
    (!from_the_plugin).then(|| {
        "relais run starts from Claude Code with the relais plugin: use /relais, or the \
         relais tool, in an interactive session"
            .to_string()
    })
}

/// Why a `relais run` is refused for the session it names, given how long
/// ago the plugin of that session last said hello, if it ever did.
pub fn hello_refusal(age: Option<std::time::Duration>) -> Option<String> {
    let recent = age.is_some_and(|age| age < HELLO_FRESH);
    (!recent).then(|| {
        format!(
            "the relais plugin of this session has not said hello within {}s: start runs from \
             Claude Code with the relais plugin",
            HELLO_FRESH.as_secs()
        )
    })
}

/// The models relais ships agent definitions for. `haiku` has no effort
/// levels, so it ships only the default.
const MODELS: [&str; 4] = ["haiku", "sonnet", "opus", "fable"];

/// The efforts every model but `haiku` ships besides its default.
const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// The kinds of agent relais ships definitions for.
const KINDS: [AgentKind; 3] = [AgentKind::Worker, AgentKind::Reviewer, AgentKind::Planner];

/// Every (model, effort) relais ships a definition for, the same for every
/// kind, in install order; `None` is the model's default effort.
fn model_efforts() -> Vec<(String, Option<String>)> {
    let mut pairs = Vec::new();
    for model in MODELS {
        pairs.push((model.to_string(), None));
        if model != "haiku" {
            pairs.extend(
                EFFORTS
                    .iter()
                    .map(|effort| (model.to_string(), Some(effort.to_string()))),
            );
        }
    }
    pairs
}

/// Every (kind, model, effort) relais ships a native agent definition for.
pub fn agent_types() -> Vec<(AgentKind, String, Option<String>)> {
    KINDS
        .into_iter()
        .flat_map(|kind| {
            model_efforts()
                .into_iter()
                .map(move |(model, effort)| (kind, model, effort))
        })
        .collect()
}

/// The (model, effort) pairs of the worker definitions, for what installs
/// only workers.
pub fn worker_agent_types() -> Vec<(String, Option<String>)> {
    agent_types()
        .into_iter()
        .filter(|(kind, _, _)| *kind == AgentKind::Worker)
        .map(|(_, model, effort)| (model, effort))
        .collect()
}

/// Whether relais ships a definition of this kind for this (model, effort).
pub fn has_definition(kind: AgentKind, model: &str, effort: Option<&str>) -> bool {
    agent_types()
        .iter()
        .any(|(k, m, e)| *k == kind && m == model && e.as_deref() == effort)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_worker_agent_type_names_its_model_and_effort() {
        assert_eq!(
            worker_agent_type("sonnet", Some("medium")),
            "relais-worker-sonnet-medium"
        );
        assert_eq!(
            worker_agent_type("haiku", None),
            "relais-worker-haiku-default"
        );
    }

    #[test]
    fn the_shipped_set_is_haiku_default_and_six_efforts_for_the_rest_of_every_kind() {
        assert_eq!(agent_types().len(), 3 * (1 + 3 * 6));
        assert_eq!(worker_agent_types().len(), 1 + 3 * 6);
        for kind in [AgentKind::Worker, AgentKind::Reviewer, AgentKind::Planner] {
            assert!(has_definition(kind, "haiku", None));
            assert!(!has_definition(kind, "haiku", Some("high")));
            assert!(has_definition(kind, "fable", Some("max")));
            assert!(has_definition(kind, "opus", None));
            assert!(!has_definition(kind, "claude-sonnet-5-5", None));
        }
    }

    #[test]
    fn an_agent_type_names_its_kind_model_and_effort() {
        assert_eq!(
            agent_type(AgentKind::Reviewer, "opus", Some("high")),
            "relais-reviewer-opus-high"
        );
        assert_eq!(
            agent_type(AgentKind::Planner, "haiku", None),
            "relais-planner-haiku-default"
        );
    }

    #[test]
    fn the_plugin_names_its_agents_under_its_own_prefix() {
        assert_eq!(
            plugin_agent_type(&worker_agent_type("sonnet", Some("medium"))),
            "relais:relais-worker-sonnet-medium"
        );
    }

    #[test]
    fn a_claude_code_inside_the_range_is_supported_and_one_outside_names_both() {
        for inside in ["2.1.291 (Claude Code)", "2.1.300", " 2.1.291\n"] {
            assert_eq!(unsupported_claude_code(Some(inside)), None, "{inside}");
        }
        for outside in [
            "2.1.290 (Claude Code)",
            "2.2.0",
            "2.2.1 (Claude Code)",
            "1.9.9",
        ] {
            let refusal = unsupported_claude_code(Some(outside)).expect(outside);
            assert!(refusal.contains(SUPPORTED_CLAUDE_CODE), "{refusal}");
            assert!(refusal.contains(outside.trim()), "{refusal}");
        }
        let unknown = unsupported_claude_code(None).expect("unreadable is unsupported");
        assert!(unknown.contains(SUPPORTED_CLAUDE_CODE), "{unknown}");
        assert!(unsupported_claude_code(Some("not a version")).is_some());
    }

    #[test]
    fn a_run_not_started_by_the_plugin_is_refused_saying_where_to_start_it() {
        let plugin = RunOrigin {
            protocol: true,
            host: Some(MOD_HOST),
        };
        assert_eq!(origin_refusal(&plugin), None);
        let refusals = [
            RunOrigin {
                protocol: false,
                host: Some(MOD_HOST),
            },
            RunOrigin {
                protocol: true,
                host: None,
            },
            RunOrigin {
                protocol: true,
                host: Some("terminal"),
            },
        ];
        for origin in refusals {
            let refusal = origin_refusal(&origin).expect("refused");
            assert!(refusal.contains("Claude Code"), "{refusal}");
            assert!(refusal.contains("relais plugin"), "{refusal}");
        }
    }

    #[test]
    fn a_hello_older_than_a_minute_or_never_refuses_the_run() {
        use std::time::Duration;
        assert_eq!(hello_refusal(Some(Duration::from_secs(59))), None);
        assert!(hello_refusal(Some(Duration::from_secs(60))).is_some());
        assert!(hello_refusal(None).is_some());
    }
}
