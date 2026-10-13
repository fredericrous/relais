//! The file `relais dataset import-task-types` reads: one JSON object per
//! line, `{"task_id": "...", "task_type": "feature", "reason": "..."}`,
//! written by whoever classified an export (relais links no model, SPEC
//! §16). Parsing is pure and total: every bad line is reported with its
//! line number, so a file with one bad row is refused whole and names it.

use std::collections::{BTreeMap, BTreeSet};

use crate::contract::TaskType;

/// One refused line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelProblem {
    /// 1-based.
    pub line: usize,
    pub reason: String,
}

/// Every line of `text` judged against `known_tasks`: the labels when all
/// lines are good, every problem otherwise. Blank lines are skipped. A
/// task named twice with the same type is one label; with two types, a
/// problem on the second line.
pub fn parse_labels(
    text: &str,
    known_tasks: &BTreeSet<String>,
) -> Result<Vec<(String, TaskType)>, Vec<LabelProblem>> {
    let mut labels: BTreeMap<String, TaskType> = BTreeMap::new();
    let mut order = Vec::new();
    let mut problems = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        if raw.trim().is_empty() {
            continue;
        }
        match parse_line(raw, known_tasks) {
            Err(reason) => problems.push(LabelProblem { line, reason }),
            Ok((task_id, task_type)) => match labels.get(&task_id) {
                Some(earlier) if *earlier != task_type => problems.push(LabelProblem {
                    line,
                    reason: format!(
                        "task `{task_id}` is already labelled `{earlier}` earlier in the file"
                    ),
                }),
                Some(_) => {}
                None => {
                    labels.insert(task_id.clone(), task_type);
                    order.push(task_id);
                }
            },
        }
    }
    if !problems.is_empty() {
        return Err(problems);
    }
    Ok(order
        .into_iter()
        .map(|task_id| {
            let task_type = labels[&task_id];
            (task_id, task_type)
        })
        .collect())
}

fn parse_line(raw: &str, known_tasks: &BTreeSet<String>) -> Result<(String, TaskType), String> {
    let value: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| format!("not a JSON object: {e}"))?;
    let field = |name: &str| -> Result<&str, String> {
        value
            .get(name)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("no string field `{name}`"))
    };
    let task_id = field("task_id")?;
    let text = field("task_type")?;
    let task_type = TaskType::parse_stored(text).ok_or_else(|| {
        format!(
            "unknown task type `{text}`: use one of {}, or unclassified",
            TaskType::declarable_list()
        )
    })?;
    if !known_tasks.contains(task_id) {
        return Err(format!("unknown task id `{task_id}`"));
    }
    Ok((task_id.to_string(), task_type))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> BTreeSet<String> {
        ["t1", "t2"].iter().map(|id| id.to_string()).collect()
    }

    #[test]
    fn good_lines_become_labels_and_blank_lines_are_skipped() {
        let text = "{\"task_id\":\"t1\",\"task_type\":\"feature\",\"reason\":\"a phase\"}\n\n\
                    {\"task_id\":\"t2\",\"task_type\":\"unclassified\"}\n";
        assert_eq!(
            parse_labels(text, &known()),
            Ok(vec![
                ("t1".to_string(), TaskType::Feature),
                ("t2".to_string(), TaskType::Unclassified)
            ])
        );
    }

    #[test]
    fn every_bad_line_is_reported_with_its_number() {
        let text = "{\"task_id\":\"t1\",\"task_type\":\"feature\"}\n\
                    {\"task_id\":\"t9\",\"task_type\":\"feature\"}\n\
                    {\"task_id\":\"t2\",\"task_type\":\"chore\"}\n\
                    nonsense\n";
        let problems = parse_labels(text, &known()).unwrap_err();
        assert_eq!(
            problems.iter().map(|p| p.line).collect::<Vec<_>>(),
            [2, 3, 4]
        );
        assert!(problems[0].reason.contains("t9"));
        assert!(problems[1].reason.contains("infra"), "names the types");
    }

    #[test]
    fn a_task_labelled_twice_differently_is_a_problem_on_the_second_line() {
        let text = "{\"task_id\":\"t1\",\"task_type\":\"ui\"}\n\
                    {\"task_id\":\"t1\",\"task_type\":\"ui\"}\n\
                    {\"task_id\":\"t1\",\"task_type\":\"infra\"}\n";
        let problems = parse_labels(text, &known()).unwrap_err();
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].line, 3);
    }
}
