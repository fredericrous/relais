//! Claude Code integration (SPEC §3).
//!
//! `relais install --claude` proposes a small namespaced set of agent
//! definitions and a `/relais` skill. Preview-first: `--write` applies
//! reviewed changes; merging never replaces unrelated entries. Every
//! owned block carries begin/end markers with a content hash, so
//! uninstall removes only owned, UNCHANGED artifacts and a user-modified
//! owned file is reported, never clobbered. Native agent definitions are
//! advisory defaults — the runner owns spending policy, not frontmatter.

use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::ids::sha256_hex;

pub const BEGIN_MARKER: &str = "<!-- relais:begin";
pub const END_MARKER: &str = "<!-- relais:end -->";

fn owned_file(content: &str) -> String {
    format!(
        "{BEGIN_MARKER} {sha} -->\n{content}\n{END_MARKER}\n",
        sha = sha256_hex(content.as_bytes())
    )
}

/// Files relais owns, in install order.
pub fn owned_files() -> Vec<(PathBuf, String)> {
    vec![
        (
            Path::new("agents").join("relais-research.md"),
            agent_research(),
        ),
        (
            Path::new("agents").join("relais-implementation.md"),
            agent_implementation(),
        ),
        (Path::new("agents").join("relais-review.md"), agent_review()),
        (
            Path::new("skills").join("relais").join("SKILL.md"),
            skill_relais(),
        ),
    ]
}

fn agent_research() -> String {
    let body = r#"---
name: relais-research
description: Read-only investigation worker (relais advisory default). Facts and evidence, never edits.
tools: Read, Grep, Glob
model: haiku
---

You are the research worker of the relais advisory set. Your job is to
investigate and report, not to edit. Answer with:

- concrete facts with file/line evidence,
- what you could NOT establish, stated as unknown,
- suggested entry points for the implementation worker.

You have no write tools. You never guess a decision; if an architectural
answer is required, say that a decision is needed instead of inventing
one. These are advisory defaults: the relais runner, not this
definition, owns the model, budget and acceptance for supervised work.
"#;
    body.trim_end().to_string()
}

fn agent_implementation() -> String {
    let body = r#"---
name: relais-implementation
description: Implementation worker (relais advisory default) for bounded changes under a task contract.
tools: Read, Grep, Glob, Edit, Write, Bash
model: sonnet
---

You are the implementation worker of the relais advisory set. When the
parent delegates a bounded change:

1. Read the task contract: objective, acceptance criteria, write scope.
2. Work ONLY inside the write scope; a diff outside it cannot be
   accepted.
3. Verification commands decide acceptance, not your own summary.
4. You cannot commit, merge, push or publish; you cannot modify
   verification commands, fixtures or policy files.
5. State what you changed and what you verified, and what remains
   unknown.

These are advisory defaults: the relais runner, not this definition,
owns the model, budget and acceptance for supervised work.
"#;
    body.trim_end().to_string()
}

fn agent_review() -> String {
    let body = r#"---
name: relais-review
description: Semantic reviewer (relais advisory default). Reports findings; cannot edit or waive anything.
tools: Read, Grep, Glob
model: fable
---

You are the semantic reviewer of the relais advisory set. You review a
candidate against its contract and report findings only. For each
finding give: file/range, the violated acceptance criterion, the
evidence, and a suggested verification. If there are no findings, say
"FINDINGS: none". You cannot edit files and you cannot waive checks.
Lack of findings is evidence, not proof. These are advisory defaults:
the relais runner owns acceptance.
"#;
    body.trim_end().to_string()
}

