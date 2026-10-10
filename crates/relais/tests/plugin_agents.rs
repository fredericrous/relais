//! The relais Claude Code plugin ships one agent definition per (kind,
//! model, effort) that `native::agent_types()` names, and nothing else in
//! `claude-plugin/agents/`. A triple added there without a file here (or a
//! file nothing names) is a dispatch Claude Code cannot start.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use relais::native::{agent_type, agent_types, kind_word};
use relais::protocol::AgentKind;

fn agents_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../claude-plugin/agents")
}

/// The `key: value` lines between the two `---` fences.
fn frontmatter(text: &str) -> Vec<(String, String)> {
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some("---"), "a definition opens with `---`");
    lines
        .take_while(|line| *line != "---")
        .map(|line| {
            let (key, value) = line
                .split_once(": ")
                .unwrap_or_else(|| panic!("a frontmatter line is `key: value`: {line:?}"));
            (key.to_string(), value.to_string())
        })
        .collect()
}

fn field<'a>(fields: &'a [(String, String)], key: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, value)| value.as_str())
}

/// The tools each kind of agent may use; none has Agent.
fn tools_of(kind: AgentKind) -> &'static [&'static str] {
    match kind {
        AgentKind::Worker => &["Read", "Grep", "Glob", "LSP", "Edit", "Write", "Bash"],
        AgentKind::Reviewer => &["Read", "Grep", "Glob", "LSP"],
        AgentKind::Planner => &["Read", "Grep", "Glob", "LSP", "Bash"],
    }
}

/// The turns each kind of agent is capped at, when it is.
fn max_turns_of(kind: AgentKind) -> Option<&'static str> {
    match kind {
        AgentKind::Planner => Some("8"),
        AgentKind::Worker | AgentKind::Reviewer => None,
    }
}

#[test]
fn the_plugin_ships_exactly_the_agent_definitions_relais_names() {
    let expected: BTreeSet<String> = agent_types()
        .iter()
        .map(|(kind, model, effort)| format!("{}.md", agent_type(*kind, model, effort.as_deref())))
        .collect();
    let shipped: BTreeSet<String> = fs::read_dir(agents_dir())
        .expect("claude-plugin/agents exists")
        .map(|entry| {
            entry
                .expect("a readable entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(
        shipped, expected,
        "claude-plugin/agents/ must hold one file per native::agent_types() triple"
    );
}

#[test]
fn each_definition_names_its_own_kind_model_and_effort_and_cannot_spawn() {
    for (kind, model, effort) in agent_types() {
        let name = agent_type(kind, &model, effort.as_deref());
        let path = agents_dir().join(format!("{name}.md"));
        let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let fields = frontmatter(&text);
        assert_eq!(field(&fields, "name"), Some(name.as_str()), "{name}: name");
        assert_eq!(
            field(&fields, "model"),
            Some(model.as_str()),
            "{name}: model"
        );
        assert_eq!(
            field(&fields, "effort"),
            effort.as_deref(),
            "{name}: effort (absent for a model's default)"
        );
        let tools = field(&fields, "tools").unwrap_or_else(|| panic!("{name}: tools"));
        let tools: Vec<&str> = tools.split(", ").collect();
        assert_eq!(
            tools,
            tools_of(kind),
            "{name}: a {}'s tools, with no Agent among them",
            kind_word(kind)
        );
        assert!(
            text.contains("Find and follow code with LSP"),
            "{name}: says to navigate with LSP, or the tool goes unused"
        );
        assert_eq!(
            field(&fields, "maxTurns"),
            max_turns_of(kind),
            "{name}: maxTurns"
        );
        let described = format!("relais {}.", kind_word(kind));
        assert!(
            field(&fields, "description").is_some_and(|d| d.starts_with(&described)),
            "{name}: description"
        );
    }
}
