//! The relais Claude Code plugin ships one worker agent definition per
//! (model, effort) that `native::worker_agent_types()` names, and nothing
//! else in `claude-plugin/agents/`. A pair added there without a file here
//! (or a file nothing names) is a dispatch Claude Code cannot start.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use relais::native::{worker_agent_type, worker_agent_types};

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

#[test]
fn the_plugin_ships_exactly_the_worker_definitions_relais_names() {
    let expected: BTreeSet<String> = worker_agent_types()
        .iter()
        .map(|(model, effort)| format!("{}.md", worker_agent_type(model, effort.as_deref())))
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
        "claude-plugin/agents/ must hold one file per native::worker_agent_types() pair"
    );
}

#[test]
fn each_definition_names_its_own_model_and_effort_and_cannot_spawn() {
    for (model, effort) in worker_agent_types() {
        let name = worker_agent_type(&model, effort.as_deref());
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
            ["Read", "Grep", "Glob", "Edit", "Write", "Bash"],
            "{name}: a worker's tools, with no Agent among them"
        );
        assert!(
            field(&fields, "description").is_some_and(|d| !d.is_empty()),
            "{name}: description"
        );
    }
}
