//! Which holdout locations a worker's own tool calls named (ADR-0045
//! phase 4, SPEC §10). The scan is a calculation over a transcript's text
//! and the markers; the engine supplies both. Only the marker's name
//! leaves this module, never the text that matched.

use std::collections::BTreeSet;

use crate::holdout::ResolvedHoldout;

/// A kind of location a holdout hides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Marker {
    Store,
    StateDir,
    MachineToml,
    Overlay,
    Config,
}

impl Marker {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Store => "store",
            Self::StateDir => "state_dir",
            Self::MachineToml => "machine_toml",
            Self::Overlay => "overlay",
            Self::Config => "config",
        }
    }
}

/// The strings that name a holdout location, each tagged with its marker.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Markers {
    needles: Vec<(Marker, String)>,
}

impl Markers {
    /// The markers of a run's resolved holdouts: the store URL and its
    /// path without the scheme, the holdouts state directory, the
    /// settings file's absolute path, each overlay path and each
    /// config path inside the store. Empty strings are dropped.
    pub(crate) fn of(holdouts: &[ResolvedHoldout], state_dir: &str, settings_path: &str) -> Self {
        let mut needles = vec![
            (Marker::StateDir, state_dir.to_string()),
            (Marker::MachineToml, settings_path.to_string()),
        ];
        for holdout in holdouts {
            needles.push((Marker::Store, holdout.store.clone()));
            if let Some((_scheme, rest)) = holdout.store.split_once("://") {
                needles.push((Marker::Store, rest.to_string()));
            }
            needles.push((Marker::Overlay, holdout.decl.overlay.clone()));
            if let Some(config) = &holdout.decl.config {
                needles.push((Marker::Config, config.clone()));
            }
        }
        needles.retain(|(_, text)| !text.is_empty());
        Self { needles }
    }

    fn matching<'a>(&'a self, text: &'a str) -> impl Iterator<Item = Marker> + 'a {
        self.needles
            .iter()
            .filter(move |(_, needle)| text.contains(needle.as_str()))
            .map(|(marker, _)| *marker)
    }
}

/// The (tool, marker) pairs a transcript's `tool_use` inputs hit. A line
/// that is not JSON, or holds no `tool_use` block, hits nothing.
pub(crate) fn scan_transcript(text: &str, markers: &Markers) -> BTreeSet<(String, Marker)> {
    let mut hits = BTreeSet::new();
    for line in text.lines() {
        let Ok(record) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let content = record
            .pointer("/message/content")
            .or_else(|| record.get("content"))
            .and_then(serde_json::Value::as_array);
        for block in content.into_iter().flatten() {
            if block.get("type").and_then(serde_json::Value::as_str) != Some("tool_use") {
                continue;
            }
            let tool = block
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            if let Some(input) = block.get("input") {
                for marker in markers_in(input, markers) {
                    hits.insert((tool.to_string(), marker));
                }
            }
        }
    }
    hits
}

fn markers_in(node: &serde_json::Value, markers: &Markers) -> Vec<Marker> {
    match node {
        serde_json::Value::String(text) => markers.matching(text).collect(),
        serde_json::Value::Array(items) => items
            .iter()
            .flat_map(|item| markers_in(item, markers))
            .collect(),
        serde_json::Value::Object(fields) => fields
            .values()
            .flat_map(|field| markers_in(field, markers))
            .collect(),
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn markers() -> Markers {
        Markers {
            needles: vec![
                (Marker::Store, "file:///vault/store".into()),
                (Marker::Store, "/vault/store".into()),
                (Marker::StateDir, "/state/holdouts".into()),
                (
                    Marker::MachineToml,
                    "/home/me/.config/relais/settings-file".into(),
                ),
                (Marker::Overlay, "tests/holdout_overlay.rs".into()),
                (Marker::Config, ".config/nextest.toml".into()),
            ],
        }
    }

    fn tool_use(name: &str, input: &str) -> String {
        format!(
            r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"t1","name":"{name}","input":{input}}}]}}}}"#
        )
    }

    #[test]
    fn each_marker_is_found_in_a_tool_use_input_and_only_its_name_is_kept() {
        let cases = [
            (
                tool_use(
                    "Read",
                    r#"{"file_path":"/home/me/.config/relais/settings-file"}"#,
                ),
                "Read",
                Marker::MachineToml,
            ),
            (
                tool_use(
                    "Bash",
                    r#"{"command":"cat /home/me/.config/relais/settings-file"}"#,
                ),
                "Bash",
                Marker::MachineToml,
            ),
            (
                tool_use("Bash", r#"{"command":"git clone file:///vault/store x"}"#),
                "Bash",
                Marker::Store,
            ),
            (
                tool_use("Grep", r#"{"path":"/vault/store","pattern":"x"}"#),
                "Grep",
                Marker::Store,
            ),
            (
                tool_use("Glob", r#"{"pattern":"/state/holdouts/**"}"#),
                "Glob",
                Marker::StateDir,
            ),
            (
                tool_use("Edit", r#"{"file_path":"tests/holdout_overlay.rs"}"#),
                "Edit",
                Marker::Overlay,
            ),
            (
                tool_use(
                    "Read",
                    r#"{"nested":[{"file_path":".config/nextest.toml"}]}"#,
                ),
                "Read",
                Marker::Config,
            ),
        ];
        for (line, tool, marker) in cases {
            let hits = scan_transcript(&line, &markers());
            assert!(
                hits.contains(&(tool.to_string(), marker)),
                "{line} -> {hits:?}"
            );
        }
    }

    #[test]
    fn a_transcript_naming_none_gives_no_row() {
        let transcript = [
            tool_use("Read", r#"{"file_path":"src/main.rs"}"#),
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"I read /vault/store"}]}}"#
                .to_string(),
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"/home/me/.config/relais/settings-file"}]}}"#
                .to_string(),
            "not json /vault/store".to_string(),
        ]
        .join("\n");
        assert!(scan_transcript(&transcript, &markers()).is_empty());
    }

    #[test]
    fn the_bare_file_name_is_no_exposure_and_the_absolute_path_is() {
        let cases = [
            (r#"{"command":"ls settings-file"}"#, false),
            (
                r#"{"command":"cat /home/me/.config/relais/settings-file"}"#,
                true,
            ),
        ];
        for (input, exposed) in cases {
            let hits = scan_transcript(&tool_use("Bash", input), &markers());
            assert_eq!(!hits.is_empty(), exposed, "{input} -> {hits:?}");
        }
    }

    #[test]
    fn a_marker_hit_twice_in_one_tool_is_one_row() {
        let transcript = [
            tool_use(
                "Read",
                r#"{"file_path":"/home/me/.config/relais/settings-file"}"#,
            ),
            tool_use(
                "Read",
                r#"{"command":"cat /home/me/.config/relais/settings-file"}"#,
            ),
        ]
        .join("\n");
        let hits = scan_transcript(&transcript, &markers());
        assert_eq!(hits.len(), 1, "{hits:?}");
    }
}
