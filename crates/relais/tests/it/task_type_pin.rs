//! The plugin's `TASK_TYPES` is the Rust `TaskType::DECLARABLE`, in order
//! (docs/plans/2026-10-12-task-types.md). The plugin's own test pins its
//! literal to the same spellings; this one reads the plugin source, which
//! the plugin's sandbox cannot do for itself.

use relais::contract::TaskType;

fn plugin_task_types() -> Vec<String> {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../claude-plugin/hooks/decide.ts"),
    )
    .expect("the plugin source is in the repository");
    let line = source
        .lines()
        .find(|line| line.starts_with("export const TASK_TYPES = ["))
        .expect("decide.ts exports TASK_TYPES on one line");
    let list = line
        .split_once('[')
        .and_then(|(_, rest)| rest.split_once(']'))
        .map(|(list, _)| list)
        .expect("a bracketed list");
    list.split(',')
        .map(|item| item.trim().trim_matches('\'').to_string())
        .collect()
}

#[test]
fn the_plugin_lists_exactly_the_rust_task_types_in_order() {
    let rust: Vec<String> = TaskType::DECLARABLE
        .iter()
        .map(|task_type| task_type.as_str().to_string())
        .collect();
    assert_eq!(plugin_task_types(), rust);
}

#[test]
fn the_plugin_test_literal_matches_too() {
    let test = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../claude-plugin/tests/guards.test.ts"),
    )
    .expect("the plugin test is in the repository");
    let rust: Vec<String> = TaskType::DECLARABLE
        .iter()
        .map(|task_type| format!("'{}'", task_type.as_str()))
        .collect();
    let expected = format!("const rust = [{}]", rust.join(", "));
    assert!(
        test.contains(&expected),
        "guards.test.ts holds `{expected}`"
    );
}
