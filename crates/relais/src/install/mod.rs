//! Claude Code integration (SPEC §3).
//!
//! `relais install --claude` proposes a small namespaced set of agent
//! definitions and skills. Preview-first: `--write` applies
//! reviewed changes; merging never replaces unrelated entries. Every
//! owned block carries begin/end markers with a content hash, so
//! uninstall removes only owned, UNCHANGED artifacts and a user-modified
//! owned file is reported, never clobbered. Native agent definitions are
//! advisory defaults — the runner owns spending policy, not frontmatter.
//!
//! "Unchanged" is judged against the sha the marker recorded AT INSTALL
//! TIME, never against whatever the shipped template says today: a block
//! the user has not touched stays ours after the template moves on, and
//! is then an `Update`. Only the marked block is rewritten; bytes the
//! user added outside it are theirs, on update exactly as on uninstall.
//!
//! Every write of an owned file goes through [`write_atomic`]: a
//! temporary sibling, fsynced, renamed over the destination. A truncating
//! write that dies half way (ENOSPC, a kill) would destroy the user's
//! text around the block and leave a file relais then refuses to touch
//! for ever (C1); a rename either happened or did not.

use serde::Serialize;
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::ids::sha256_hex;

pub mod settings;
pub use settings::{HookEventAction, HookEventPlan, HooksPlan};

pub const BEGIN_MARKER: &str = "<!-- relais:begin";
/// The begin marker's spelling INSIDE YAML frontmatter. Claude Code reads
/// a skill's or an agent's frontmatter only when `---` is the file's
/// first line, so an HTML comment above it turns the whole comment into
/// the description and loses the name. A YAML comment on the line after
/// the opener is invisible to the frontmatter parser and still names the
/// block and its sha.
pub const YAML_BEGIN_MARKER: &str = "# relais:begin";
pub const END_MARKER: &str = "<!-- relais:end -->";
const FRONTMATTER_OPENER: &str = "---\n";

/// A template split into the part that must stay first in the file (the
/// frontmatter opener, or nothing) and the part the block owns and hashes.
fn split_template(content: &str) -> (&'static str, &str) {
    match content.strip_prefix(FRONTMATTER_OPENER) {
        Some(rest) => (FRONTMATTER_OPENER, rest.trim_end_matches('\n')),
        None => ("", content.trim_end_matches('\n')),
    }
}

/// The sha a marker records for this template: of the owned part only,
/// so the reader and the writer describe the same bytes.
fn template_sha(content: &str) -> String {
    sha256_hex(split_template(content).1.as_bytes())
}

/// The marked block alone, with the sha of the content as installed.
/// Trailing newlines are trimmed before hashing AND before writing, so
/// the sha always describes exactly the bytes `read_block` reads back.
/// A template with frontmatter gets the YAML spelling of the begin
/// marker; the caller keeps the opener above it.
fn owned_block_for(content: &str) -> String {
    let (opener, body) = split_template(content);
    let sha = sha256_hex(body.as_bytes());
    if opener.is_empty() {
        format!("{BEGIN_MARKER} {sha} -->\n{body}\n{END_MARKER}")
    } else {
        format!("{YAML_BEGIN_MARKER} {sha}\n{body}\n{END_MARKER}")
    }
}

/// A whole file relais owns end to end: the opener (when the template
/// has one), the block, and nothing else.
fn owned_file(content: &str) -> String {
    let (opener, _) = split_template(content);
    format!("{opener}{}\n", owned_block_for(content))
}

/// Replace a file's contents so that a failed write leaves the previous
/// contents intact: a temporary file IN THE SAME DIRECTORY (rename is
/// only atomic within a filesystem), flushed and fsynced, then renamed
/// over the destination. `rename(2)` replaces atomically on POSIX and
/// `ReplaceFile`/`MoveFileEx` semantics give the same guarantee on
/// Windows, where `std::fs::rename` overwrites an existing file.
///
/// The temporary file is removed on every failure path, so a full disk
/// or a permission error leaves nothing behind but the original.
pub(crate) fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    // A sibling, hidden, and unique per process and per call: two relais
    // processes installing at once must not share a staging file.
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "relais-owned".to_string());
    let parent = path.parent().unwrap_or(Path::new("."));
    let temp = parent.join(format!(
        ".{file_name}.relais-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let staged = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(contents.as_bytes())?;
        file.flush()?;
        // Durability, not just visibility: without this the rename can be
        // ordered before the data on a crash, and the file comes back
        // empty rather than old.
        file.sync_all()?;
        Ok(())
    })();
    if let Err(e) = staged {
        // Best effort: the staging file is already unreachable by name
        // for anything but this call, and the error to report is the
        // write's, not the cleanup's.
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&temp, path) {
        // Same: the error to report is the rename's; a staging file
        // that cannot be removed is named by this call alone.
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }
    Ok(())
}

/// Text outside the block that is nothing but the frontmatter opener is
/// ours too — it is the one line the YAML marker cannot sit above.
fn is_only_opener(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.is_empty() || trimmed == "---"
}

/// A marked block found in a file: where it sits, the sha its marker
/// recorded when it was installed, and the content it carries now.
#[derive(Debug, Clone, PartialEq)]
struct OwnedBlock {
    /// Byte offset of `BEGIN_MARKER`.
    start: usize,
    /// Byte offset one past `END_MARKER`.
    end: usize,
    recorded_sha: String,
    content: String,
}

impl OwnedBlock {
    /// The user edited the block iff its content no longer hashes to what
    /// the marker recorded — a question about this file only, never about
    /// the template the current binary ships.
    fn is_unchanged(&self) -> bool {
        sha256_hex(self.content.as_bytes()) == self.recorded_sha
    }
}

/// What reading a file's markers established.
#[derive(Debug, Clone, PartialEq)]
enum BlockRead {
    /// No begin marker: the file is present but not ours.
    Absent,
    /// Markers are there but unusable — no end marker after the begin
    /// marker (a reordered or truncated pair), or no sha in the header.
    /// Reported; never parsed further, never rewritten.
    Malformed,
    Found(OwnedBlock),
}