fn skill_relais() -> String {
    let body = r#"---
name: relais
description: Route a bounded coding task through the relais supervised runner — explicit model, verification, escalation and accounting.
---

# /relais

Express the requested work as a task contract, then hand it to the
runner. The parent does not supervise intermediate turns.

## Steps

1. Write a contract at `.relais/task.json`:

```json
{
  "schema_version": 1,
  "kind": "change",
  "objective": "<one precise sentence>",
  "base_ref": "HEAD",
  "write_scope": ["<paths the change may touch>"],
  "read_hints": ["<entry points>"],
  "acceptance": ["<criterion a command can verify>", "..."],
  "verification_profile": "<profile from relais.toml>",
  "review": "optional"
}
```

Use `"kind": "inspect"` with evidence criteria for investigations that
must not edit files.

2. Preflight without spending: `relais plan --task .relais/task.json`

3. Execute: `relais run --task .relais/task.json`

4. Read the outcome: accepted (receipt + patch), needs_decision,
needs_review, blocked, failed, budget_exhausted or interrupted. The
artifacts path is printed on every terminal state.

## Rules

- The contract is frozen once the run starts; changing objective, scope,
  acceptance or budget is a new run.
- Acceptance criteria must be executable by the verification profile; a
  worker's completion message is never acceptance.
- If the run needs a decision, make it explicitly — do not let a model
  invent it.
"#;
    body.trim_end().to_string()
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Action {
    Create {
        relative: PathBuf,
    },
    Update {
        relative: PathBuf,
    },
    /// Present, not ours, left untouched.
    SkipForeign {
        relative: PathBuf,
    },
    /// Ours, but the user modified it since; never clobbered.
    Conflict {
        relative: PathBuf,
    },
    Remove {
        relative: PathBuf,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InstallPlan {
    pub actions: Vec<Action>,
}

pub struct InstallRoot {
    /// The directory holding `.claude/` (project) or the user config dir.
    pub claude_dir: PathBuf,
}

impl InstallRoot {
    pub fn project(project_dir: &Path) -> Self {
        Self {
            claude_dir: project_dir.join(".claude"),
        }
    }

    /// User-level installation is explicit, not the default (SPEC §3).
    pub fn user() -> Self {
        Self {
            claude_dir: crate::paths::home_dir().join(".claude"),
        }
    }

    fn owned_path(&self, relative: &Path) -> PathBuf {
        self.claude_dir.join(relative)
    }

    /// The marked block of an in-memory file.
    fn owned_block_text(text: &str) -> Option<String> {
        let begin = text.find(BEGIN_MARKER)?;
        let end = text.find(END_MARKER)? + END_MARKER.len();
        Some(text[begin..end].to_string())
    }

    fn owned_block(path: &Path) -> Option<String> {
        let text = std::fs::read_to_string(path).ok()?;
        let begin = text.find(BEGIN_MARKER)?;
        let end = text.find(END_MARKER)? + END_MARKER.len();
        Some(text[begin..end].to_string())
    }

    /// Plan the install. Preview-first: nothing is written here.
    pub fn plan(&self) -> InstallPlan {
        let mut actions = Vec::new();
        for (relative, content) in owned_files() {
            let path = self.owned_path(&relative);
            if !path.exists() {
                actions.push(Action::Create { relative });
                continue;
            }
            match Self::owned_block(&path) {
                None => actions.push(Action::SkipForeign { relative }),
                Some(existing) => {
                    let current = owned_file(&content);
                    if existing == current.trim_end() {
                        // Unchanged owned file: nothing to do.
                    } else if Self::inner_content_of(&existing) == Some(content.clone()) {
                        // The installed block is an older shipped
                        // version of the same template: update.
                        actions.push(Action::Update { relative });
                    } else {
                        // Ours, but the user changed it since: never
                        // clobbered.
                        actions.push(Action::Conflict { relative });
                    }
                }
            }
        }
        InstallPlan { actions }
    }

    /// The content between the markers: what the block actually carries.
    fn inner_content_of(block: &str) -> Option<String> {
        let begin = block.find(BEGIN_MARKER)?;
        let marker_end = block[begin..].find("-->\n")? + begin + 4;
        let end = block.rfind(END_MARKER)?;
        if marker_end > end {
            return None;
        }
        Some(block[marker_end..end].trim_end_matches('\n').to_string())
    }

    /// Apply a plan: --write. Only Create and Update run; SkipForeign and
    /// Conflict stay untouched and are reported.
    pub fn apply(&self, plan: &InstallPlan) -> std::io::Result<Vec<Action>> {
        let mut applied = Vec::new();
        for action in &plan.actions {
            match action {
                Action::Create { relative } | Action::Update { relative } => {
                    let content = owned_files()
                        .into_iter()
                        .find(|(candidate, _)| candidate == relative)
                        .map(|(_, content)| content)
                        .expect("planned files are owned");
                    let path = self.owned_path(relative);
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    // Merge without replacing unrelated entries: the file
                    // is either wholly ours (markers wrap the whole
                    // content we manage) or wholly foreign (skipped).
                    std::fs::write(&path, owned_file(&content))?;
                    applied.push(action.clone());
                }
                _ => {}
            }
        }
        Ok(applied)
    }

    /// Uninstall removes only owned, UNCHANGED artifacts. A user-modified
    /// owned file and every unrelated file stay exactly where they were.
    pub fn uninstall_plan(&self) -> InstallPlan {
        let mut actions = Vec::new();
        for (relative, content) in owned_files() {
            let path = self.owned_path(&relative);
            if !path.exists() {
                continue;
            }
            match Self::owned_block(&path) {
                None => actions.push(Action::SkipForeign { relative }),
                Some(existing) if existing == owned_file(&content).trim_end() => {
                    actions.push(Action::Remove { relative });
                }
                Some(_) => actions.push(Action::Conflict { relative }),
            }
        }
        InstallPlan { actions }
    }

    pub fn apply_uninstall(&self, plan: &InstallPlan) -> std::io::Result<Vec<Action>> {
        let mut applied = Vec::new();
        for action in &plan.actions {
            if let Action::Remove { relative } = action {
                let path = self.owned_path(relative);
                // If the user added content outside the markers, remove
                // only the owned block and keep their bytes; otherwise
                // the whole file was ours and goes away.
                if let Ok(text) = std::fs::read_to_string(&path) {
                    if let Some(block) = Self::owned_block_text(&text) {
                        let remaining = text.replacen(&block, "", 1);
                        if !remaining.trim().is_empty() {
                            std::fs::write(&path, remaining)?;
                            applied.push(action.clone());
                            continue;
                        }
                    }
                }
                std::fs::remove_file(&path)?;
                // Clean directories we created, but never a directory we
                // did not own end-to-end.
                for ancestor in path.ancestors().skip(1) {
                    if ancestor == self.claude_dir {
                        break;
                    }
                    if std::fs::read_dir(ancestor)
                        .map(|mut entries| entries.next().is_none())
                        .unwrap_or(false)
                    {
                        let _ = std::fs::remove_dir(ancestor);
                    } else {
                        break;
                    }
                }
                applied.push(action.clone());
            }
        }
        Ok(applied)
    }
}

impl InstallPlan {
    pub fn render(&self) -> String {
        let mut out = String::new();
        for action in &self.actions {
            match action {
                Action::Create { relative } => {
                    out.push_str(&format!("  create  {}\n", relative.display()))
                }
                Action::Update { relative } => {
                    out.push_str(&format!("  update  {}\n", relative.display()))
                }
                Action::SkipForeign { relative } => out.push_str(&format!(
                    "  skip    {} (present, not owned — left untouched)\n",
                    relative.display()
                )),
                Action::Conflict { relative } => out.push_str(&format!(
                    "  keep    {} (owned but modified since install — NOT overwritten)\n",
                    relative.display()
                )),
                Action::Remove { relative } => {
                    out.push_str(&format!("  remove  {}\n", relative.display()))
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root() -> (InstallRoot, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "relais-install-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let root = InstallRoot {
            claude_dir: dir.join("project").join(".claude"),
        };
        (root, dir)
    }

    #[test]
    fn install_is_preview_first_and_merge_safe() {
        let (root, dir) = temp_root();
        let plan = root.plan();
        assert_eq!(plan.actions.len(), 4);
        assert!(plan
            .actions
            .iter()
            .all(|action| matches!(action, Action::Create { .. })));
        // Preview wrote nothing.
        assert!(!root.claude_dir.exists());
        let applied = root.apply(&plan).expect("apply");
        assert_eq!(applied.len(), 4);
        let skill = root.claude_dir.join("skills/relais/SKILL.md");
        assert!(skill.is_file());
        // Unrelated content survives untouched.
        let settings = root.claude_dir.join("settings.json");
        std::fs::write(&settings, r#"{"hooks": {"PreToolUse": []}}"#).expect("unrelated");
        let second = root.plan();
        assert!(
            second
                .actions
                .iter()
                .all(|action| matches!(action, Action::Create { .. } | Action::SkipForeign { .. })),
            "own files unchanged, foreign files skipped"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn modified_owned_files_are_kept_not_clobbered() {
        let (root, dir) = temp_root();
        let plan = root.plan();
        root.apply(&plan).expect("apply");
        let agent = root.claude_dir.join("agents/relais-research.md");
        let original = std::fs::read_to_string(&agent).expect("read");
        // Content the user adds OUTSIDE the markers is theirs: no
        // conflict, no update — markers define what install owns.
        std::fs::write(&agent, format!("{original}\n<!-- user notes -->\n")).expect("edit");
        let replan = root.plan();
        assert!(
            replan
                .actions
                .iter()
                .all(|action| !matches!(action, Action::Conflict { .. } | Action::Update { .. })),
            "user content outside the markers is not ours to touch: {:?}",
            replan.actions
        );
        // An edit INSIDE the marked block is a modified owned artifact.
        let inside = original.replace("advisory", "ADVISORY (user edit)");
        std::fs::write(&agent, inside).expect("edit inside the block");
        let replan = root.plan();
        assert!(
            replan
                .actions
                .iter()
                .any(|action| matches!(action, Action::Conflict { .. })),
            "a user-modified owned block is a conflict, not an update"
        );
        let applied = root.apply(&replan).expect("apply");
        assert!(applied.is_empty(), "conflicts are never written");
        let after = std::fs::read_to_string(&agent).expect("read");
        assert!(after.contains("user edit"), "the user's edit survives");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn uninstall_removes_only_owned_unchanged_artifacts() {
        let (root, dir) = temp_root();
        root.apply(&root.plan()).expect("apply");
        // Foreign file stays; owned file gets a user edit.
        std::fs::write(root.claude_dir.join("agents/custom.md"), "# custom\n").expect("foreign");
        let owned = root.claude_dir.join("skills/relais/SKILL.md");
        let original = std::fs::read_to_string(&owned).expect("read");
        std::fs::write(&owned, format!("{original}\n<!-- user note -->\n")).expect("edit");

        let plan = root.uninstall_plan();
        let applied = root.apply_uninstall(&plan).expect("uninstall");
        // The three unchanged agents are removed entirely; the skill's
        // owned block is removed but the user's note survives; the
        // foreign agent was never ours.
        assert_eq!(applied.len(), 4, "{applied:?}");
        assert!(!root.claude_dir.join("agents/relais-research.md").exists());
        let remaining = std::fs::read_to_string(&owned).expect("kept file");
        assert!(
            remaining.contains("user note"),
            "user content survives uninstall: {remaining}"
        );
        assert!(!remaining.contains(BEGIN_MARKER), "the owned block is gone");
        assert!(
            root.claude_dir.join("agents/custom.md").is_file(),
            "foreign file is kept"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn owned_content_carries_verifiable_markers() {
        for (relative, content) in owned_files() {
            let file = owned_file(&content);
            assert!(
                file.starts_with(BEGIN_MARKER),
                "{relative:?} must be marked"
            );
            assert!(file.contains(&sha256_hex(content.as_bytes())));
            assert!(file.trim_end().ends_with(END_MARKER));
        }
    }
}