/// Parse the owned block of an in-memory file. The end marker is searched
/// AFTER the begin marker: `find(END)` over the whole text can land before
/// `find(BEGIN)` on a reordered pair, and slicing that reversed range
/// panics the preview.
fn read_block(text: &str) -> BlockRead {
    // Either spelling of the begin marker: the HTML comment, or the YAML
    // comment that sits on the line after a frontmatter opener.
    let html = text.find(BEGIN_MARKER);
    let yaml = if text.starts_with(YAML_BEGIN_MARKER) {
        Some(0)
    } else {
        text.find(&format!("\n{YAML_BEGIN_MARKER}"))
            .map(|at| at + 1)
    };
    let (start, is_yaml) = match (html, yaml) {
        (Some(h), Some(y)) if y < h => (y, true),
        (Some(h), _) => (h, false),
        (None, Some(y)) => (y, true),
        (None, None) => return BlockRead::Absent,
    };
    let Some(end_offset) = text[start..].find(END_MARKER) else {
        return BlockRead::Malformed;
    };
    let end_start = start + end_offset;
    let end = end_start + END_MARKER.len();
    // The header is `<!-- relais:begin <sha> -->` or `# relais:begin
    // <sha>` up to the end of its line; either way it must close before
    // the end marker begins.
    let (header_end, sha_span_end, marker_len) = if is_yaml {
        let Some(line_end) = text[start..end_start].find('\n') else {
            return BlockRead::Malformed;
        };
        (start + line_end, start + line_end, YAML_BEGIN_MARKER.len())
    } else {
        let Some(header_offset) = text[start..end_start].find("-->") else {
            return BlockRead::Malformed;
        };
        (
            start + header_offset + "-->".len(),
            start + header_offset,
            BEGIN_MARKER.len(),
        )
    };
    let recorded_sha = text[start + marker_len..sha_span_end].trim().to_string();
    if recorded_sha.is_empty() {
        return BlockRead::Malformed;
    }
    let body = &text[header_end..end_start];
    let content = body
        .strip_prefix('\n')
        .unwrap_or(body)
        .trim_end_matches('\n')
        .to_string();
    BlockRead::Found(OwnedBlock {
        start,
        end,
        recorded_sha,
        content,
    })
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
        (
            Path::new("skills")
                .join("relais-verified-push")
                .join("SKILL.md"),
            skill_relais_verified_push(),
        ),
        (
            Path::new("skills")
                .join("relais-architecture-conflict")
                .join("SKILL.md"),
            skill_relais_architecture_conflict(),
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

## Inputs

Before a contract can be written, the caller must supply:

- the repository or task worktree the change belongs to (its root is
  where `relais.toml` is found);
- one precise objective sentence;
- the write scope: which paths the change may touch;
- acceptance criteria a command can verify — never "looks right" or a
  worker's own completion message;
- the verification profile to run, named in that repository's
  `relais.toml`.

Missing any of these is a reason to ask, not to guess one on the
caller's behalf.

## Steps

1. Work from the repository the task changes — its task worktree when
   one exists. Every command below runs with that directory as cwd
   (`cd <root> && …`); the session's own cwd may be anywhere else.
   `relais` finds `relais.toml` upward from cwd to the repository root,
   so a subdirectory is fine, but a directory outside the repository is
   refused, never guessed.

2. Write a contract at `<root>/.relais/task.json`:

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

3. Preflight without spending: `relais plan --task .relais/task.json`

4. Execute, naming this session so the coordinator's per-session limits
   and attribution are per TAB rather than per shell (SPEC §23):
   `RELAIS_SESSION_ID="${CLAUDE_SESSION_ID:-$$}" relais run --task .relais/task.json`

5. Read the outcome: accepted (receipt + patch), needs_decision,
needs_review, blocked, failed, budget_exhausted or interrupted. The
artifacts path is printed on every terminal state.

## Rules

- The contract is frozen once the run starts; changing objective, scope,
  acceptance or budget is a new run.
- Acceptance criteria must be executable by the verification profile; a
  worker's completion message is never acceptance.
- If the run needs a decision, make it explicitly — do not let a model
  invent it.

## Done when

- the contract was written to `<root>/.relais/task.json` and `relais
  plan` accepted it without complaint;
- the run reached a terminal state (accepted, needs_decision,
  needs_review, blocked, failed, budget_exhausted or interrupted) and
  that state, not a worker's own summary, was read;
- a `needs_decision` outcome was resolved explicitly, not guessed;
- for `accepted`, the printed artifacts path was checked, not assumed.
"#;
    body.trim_end().to_string()
}

fn skill_relais_verified_push() -> String {
    let body = r#"---
name: relais-verified-push
description: Verify a push actually left before trusting it — gate status, rehearsal, a bare push, then confirm the remote ref moved.
---

# /relais-verified-push

A push that "succeeded" is not the same claim as a push that left. Run
these four steps in order, from the repository the branch lives in.

## Steps

1. Check what pushing next gates: `amont list --json --stage pre-push
   --pushed`. The output is the `amont-list-v1` envelope: a `checks`
   array whose rows carry `id`, `effective_severity` and `status`. Read
   it before assuming the push is clean.

2. Rehearse the push gate on a snapshot of HEAD, ahead of time:
   `amont rehearse --wait`. This runs the same checks `git push` would
   run, with no remote connection open, so the push itself is fast and
   does not hold the remote idle while a suite runs.

3. Push, bare — never piped:

   ```sh
   git push
   ```

   Never pipe `git push` into `tail`, `head` or `grep`. A pipeline's exit
   status is the LAST command's, so the trimming command reports exit 0
   however the push actually ended, and the trimmed output discards the
   one line that would have said why it failed. If output must be
   trimmed, redirect to a file and read the file — never pipe a mutating
   command's own exit status away.

4. Confirm the ref actually moved, against the remote, not the local
   log: `git ls-remote origin refs/heads/<branch>`. Compare the sha it
   reports to `git log --oneline -1 <branch>`; only a match is a push
   that left.

## Rules

- Never trust a `git push` exit code alone; step 4 is the actual
  verification.
- A rehearsal failure is the push gate telling you now what `git push`
  would have refused later — fix it before step 3, not after.
"#;
    body.trim_end().to_string()
}

fn skill_relais_architecture_conflict() -> String {
    let body = r#"---
name: relais-architecture-conflict
description: Resolve an architecture decision key and handle every outcome aval can return, including a corpus that contradicts itself.
---

# /relais-architecture-conflict

`aval resolve <key> --json` has a stable exit-code contract; read the
code, not just stdout, before deciding what happened.

## Exit codes

| exit | meaning |
|---|---|
| 0 | active — a decision exists and holds |
| 4 | undecided — no record answers this key |
| 5 | contradiction — the corpus disagrees with itself |
| 6 | retired — a decision existed and was withdrawn |
| 7 | unknown — the key is not one aval recognises |
| 1 | tool failure |
| 2 | usage error |
| 3 | corpus unreadable |

## On exit 5 (contradiction)

Do not pick a side. aval is read-only and deciding between two
contradicting records is not this skill's call, or the caller's model's
call, to make silently. Instead:

1. Write an inspect contract (`"kind": "inspect"`) capturing the
   evidence: the key, the scope, and both records aval's contradiction
   names.
2. Hand back a decision that names BOTH readings the corpus supports,
   with their evidence, and says explicitly that the corpus contradicts
   itself here.
3. Let a person or an explicit decision step choose; never resolve the
   contradiction by preferring one record over the other on your own
   initiative.

## Rules

- Exit 1/2/3 are tool or corpus failures, not decisions — retry or
  report the failure, never treat them as "undecided".
- A `suggestion` field in aval's output is advisory: it resolves
  nothing and is never a substitute for calling `aval resolve` again.
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
    /// Marked, but the markers cannot be read (a reordered or truncated
    /// pair, a header without a sha, an unreadable file). Reported so a
    /// human can look; relais neither rewrites nor removes it.
    Malformed {
        relative: PathBuf,
    },
    Remove {
        relative: PathBuf,
    },
}

impl Action {
    /// The file the action is about, whatever the action is.
    pub fn relative(&self) -> &Path {
        match self {
            Action::Create { relative }
            | Action::Update { relative }
            | Action::SkipForeign { relative }
            | Action::Conflict { relative }
            | Action::Malformed { relative }
            | Action::Remove { relative } => relative,
        }
    }

    /// Whether `apply`/`apply_uninstall` is supposed to carry this action
    /// out. The reported-only actions (foreign, conflicted, malformed)
    /// are deliberate no-ops; an applicable action that did not happen is
    /// a failure the CLI exits non-zero on (C9).
    fn is_applicable(&self) -> bool {
        match self {
            Action::Create { .. } | Action::Update { .. } | Action::Remove { .. } => true,
            Action::SkipForeign { .. } | Action::Conflict { .. } | Action::Malformed { .. } => {
                false
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InstallPlan {
    pub actions: Vec<Action>,
}

/// What an apply pass did, and what it was supposed to do and did not.
/// The second list is the whole point: a file that changed between the
/// preview and `--write` is silently skipped by the writer, and a run
/// that reports "applied 0 change(s)" and exits 0 hides it (C9).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Applied {
    pub applied: Vec<Action>,
    pub not_applied: Vec<Action>,
}

/// Whether the file at an owned path is there, and in what condition.
/// A DANGLING symlink exists as a name and not as a file: `Path::exists`
/// follows the link and says "missing", and writing to that name writes
/// through the link into whatever it points at. Relais does not own the
/// far end of somebody's symlink, so it is foreign, not a `Create`.
enum Presence {
    Missing,
    Dangling,
    Present,
}

fn presence(path: &Path) -> Presence {
    match std::fs::symlink_metadata(path) {
        Err(_) => Presence::Missing,
        Ok(_) if path.exists() => Presence::Present,
        // The name resolves to nothing: a broken link, left alone.
        Ok(_) => Presence::Dangling,
    }
}

/// Which `.claude` directory an install acts on. User level is explicit,
/// never the default (SPEC §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    User,
    Project(PathBuf),
}

/// Preview-first: `--write` is the second, explicit step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Preview,
    Apply,
}

/// One `relais install --claude` / `relais uninstall --claude`
/// invocation, as a value the module can plan, apply and test without a
/// CLI around it. The home directory is a parameter of
/// [`InstallRequest::root`] rather than read here, so a test can drive
/// the user scope against a temporary directory without touching the
/// process environment.
#[derive(Debug, Clone, PartialEq)]
pub struct InstallRequest {
    pub scope: Scope,
    pub mode: Mode,
}

impl InstallRequest {
    /// The directory this request acts on, given the home directory the
    /// caller resolved.
    pub fn root(&self, home: &Path) -> InstallRoot {
        match &self.scope {
            Scope::User => InstallRoot {
                claude_dir: home.join(".claude"),
            },
            Scope::Project(dir) => InstallRoot::project(dir),
        }
    }

    /// How to name this scope in output.
    pub fn scope_label(&self) -> &'static str {
        match self.scope {
            Scope::User => "user level",
            Scope::Project(_) => "project level",
        }
    }
}

/// The plan, and what applying it did — nothing at all in preview mode.
#[derive(Debug, Clone, PartialEq)]
pub struct InstallReport {
    pub plan: InstallPlan,
    pub mode: Mode,
    pub applied: Vec<Action>,
    /// Applicable actions the apply pass did NOT carry out, because the
    /// file changed between the preview and the write.
    pub not_applied: Vec<Action>,
}

impl InstallReport {
    fn previewed(plan: InstallPlan) -> Self {
        Self {
            plan,
            mode: Mode::Preview,
            applied: Vec::new(),
            not_applied: Vec::new(),
        }
    }
}

/// Carry out an install request against the `.claude` directory it names.
pub fn install(request: &InstallRequest, home: &Path) -> std::io::Result<InstallReport> {
    let root = request.root(home);
    let plan = root.plan();
    match request.mode {
        Mode::Preview => Ok(InstallReport::previewed(plan)),
        Mode::Apply => {
            let done = root.apply(&plan)?;
            Ok(InstallReport {
                plan,
                mode: Mode::Apply,
                applied: done.applied,
                not_applied: done.not_applied,
            })
        }
    }
}

/// Carry out an uninstall request. Only owned, unchanged artifacts go.
pub fn uninstall(request: &InstallRequest, home: &Path) -> std::io::Result<InstallReport> {
    let root = request.root(home);
    let plan = root.uninstall_plan();
    match request.mode {
        Mode::Preview => Ok(InstallReport::previewed(plan)),
        Mode::Apply => {
            let done = root.apply_uninstall(&plan)?;
            Ok(InstallReport {
                plan,
                mode: Mode::Apply,
                applied: done.applied,
                not_applied: done.not_applied,
            })
        }
    }
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

    fn owned_path(&self, relative: &Path) -> PathBuf {
        self.claude_dir.join(relative)
    }

    /// What the file at `path` carries; an unreadable file is malformed,
    /// not silently foreign.
    fn block_of(path: &Path) -> BlockRead {
        match std::fs::read_to_string(path) {
            Ok(text) => read_block(&text),
            Err(_) => BlockRead::Malformed,
        }
    }

    /// Plan the install. Preview-first: nothing is written here.
    pub fn plan(&self) -> InstallPlan {
        let mut actions = Vec::new();
        for (relative, content) in owned_files() {
            let path = self.owned_path(&relative);
            match presence(&path) {
                Presence::Missing => {
                    actions.push(Action::Create { relative });
                    continue;
                }
                Presence::Dangling => {
                    actions.push(Action::SkipForeign { relative });
                    continue;
                }
                Presence::Present => {}
            }
            match Self::block_of(&path) {
                BlockRead::Absent => actions.push(Action::SkipForeign { relative }),
                BlockRead::Malformed => actions.push(Action::Malformed { relative }),
                BlockRead::Found(block) => {
                    if !block.is_unchanged() {
                        // Ours, but the user changed the block since: never
                        // clobbered, whatever the template says today.
                        actions.push(Action::Conflict { relative });
                    } else if block.recorded_sha != template_sha(&content) {
                        // Untouched since install, and the shipped
                        // template has moved on: this is the upgrade.
                        actions.push(Action::Update { relative });
                    }
                    // Untouched and current: nothing to do.
                }
            }
        }
        InstallPlan { actions }
    }

    /// Apply a plan: --write. Only Create and Update run; SkipForeign,
    /// Conflict and Malformed stay untouched and are reported. An
    /// applicable action the file's current state refuses is returned in
    /// `not_applied`, never swallowed.
    pub fn apply(&self, plan: &InstallPlan) -> std::io::Result<Applied> {
        let mut done = Applied {
            applied: Vec::new(),
            not_applied: Vec::new(),
        };
        for action in &plan.actions {
            let relative = match action {
                Action::Create { relative } | Action::Update { relative } => relative,
                Action::SkipForeign { .. }
                | Action::Conflict { .. }
                | Action::Malformed { .. }
                | Action::Remove { .. } => continue,
            };
            let Some(content) = owned_files()
                .into_iter()
                .find(|(candidate, _)| candidate == relative)
                .map(|(_, content)| content)
            else {
                // A plan naming a file this binary does not own is not
                // ours to write, whoever built it.
                done.not_applied.push(action.clone());
                continue;
            };
            let path = self.owned_path(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // Merge without replacing unrelated entries: an update
            // rewrites the marked block IN PLACE, so text the user keeps
            // around it survives exactly as it does on uninstall.
            match std::fs::read_to_string(&path) {
                Ok(text) => match read_block(&text) {
                    BlockRead::Found(block) if block.is_unchanged() => {
                        let before = &text[..block.start];
                        let after = &text[block.end..];
                        let updated = if is_only_opener(before) && after.trim().is_empty() {
                            // Wholly ours: lay the file out afresh. This is
                            // also how a file installed with the marker
                            // above its frontmatter migrates to the layout
                            // Claude Code can read.
                            owned_file(&content)
                        } else {
                            let (opener, _) = split_template(&content);
                            let mut updated = String::with_capacity(text.len() + content.len());
                            updated.push_str(before);
                            if !opener.is_empty() && !before.ends_with(opener) {
                                updated.push_str(opener);
                            }
                            updated.push_str(&owned_block_for(&content));
                            updated.push_str(after);
                            updated
                        };
                        write_atomic(&path, &updated)?;
                    }
                    // The file changed under us between plan and apply:
                    // a modified or malformed block is never overwritten,
                    // and the caller is told which.
                    BlockRead::Found(_) | BlockRead::Malformed | BlockRead::Absent => {
                        done.not_applied.push(action.clone());
                        continue;
                    }
                },
                // Not there (the Create case, or a file removed since the
                // preview): the whole file is ours to write.
                Err(_) => write_atomic(&path, &owned_file(&content))?,
            }
            done.applied.push(action.clone());
        }
        Ok(done)
    }

    /// Uninstall removes only owned, UNCHANGED artifacts. A user-modified
    /// owned file and every unrelated file stay exactly where they were.
    pub fn uninstall_plan(&self) -> InstallPlan {
        let mut actions = Vec::new();
        // "Unchanged" is the marker's own sha, not today's template: a
        // block installed by an older relais and never edited is still
        // ours to remove, so an upgrade never strands its own files.
        for (relative, _template) in owned_files() {
            let path = self.owned_path(&relative);
            match presence(&path) {
                Presence::Missing => continue,
                Presence::Dangling => {
                    actions.push(Action::SkipForeign { relative });
                    continue;
                }
                Presence::Present => {}
            }
            match Self::block_of(&path) {
                BlockRead::Absent => actions.push(Action::SkipForeign { relative }),
                BlockRead::Malformed => actions.push(Action::Malformed { relative }),
                BlockRead::Found(block) if block.is_unchanged() => {
                    actions.push(Action::Remove { relative })
                }
                BlockRead::Found(_) => actions.push(Action::Conflict { relative }),
            }
        }
        InstallPlan { actions }
    }

    /// Apply an uninstall plan. Every `Remove` is re-judged against the
    /// file as it is NOW: the preview and the write are two moments, and
    /// a block edited in between is the user's, not ours to delete.
    pub fn apply_uninstall(&self, plan: &InstallPlan) -> std::io::Result<Applied> {
        let mut done = Applied {
            applied: Vec::new(),
            not_applied: Vec::new(),
        };
        for action in &plan.actions {
            let Action::Remove { relative } = action else {
                continue;
            };
            let path = self.owned_path(relative);
            // If the user added content outside the markers, remove
            // only the owned block and keep their bytes; otherwise
            // the whole file was ours and goes away.
            match std::fs::read_to_string(&path) {
                Ok(text) => match read_block(&text) {
                    // Re-checked here, not trusted from the plan: the
                    // file may have been edited since the preview.
                    BlockRead::Found(block) if block.is_unchanged() => {
                        let mut remaining = String::with_capacity(text.len());
                        remaining.push_str(&text[..block.start]);
                        remaining.push_str(&text[block.end..]);
                        // The frontmatter opener above a YAML marker is ours
                        // as much as the block: a file holding nothing else
                        // goes away whole rather than leaving a `---` stub.
                        if !is_only_opener(&remaining) {
                            write_atomic(&path, &remaining)?;
                            done.applied.push(action.clone());
                            continue;
                        }
                    }
                    BlockRead::Found(_) | BlockRead::Malformed | BlockRead::Absent => {
                        done.not_applied.push(action.clone());
                        continue;
                    }
                },
                // Gone since the preview, or unreadable: nothing to
                // remove, and nothing to claim was removed.
                Err(_) => {
                    done.not_applied.push(action.clone());
                    continue;
                }
            }
            std::fs::remove_file(&path)?;
            // Clean directories we created, but never a directory we
            // did not own end-to-end.
            for ancestor in path.ancestors().skip(1) {
                if ancestor == self.claude_dir {
                    break;
                }
                match std::fs::read_dir(ancestor).map(|mut entries| entries.next().is_none()) {
                    // An empty directory relais made on the way in. A
                    // failed removal is not worth reporting: the file the
                    // user asked to remove is gone either way.
                    Ok(true) => {
                        let _ = std::fs::remove_dir(ancestor);
                    }
                    Ok(false) | Err(_) => break,
                }
            }
            done.applied.push(action.clone());
        }
        Ok(done)
    }
}

/// What one `--hooks` apply pass did.
#[derive(Debug, Clone, PartialEq)]
pub enum HooksApplied {
    /// Every target was already current; nothing was written.
    AlreadyCurrent,
    /// The named events were added, and the file was rewritten.
    Applied(Vec<&'static str>),
    /// The file could not be re-rendered byte for byte; nothing was
    /// written. Carries the message and the fragment to paste by hand.
    Refused { reason: String, paste_block: String },
}

/// What a caller who found nothing to paste puts in `paste_block`
/// instead: the field exists for an unrenderable FILE, where there is a
/// fragment to hand-merge; a duplicate handler in another file has no
/// such fragment — removing the other file's entry is the fix, not
/// pasting anything here.
const NO_PASTE_BLOCK: &str = "(nothing to paste here — remove the duplicate handler from the \
                               other settings file first)";

/// What one `--hooks` uninstall pass did.
#[derive(Debug, Clone, PartialEq)]
pub enum HooksRemoved {
    AlreadyAbsent,
    Removed(Vec<&'static str>),
    Refused { reason: String },
}

impl InstallRoot {
    fn settings_path(&self) -> PathBuf {
        self.claude_dir.join("settings.json")
    }

    /// The settings file's text, or `None` when there is no such file.
    ///
    /// Only `NotFound` is `None`. Every other error — no permission to
    /// read it, a directory in its place, an I/O failure mid-read — is
    /// returned, because "I could not see what is there" is not "there
    /// is nothing there", and the caller's answer to the second is to
    /// write a fresh seven-handler document. Collapsing the two would
    /// mean a settings.json relais could not read got replaced by one it
    /// composed, which is the opposite of this module's promise.
    fn read_settings(&self) -> std::io::Result<Option<String>> {
        match std::fs::read_to_string(self.settings_path()) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// A relais `PreToolUse` command already recorded in one of the OTHER
    /// settings files the Claude Code harness merges with this one — see
    /// [`settings::settings_candidates`], the single list of files it
    /// merges (issue #94). `home_dir`, when given, is the real user home:
    /// a project-scope install still needs it to see `~/.claude/settings.json`,
    /// even though the `home` an install request otherwise carries is the
    /// project root for that scope (`InstallRequest::root`'s doc comment).
    /// A private parameter rather than read from the environment here, so
    /// a test can drive this against a controlled directory; the two
    /// public entry points below resolve the real one themselves. `None`
    /// means writing here is free to go ahead as far as duplicates are
    /// concerned.
    fn duplicate_hook_reason(&self, roots: settings::MergedRoots<'_>) -> Option<String> {
        let target = self.settings_path();
        settings::settings_candidates(roots)
            .into_iter()
            .filter(|(_, path)| *path != target)
            .find_map(|(label, path)| {
                // NotFound is the ordinary case: that file records no
                // hook. Anything else is a file that MIGHT record one and
                // cannot be read, and treating it as "no hook" would let
                // an install write the second handler this check exists
                // to prevent (#94) — the same rule `read_settings` states
                // for the file being written.
                let text = match std::fs::read_to_string(&path) {
                    Ok(text) => text,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
                    Err(e) => {
                        return Some(format!(
                            "{} ({label}) cannot be read ({e}), so whether it already records a \
                             relais command is unknown — Claude Code merges settings files and \
                             would run both; read or remove that file before wiring another here",
                            path.display()
                        ))
                    }
                };
                settings::recorded_hook_command(&text)?;
                Some(format!(
                    "a relais command is already recorded on PreToolUse in {} ({label}) — \
                     Claude Code merges settings files and would run both on every spawn; \
                     remove that one before wiring another here",
                    path.display()
                ))
            })
    }

    /// Plan wiring the live hook into `settings.json`. Preview-first,
    /// like every other plan here: nothing is written. `queue_wait` is
    /// the admission wait currently configured — what the `PreToolUse`
    /// handler's recorded timeout must cover. A relais command already
    /// recorded in another settings file the harness merges reads as
    /// [`HooksPlan::Unrenderable`], the same "nothing will be written"
    /// shape a file that cannot be safely edited already gets — there is
    /// no fragment to paste for this one, only another file to clean up.
    pub fn plan_hooks(
        &self,
        relais_binary: &Path,
        queue_wait: std::time::Duration,
        roots: settings::MergedRoots<'_>,
    ) -> std::io::Result<HooksPlan> {
        if let Some(reason) = self.duplicate_hook_reason(roots) {
            return Ok(HooksPlan::Unrenderable {
                reason,
                paste_block: NO_PASTE_BLOCK.to_string(),
            });
        }
        Ok(settings::plan_hooks(
            self.read_settings()?.as_deref(),
            relais_binary,
            queue_wait,
        ))
    }

    /// Apply a hooks plan: `--write`. The file is re-read and
    /// re-checked for a round trip here, not trusted from the plan — the
    /// preview and the write are two moments, and a file edited or
    /// reformatted in between must be refused now, not silently
    /// rewritten on the strength of an earlier reading (C9). The
    /// duplicate check is re-run here too, for the same reason, and
    /// reads as [`HooksApplied::Refused`] — see [`Self::plan_hooks`].
    pub fn apply_hooks(
        &self,
        relais_binary: &Path,
        queue_wait: std::time::Duration,
        roots: settings::MergedRoots<'_>,
    ) -> std::io::Result<HooksApplied> {
        if let Some(reason) = self.duplicate_hook_reason(roots) {
            return Ok(HooksApplied::Refused {
                reason,
                paste_block: NO_PASTE_BLOCK.to_string(),
            });
        }
        let text = self.read_settings()?;
        match settings::plan_hooks(text.as_deref(), relais_binary, queue_wait) {
            HooksPlan::Unrenderable {
                reason,
                paste_block,
            } => Ok(HooksApplied::Refused {
                reason,
                paste_block,
            }),
            HooksPlan::Ready { events } => {
                if !events.iter().any(|e| e.action.changes_anything()) {
                    return Ok(HooksApplied::AlreadyCurrent);
                }
                let mut value: Value = match &text {
                    None => serde_json::json!({}),
                    Some(t) => serde_json::from_str(t)
                        .expect("plan_hooks already parsed this text without error"),
                };
                let changed = settings::apply_hooks(&mut value, relais_binary, queue_wait);
                let rendered = settings::render_like(text.as_deref(), &value);
                std::fs::create_dir_all(&self.claude_dir)?;
                write_atomic(&self.settings_path(), &rendered)?;
                Ok(HooksApplied::Applied(changed))
            }
        }
    }

    /// Plan removing relais's own hook commands from `settings.json`.
    pub fn plan_hooks_removal(
        &self,
        relais_binary: &Path,
    ) -> std::io::Result<settings::HooksRemovalPlan> {
        Ok(settings::plan_removal(
            self.read_settings()?.as_deref(),
            relais_binary,
        ))
    }

    /// Apply a hooks removal plan: `--write`. Re-checked at write time
    /// for the same reason [`Self::apply_hooks`] is.
    pub fn apply_hooks_removal(&self, relais_binary: &Path) -> std::io::Result<HooksRemoved> {
        let text = self.read_settings()?;
        match settings::plan_removal(text.as_deref(), relais_binary) {
            settings::HooksRemovalPlan::Unrenderable { reason } => {
                Ok(HooksRemoved::Refused { reason })
            }
            settings::HooksRemovalPlan::Ready { events } => {
                if !events
                    .iter()
                    .any(|e| e.action == settings::HookRemovalAction::WouldRemove)
                {
                    return Ok(HooksRemoved::AlreadyAbsent);
                }
                let mut value: Value = match &text {
                    None => serde_json::json!({}),
                    Some(t) => serde_json::from_str(t)
                        .expect("plan_removal already parsed this text without error"),
                };
                let changed = settings::remove_hooks(&mut value, relais_binary);
                let rendered = settings::render_like(text.as_deref(), &value);
                write_atomic(&self.settings_path(), &rendered)?;
                Ok(HooksRemoved::Removed(changed))
            }
        }
    }
}

impl InstallPlan {
    /// How many of the planned actions `apply` is meant to carry out.
    /// The rest are reported-only: foreign, conflicted or malformed.
    pub fn applicable_count(&self) -> usize {
        self.actions
            .iter()
            .filter(|action| action.is_applicable())
            .count()
    }

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
                Action::Malformed { relative } => out.push_str(&format!(
                    "  keep    {} (relais markers are unreadable — NOT touched; fix or remove the block by hand)\n",
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
    use std::time::Duration;

    fn temp_root() -> (InstallRoot, crate::test_support::TempDir) {
        let dir = crate::test_support::temp_dir("install");
        let root = InstallRoot {
            claude_dir: dir.join("project").join(".claude"),
        };
        (root, dir)
    }

    #[test]
    fn install_is_preview_first_and_merge_safe() {
        let (root, dir) = temp_root();
        let plan = root.plan();
        assert_eq!(plan.actions.len(), 6);
        assert!(plan
            .actions
            .iter()
            .all(|action| matches!(action, Action::Create { .. })));
        // Preview wrote nothing.
        assert!(!root.claude_dir.exists());
        let applied = root.apply(&plan).expect("apply");
        assert_eq!(applied.applied.len(), 6);
        assert!(applied.not_applied.is_empty(), "{applied:?}");
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
        assert!(
            applied.applied.is_empty(),
            "conflicts are never written: {applied:?}"
        );
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
        let applied = root.apply_uninstall(&plan).expect("uninstall").applied;
        // The three unchanged agents are removed entirely; the skill's
        // owned block is removed but the user's note survives; the
        // foreign agent was never ours.
        assert_eq!(applied.len(), 6, "{applied:?}");
        assert!(!root.claude_dir.join("agents/relais-research.md").exists());
        let remaining = std::fs::read_to_string(&owned).expect("kept file");
        assert!(
            remaining.contains("user note"),
            "user content survives uninstall: {remaining}"
        );
        assert!(
            !remaining.contains(BEGIN_MARKER) && !remaining.contains(YAML_BEGIN_MARKER),
            "the owned block is gone"
        );
        assert!(
            root.claude_dir.join("agents/custom.md").is_file(),
            "foreign file is kept"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// What an earlier relais would have installed: the same file, an
    /// older body, and the marker sha of THAT body.
    fn install_older_template(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, owned_file(body)).expect("write");
    }

    #[test]
    fn a_moved_template_upgrades_an_untouched_block() {
        let (root, dir) = temp_root();
        let agent = root.claude_dir.join("agents/relais-research.md");
        install_older_template(&agent, "# an older shipped body\n");

        let plan = root.plan();
        assert!(
            plan.actions.iter().any(|action| matches!(
                action,
                Action::Update { relative } if relative == Path::new("agents/relais-research.md")
            )),
            "an untouched block whose template moved on is an update: {:?}",
            plan.actions
        );
        let applied = root.apply(&plan).expect("apply").applied;
        assert!(applied
            .iter()
            .any(|action| matches!(action, Action::Update { .. })));
        let after = std::fs::read_to_string(&agent).expect("read");
        assert!(
            after.contains("read-only investigation worker") || after.contains("relais-research"),
            "the current template is installed: {after}"
        );
        assert!(
            after.contains(&template_sha(&agent_research())),
            "the marker records the sha of the content as installed"
        );
        // Applying twice is a no-op: the block is now current.
        assert!(
            root.plan().actions.is_empty(),
            "after the upgrade there is nothing left to do: {:?}",
            root.plan().actions
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_update_keeps_the_user_text_around_the_block() {
        let (root, dir) = temp_root();
        let agent = root.claude_dir.join("agents/relais-implementation.md");
        install_older_template(&agent, "# an older shipped body\n");
        let text = std::fs::read_to_string(&agent).expect("read");
        std::fs::write(
            &agent,
            format!("# my header\n\n{text}\n<!-- my note below the block -->\n"),
        )
        .expect("surround");

        let plan = root.plan();
        assert!(
            plan.actions.iter().any(|action| matches!(
                action,
                Action::Update { relative } if relative == Path::new("agents/relais-implementation.md")
            )),
            "text outside the markers is not a modification of our block: {:?}",
            plan.actions
        );
        root.apply(&plan).expect("apply");
        let after = std::fs::read_to_string(&agent).expect("read");
        assert!(
            after.starts_with("# my header"),
            "user header survives: {after}"
        );
        assert!(
            after.contains("<!-- my note below the block -->"),
            "user text below the block survives an update: {after}"
        );
        assert!(
            !after.contains("an older shipped body"),
            "the old block content is gone: {after}"
        );
        assert!(after.contains("implementation worker of the relais advisory set"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_hand_edited_block_is_kept_even_when_the_template_moved_on() {
        let (root, dir) = temp_root();
        let agent = root.claude_dir.join("agents/relais-review.md");
        install_older_template(&agent, "# an older shipped body\n");
        let text = std::fs::read_to_string(&agent).expect("read");
        // An edit INSIDE the block: the content no longer hashes to the
        // sha the marker recorded.
        std::fs::write(
            &agent,
            text.replace("an older shipped body", "MY body, thank you"),
        )
        .expect("edit inside");

        let plan = root.plan();
        assert!(
            plan.actions.iter().any(|action| matches!(
                action,
                Action::Conflict { relative } if relative == Path::new("agents/relais-review.md")
            )),
            "a hand-edited block is a conflict, never an update: {:?}",
            plan.actions
        );
        root.apply(&plan).expect("apply");
        let after = std::fs::read_to_string(&agent).expect("read");
        assert!(after.contains("MY body, thank you"), "the edit survives");
        // Uninstall will not take it either.
        let uninstall = root.uninstall_plan();
        assert!(
            uninstall.actions.iter().any(|action| matches!(
                action,
                Action::Conflict { relative } if relative == Path::new("agents/relais-review.md")
            )),
            "uninstall keeps a modified block: {:?}",
            uninstall.actions
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn uninstall_removes_an_older_but_unmodified_block() {
        let (root, dir) = temp_root();
        let skill = root.claude_dir.join("skills/relais/SKILL.md");
        install_older_template(&skill, "# an older shipped body\n");
        let plan = root.uninstall_plan();
        assert!(
            plan.actions.iter().any(|action| matches!(
                action,
                Action::Remove { relative } if relative == Path::new("skills/relais/SKILL.md")
            )),
            "an untouched block from an older relais is still ours: {:?}",
            plan.actions
        );
        root.apply_uninstall(&plan).expect("uninstall");
        assert!(!skill.exists(), "the file relais owned end to end is gone");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_reordered_marker_pair_is_reported_not_a_panic() {
        let (root, dir) = temp_root();
        let agent = root.claude_dir.join("agents/relais-research.md");
        std::fs::create_dir_all(agent.parent().expect("parent")).expect("mkdir");
        // The end marker BEFORE the begin marker: the old code sliced a
        // reversed byte range and panicked the preview.
        std::fs::write(
            &agent,
            format!("{END_MARKER}\nstuff\n{BEGIN_MARKER} deadbeef -->\n"),
        )
        .expect("write");

        let plan = root.plan();
        assert!(
            plan.actions.iter().any(|action| matches!(
                action,
                Action::Malformed { relative } if relative == Path::new("agents/relais-research.md")
            )),
            "a reordered pair is reported: {:?}",
            plan.actions
        );
        assert!(plan.render().contains("markers are unreadable"));
        let before = std::fs::read_to_string(&agent).expect("read");
        let applied = root.apply(&plan).expect("apply").applied;
        assert!(
            !applied
                .iter()
                .any(|action| matches!(action, Action::Malformed { .. } | Action::Update { .. })),
            "nothing malformed is rewritten: {applied:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&agent).expect("read"),
            before,
            "the malformed file is byte-identical after --write"
        );
        // Uninstall is equally hands-off.
        let uninstall = root.uninstall_plan();
        assert!(uninstall
            .actions
            .iter()
            .any(|action| matches!(action, Action::Malformed { .. })));
        root.apply_uninstall(&uninstall).expect("uninstall");
        assert!(agent.is_file(), "the file is left exactly where it was");
        assert_eq!(
            std::fs::read_to_string(&agent).expect("read"),
            before,
            "uninstall does not touch it either"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_header_without_a_sha_is_malformed() {
        let (root, dir) = temp_root();
        let agent = root.claude_dir.join("agents/relais-research.md");
        std::fs::create_dir_all(agent.parent().expect("parent")).expect("mkdir");
        std::fs::write(&agent, format!("{BEGIN_MARKER} -->\nbody\n{END_MARKER}\n")).expect("write");
        assert!(root
            .plan()
            .actions
            .iter()
            .any(|action| matches!(action, Action::Malformed { .. })));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn owned_content_carries_verifiable_markers() {
        for (relative, content) in owned_files() {
            let file = owned_file(&content);
            // Every shipped template carries frontmatter, and Claude Code
            // reads it only when `---` is the FIRST line: the marker sits
            // under it as a YAML comment, never above it.
            assert!(
                file.starts_with("---\n# relais:begin "),
                "{relative:?}: frontmatter first, marker under it:\n{file}"
            );
            let second_line_end = file[4..].find('\n').expect("marker line") + 4;
            let frontmatter_body = &file[second_line_end + 1..];
            assert!(
                frontmatter_body.starts_with("name: "),
                "{relative:?}: the frontmatter's own keys follow the marker"
            );
            assert!(file.contains(&template_sha(&content)));
            assert!(file.trim_end().ends_with(END_MARKER));
            // The block the reader sees is exactly what the writer hashed.
            let BlockRead::Found(block) = read_block(&file) else {
                panic!("{relative:?}: the marker must read back");
            };
            assert!(block.is_unchanged());
            assert_eq!(block.start, 4, "the block starts right after `---`");
        }
    }

    /// A body and its installed path can never disagree: the frontmatter
    /// `name:` a file carries must match the agent or skill directory it
    /// is installed at, so a mismatch cannot ship silently.
    #[test]
    fn every_owned_body_names_the_path_it_installs_at() {
        for (relative, content) in owned_files() {
            assert!(
                content.starts_with("---\n"),
                "{relative:?}: the body must open with frontmatter:\n{content}"
            );
            let name_line = content
                .lines()
                .find(|line| line.starts_with("name: "))
                .unwrap_or_else(|| panic!("{relative:?}: no `name:` line in frontmatter"));
            let name = name_line.trim_start_matches("name: ").trim();
            let expected = if relative.starts_with("agents") {
                relative
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .expect("agent file stem")
                    .to_string()
            } else {
                relative
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|s| s.to_str())
                    .expect("skill directory name")
                    .to_string()
            };
            assert_eq!(
                name, expected,
                "{relative:?}: frontmatter name must match its install path"
            );
        }
    }

    /// The relais-verified-push skill exists to stop exactly one failure
    /// mode: a piped `git push` reporting success when the push did not
    /// happen. It must name every step, and must not itself contain the
    /// pattern it teaches against.
    #[test]
    fn relais_verified_push_names_every_step_and_never_pipes_a_mutation() {
        let body = skill_relais_verified_push();
        for needle in [
            "amont list",
            "amont rehearse --wait",
            "git push",
            "git ls-remote",
        ] {
            assert!(
                body.contains(needle),
                "the skill must name `{needle}`:\n{body}"
            );
        }
        // No exemption, deliberately. An earlier version of this test
        // skipped any line containing `…` or "Do not", so that the prose
        // warning could quote the anti-pattern — and that exemption let
        // `git push origin … | tail -1` sit INSIDE the fenced step a
        // reader copies from and still pass (verified by injecting it).
        // The body is worded so no single line ever needs the exemption:
        // the warning names `git push` and the trimming commands in
        // separate code spans, so a line carrying both is always a
        // genuine invocation.
        for line in body.lines() {
            let has_mutation = line.contains("git push") || line.contains("git commit");
            let pipes_into_trim =
                line.contains("| tail") || line.contains("| head") || line.contains("| grep");
            assert!(
                !(has_mutation && pipes_into_trim),
                "a mutating command must never be piped into tail/head/grep: {line:?}"
            );
        }
    }

    #[test]
    fn a_marker_installed_above_the_frontmatter_migrates_under_it() {
        let (root, dir) = temp_root();
        // What relais 0.1.4 wrote: an HTML marker as line 1, the whole
        // template (opener included) inside the block.
        let skill = root.claude_dir.join("skills/relais/SKILL.md");
        std::fs::create_dir_all(skill.parent().unwrap()).expect("mkdir");
        let content = skill_relais();
        let old_layout = format!(
            "{BEGIN_MARKER} {} -->\n{}\n{END_MARKER}\n",
            sha256_hex(content.trim_end_matches('\n').as_bytes()),
            content.trim_end_matches('\n')
        );
        std::fs::write(&skill, &old_layout).expect("write");
        let plan = root.plan();
        assert!(
            plan.actions.iter().any(|action| matches!(
                action,
                Action::Update { relative } if relative.ends_with("SKILL.md")
            )),
            "an untouched old-layout file is an upgrade, not a conflict: {plan:?}"
        );
        root.apply(&plan).expect("apply");
        let migrated = std::fs::read_to_string(&skill).expect("read");
        assert!(
            migrated.starts_with("---\nname: relais\n")
                || migrated.starts_with("---\n# relais:begin ")
        );
        assert!(
            migrated.starts_with("---\n"),
            "Claude Code needs the opener first:\n{migrated}"
        );
        assert!(!migrated.contains(BEGIN_MARKER), "the HTML marker is gone");
        // Uninstall removes the whole file: the opener above the marker
        // is not user text.
        let uninstall = root.uninstall_plan();
        root.apply_uninstall(&uninstall).expect("uninstall");
        assert!(!skill.exists(), "no `---` stub is left behind");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A write that cannot land must leave the previous bytes where they
    /// were AND leave no staging file behind (C1). The failure is
    /// arranged rather than simulated: renaming a file over a
    /// non-empty DIRECTORY fails on every platform relais ships to.
    #[test]
    fn a_failed_write_leaves_the_old_file_and_no_temp_behind() {
        let (_root, dir) = temp_root();
        let target = dir.join("occupied");
        std::fs::create_dir_all(&target).expect("mkdir");
        std::fs::write(target.join("inside"), "the user's bytes").expect("write");

        let refused = write_atomic(&target, "replacement").expect_err("a directory is not a file");
        assert!(
            target.is_dir(),
            "the destination is untouched: {refused} ({refused:?})"
        );
        assert_eq!(
            std::fs::read_to_string(target.join("inside")).expect("read"),
            "the user's bytes"
        );
        let leftovers: Vec<String> = std::fs::read_dir(&dir)
            .expect("read_dir")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.contains(".relais-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "staging files left behind: {leftovers:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The happy path still goes through a temp file: after the write the
    /// directory holds the destination and nothing else.
    #[test]
    fn an_atomic_write_leaves_only_the_destination() {
        let (_root, dir) = temp_root();
        let target = dir.join("agent.md");
        write_atomic(&target, "first").expect("write");
        write_atomic(&target, "second").expect("rewrite");
        assert_eq!(std::fs::read_to_string(&target).expect("read"), "second");
        let names: Vec<String> = std::fs::read_dir(&dir)
            .expect("read_dir")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["agent.md".to_string()], "{names:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A name that resolves to nothing is somebody's broken symlink, not
    /// a missing file: writing it would write THROUGH the link.
    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_is_foreign_not_a_create() {
        let (root, dir) = temp_root();
        let agent = root.claude_dir.join("agents/relais-research.md");
        std::fs::create_dir_all(agent.parent().expect("parent")).expect("mkdir");
        let nowhere = dir.join("nowhere.md");
        std::os::unix::fs::symlink(&nowhere, &agent).expect("symlink");

        let plan = root.plan();
        assert!(
            plan.actions.iter().any(|action| matches!(
                action,
                Action::SkipForeign { relative } if relative == Path::new("agents/relais-research.md")
            )),
            "a dangling link is foreign: {:?}",
            plan.actions
        );
        root.apply(&plan).expect("apply");
        assert!(
            !nowhere.exists(),
            "nothing was written through the link to {}",
            nowhere.display()
        );
        // Uninstall leaves it alone too.
        let uninstall = root.uninstall_plan();
        assert!(uninstall
            .actions
            .iter()
            .any(|action| matches!(action, Action::SkipForeign { .. })));
        assert!(
            agent.symlink_metadata().is_ok(),
            "the link itself is still there"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The preview and the write are two moments. A file that became
    /// foreign in between is not written — and the caller is TOLD, so
    /// `--write` can exit non-zero instead of claiming success (C9).
    #[test]
    fn an_action_the_write_cannot_carry_out_is_reported() {
        let (root, dir) = temp_root();
        let plan = root.plan();
        assert_eq!(plan.applicable_count(), 6);
        // Between plan and apply, somebody else writes one of the files.
        let agent = root.claude_dir.join("agents/relais-review.md");
        std::fs::create_dir_all(agent.parent().expect("parent")).expect("mkdir");
        std::fs::write(&agent, "# mine now\n").expect("foreign");

        let done = root.apply(&plan).expect("apply");
        assert_eq!(done.applied.len(), 5, "{done:?}");
        assert_eq!(
            done.not_applied
                .iter()
                .map(|action| action.relative().to_path_buf())
                .collect::<Vec<_>>(),
            vec![PathBuf::from("agents/relais-review.md")],
        );
        assert_eq!(
            std::fs::read_to_string(&agent).expect("read"),
            "# mine now\n",
            "the foreign file is byte-identical"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Uninstall judges the block at REMOVAL time, not at preview time.
    #[test]
    fn uninstall_rechecks_the_block_before_removing_it() {
        let (root, dir) = temp_root();
        root.apply(&root.plan()).expect("apply");
        let plan = root.uninstall_plan();
        assert_eq!(plan.applicable_count(), 6);
        // The user edits inside the block after previewing the removal.
        let agent = root.claude_dir.join("agents/relais-research.md");
        let text = std::fs::read_to_string(&agent).expect("read");
        std::fs::write(&agent, text.replace("advisory set", "MY set")).expect("edit inside");

        let done = root.apply_uninstall(&plan).expect("uninstall");
        assert_eq!(done.applied.len(), 5, "{done:?}");
        assert_eq!(
            done.not_applied
                .iter()
                .map(|action| action.relative().to_path_buf())
                .collect::<Vec<_>>(),
            vec![PathBuf::from("agents/relais-research.md")],
        );
        assert!(
            std::fs::read_to_string(&agent)
                .expect("read")
                .contains("MY set"),
            "a block edited since the preview is kept"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `relais install --claude --user --write` against a home directory
    /// the caller injects: the request is a value, so the user scope is
    /// testable without touching the process environment.
    #[test]
    fn a_user_scope_request_writes_under_the_home_it_is_given() {
        let (_root, dir) = temp_root();
        let home = dir.join("home");
        std::fs::create_dir_all(&home).expect("mkdir");
        let request = InstallRequest {
            scope: Scope::User,
            mode: Mode::Preview,
        };
        assert_eq!(request.scope_label(), "user level");
        let preview = install(&request, &home).expect("preview");
        assert_eq!(preview.mode, Mode::Preview);
        assert_eq!(preview.plan.applicable_count(), 6);
        assert!(preview.applied.is_empty(), "a preview writes nothing");
        assert!(
            !home.join(".claude").exists(),
            "a preview creates no directory"
        );

        let request = InstallRequest {
            scope: Scope::User,
            mode: Mode::Apply,
        };
        let written = install(&request, &home).expect("apply");
        assert_eq!(written.applied.len(), 6, "{written:?}");
        assert!(written.not_applied.is_empty(), "{written:?}");
        assert!(home.join(".claude/skills/relais/SKILL.md").is_file());
        assert!(home.join(".claude/agents/relais-research.md").is_file());

        let removed = uninstall(
            &InstallRequest {
                scope: Scope::User,
                mode: Mode::Apply,
            },
            &home,
        )
        .expect("uninstall");
        assert_eq!(removed.applied.len(), 6, "{removed:?}");
        assert!(!home.join(".claude/agents/relais-research.md").exists());

        // A project request is the same shape with the directory named.
        let project = InstallRequest {
            scope: Scope::Project(dir.join("repo")),
            mode: Mode::Preview,
        };
        assert_eq!(project.scope_label(), "project level");
        assert_eq!(
            project.root(&home).claude_dir,
            dir.join("repo").join(".claude")
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Roots naming no other settings file, for the tests that are about
    /// what install writes rather than about the duplicate check. Written
    /// out rather than defaulted so a test that DOES care has to say so:
    /// the whole of issue #94 was a root being inferred instead of named.
    fn no_other_roots() -> settings::MergedRoots<'static> {
        settings::MergedRoots {
            project: None,
            home: None,
        }
    }

    fn relais_binary_for_test() -> PathBuf {
        PathBuf::from("/opt/relais/bin/relais")
    }

    /// `install --claude` alone (no `--hooks`) reads and writes no
    /// settings.json at all — wiring a hook into a file the user
    /// maintains is a separate, explicit ask. A settings.json already in
    /// place before the run is byte-identical after it.
    #[test]
    fn install_without_hooks_never_touches_settings_json() {
        let (root, dir) = temp_root();
        std::fs::create_dir_all(&root.claude_dir).expect("mkdir");
        let settings_path = root.claude_dir.join("settings.json");
        let original = "{\n  \"hooks\": {}\n}\n";
        std::fs::write(&settings_path, original).expect("write settings");

        root.apply(&root.plan()).expect("apply");

        assert_eq!(
            std::fs::read_to_string(&settings_path).expect("read"),
            original,
            "settings.json must be untouched by a plain --claude install"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn hooks_apply_wires_all_seven_targets_into_a_fresh_settings_file() {
        let (root, dir) = temp_root();
        let binary = relais_binary_for_test();
        let plan = root
            .plan_hooks(&binary, Duration::from_secs(2), no_other_roots())
            .expect("plan hooks");
        assert_eq!(plan.applicable_count(), 7);

        let applied = root
            .apply_hooks(&binary, Duration::from_secs(2), no_other_roots())
            .expect("apply hooks");
        let HooksApplied::Applied(events) = applied else {
            panic!("expected events to be wired: {applied:?}");
        };
        assert_eq!(events.len(), 7, "{events:?}");

        // A re-run is a no-op.
        let replan = root
            .plan_hooks(&binary, Duration::from_secs(2), no_other_roots())
            .expect("re-plan hooks");
        assert_eq!(replan.applicable_count(), 0, "{replan:?}");
        let reapplied = root
            .apply_hooks(&binary, Duration::from_secs(2), no_other_roots())
            .expect("apply hooks again");
        assert_eq!(reapplied, HooksApplied::AlreadyCurrent);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `duplicate_hook_reason` itself, driven with an explicit home
    /// directory rather than through `plan_hooks`/`apply_hooks` (which
    /// resolve the real one): the user's `~/.claude/settings.json` is one
    /// of the files the harness merges, and a relais command already
    /// there is a duplicate a project-scope install must see too.
    #[test]
    fn duplicate_hook_reason_sees_a_relais_command_in_the_users_settings_file() {
        let (root, dir) = temp_root();
        let home = dir.join("home");
        std::fs::create_dir_all(home.join(".claude")).expect("mkdir");
        std::fs::write(
            home.join(".claude").join("settings.json"),
            serde_json::json!({
                "hooks": {
                    "PreToolUse": [
                        {"matcher": "Agent|Task", "hooks": [
                            {"type": "command", "command": "/opt/relais/bin/relais hook"}
                        ]}
                    ]
                }
            })
            .to_string(),
        )
        .expect("write");

        assert!(root.duplicate_hook_reason(no_other_roots()).is_none());
        let reason = root
            .duplicate_hook_reason(settings::MergedRoots {
                project: None,
                home: Some(&home),
            })
            .expect("a duplicate in the user's settings.json");
        assert!(reason.contains("settings.json"), "{reason}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `settings.local.json` sits right next to `settings.json` and
    /// Claude Code merges the two (issue #94): a relais command already
    /// recorded there is a duplicate this install must refuse to add to,
    /// not a file it is blind to.
    #[test]
    fn hooks_apply_refuses_when_the_local_settings_file_already_has_a_relais_hook() {
        let (root, dir) = temp_root();
        std::fs::create_dir_all(&root.claude_dir).expect("mkdir");
        std::fs::write(
            root.claude_dir.join("settings.local.json"),
            serde_json::json!({
                "hooks": {
                    "PreToolUse": [
                        {"matcher": "Agent|Task", "hooks": [
                            {"type": "command", "command": "/opt/relais/bin/relais hook"}
                        ]}
                    ]
                }
            })
            .to_string(),
        )
        .expect("write");

        let binary = relais_binary_for_test();
        // This test is ABOUT the roots, so it names them: the project
        // whose `.claude` holds the local file. `no_other_roots()` would
        // tell the duplicate check to look nowhere and the refusal would
        // never fire — which is what a blanket sweep of these call sites
        // did, and what naming the helper that way is meant to reveal.
        let project = root
            .claude_dir
            .parent()
            .expect("the fixture's project root")
            .to_path_buf();
        let roots = settings::MergedRoots {
            project: Some(project.as_path()),
            home: None,
        };
        let planned = root
            .plan_hooks(&binary, Duration::from_secs(2), roots)
            .expect("plan hooks");
        let HooksPlan::Unrenderable { reason, .. } = planned else {
            panic!("expected a duplicate to refuse the plan: {planned:?}");
        };
        assert!(reason.contains("settings.local.json"), "{reason}");

        let applied = root
            .apply_hooks(&binary, Duration::from_secs(2), roots)
            .expect("apply hooks");
        let HooksApplied::Refused { reason, .. } = applied else {
            panic!("expected a duplicate to refuse the apply: {applied:?}");
        };
        assert!(reason.contains("settings.local.json"), "{reason}");
        assert!(
            !root.claude_dir.join("settings.json").exists(),
            "refused: nothing written"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A settings.json relais cannot READ is not a settings.json that is
    /// absent. The error reaches the caller and nothing is written: the
    /// alternative — treating "no permission" as "no file" — replaces a
    /// person's configuration with a document relais composed, having
    /// never seen what it destroyed.
    #[test]
    #[cfg(unix)]
    fn hooks_apply_refuses_a_settings_file_it_cannot_read() {
        use std::os::unix::fs::PermissionsExt;

        let (root, dir) = temp_root();
        std::fs::create_dir_all(&root.claude_dir).expect("mkdir");
        let settings_path = root.claude_dir.join("settings.json");
        let original = "{\n  \"hooks\": {}\n}\n";
        std::fs::write(&settings_path, original).expect("write");
        std::fs::set_permissions(&settings_path, std::fs::Permissions::from_mode(0o000))
            .expect("chmod");

        // Running as root defeats the premise: the file IS readable then,
        // and the scenario cannot be staged at all.
        if std::fs::read_to_string(&settings_path).is_ok() {
            std::fs::remove_dir_all(&dir).ok();
            return;
        }

        let binary = relais_binary_for_test();
        let planned = root.plan_hooks(&binary, Duration::from_secs(2), no_other_roots());
        assert!(planned.is_err(), "an unreadable file is not an absent one");
        let applied = root.apply_hooks(&binary, Duration::from_secs(2), no_other_roots());
        assert!(applied.is_err(), "{applied:?}");

        std::fs::set_permissions(&settings_path, std::fs::Permissions::from_mode(0o600))
            .expect("chmod back");
        assert_eq!(
            std::fs::read_to_string(&settings_path).expect("read"),
            original,
            "the file relais could not read is still exactly as it was"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn hooks_apply_is_refused_on_a_hand_formatted_settings_file() {
        let (root, dir) = temp_root();
        std::fs::create_dir_all(&root.claude_dir).expect("mkdir");
        std::fs::write(
            root.claude_dir.join("settings.json"),
            "{\n  \"hooks\":{}\n}\n",
        )
        .expect("write");
        let binary = relais_binary_for_test();
        let applied = root
            .apply_hooks(&binary, Duration::from_secs(2), no_other_roots())
            .expect("apply hooks");
        assert!(
            matches!(applied, HooksApplied::Refused { .. }),
            "{applied:?}"
        );
        let after = std::fs::read_to_string(root.claude_dir.join("settings.json")).expect("read");
        assert_eq!(after, "{\n  \"hooks\":{}\n}\n", "refused: nothing written");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn hooks_uninstall_removes_only_what_install_added() {
        let (root, dir) = temp_root();
        let binary = relais_binary_for_test();
        root.apply_hooks(&binary, Duration::from_secs(2), no_other_roots())
            .expect("apply hooks");

        // Add a foreign hook on the same event, same matcher, so the
        // removal has to leave it behind.
        let settings_path = root.claude_dir.join("settings.json");
        let mut value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings_path).expect("read"))
                .expect("parse");
        value["hooks"]["PreToolUse"][0]["hooks"]
            .as_array_mut()
            .expect("hooks array")
            .push(serde_json::json!({"type": "command", "command": "/usr/bin/someone-elses-tool"}));
        std::fs::write(&settings_path, settings::render_canonical(&value) + "\n")
            .expect("write foreign hook");

        let plan = root.plan_hooks_removal(&binary).expect("plan removal");
        assert_eq!(plan.applicable_count(), 7);
        let removed = root.apply_hooks_removal(&binary).expect("apply removal");
        let HooksRemoved::Removed(events) = removed else {
            panic!("expected removal: {removed:?}");
        };
        assert_eq!(events.len(), 7, "{events:?}");

        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings_path).expect("read"))
                .expect("parse");
        let pre_tool_use = after["hooks"]["PreToolUse"].as_array().expect("array");
        assert_eq!(
            pre_tool_use.len(),
            1,
            "the entry another author's handler lives on survives: {pre_tool_use:?}"
        );
        let remaining_hooks = pre_tool_use[0]["hooks"].as_array().expect("hooks");
        assert_eq!(remaining_hooks.len(), 1, "{remaining_hooks:?}");
        assert_eq!(
            remaining_hooks[0]["command"], "/usr/bin/someone-elses-tool",
            "another author's handler on the same event survives"
        );

        // A re-run has nothing left to remove.
        let replan = root.plan_hooks_removal(&binary).expect("re-plan removal");
        assert_eq!(replan.applicable_count(), 0, "{replan:?}");
        let reremoved = root
            .apply_hooks_removal(&binary)
            .expect("apply removal again");
        assert_eq!(reremoved, HooksRemoved::AlreadyAbsent);
        std::fs::remove_dir_all(&dir).ok();
    }
}
