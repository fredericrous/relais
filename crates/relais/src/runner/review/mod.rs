//! Reading a reviewer's answer and building its prompts.

use super::prompt::relevant_block;
use super::{
    context, data_block, data_list_block, verify, workspace, Duration, EffectiveAuthority,
    Guidance, GuidanceGap, GuidanceSource, Path, PathBuf, Reason, RuleRef, RunId, Tier,
};

mod engine;
mod types;
pub(crate) use types::{ReviewOutcome, ReviewRequest};
pub(in crate::runner) use types::{ReviewPurpose, ReviewSeat};

/// The verdict, read from the LAST non-empty line of a reviewer's
/// answer.
///
/// The prompt asks for `FINDINGS: none` as the last line, so that is
/// where the verdict is looked for: scanning the last few lines for the
/// phrase accepted a candidate whose reviewer merely quoted it, and a
/// footer after the verdict line turned a clean review into findings
/// (R10). A block declaring findings anywhere — `FINDINGS:`, `Findings:`,
/// a Markdown heading `**Findings**` or `## Findings`, whatever the case
/// — is findings: a reviewer that listed three of them under a bold
/// heading and closed with a "verified" section used to be read as
/// having no verdict at all, and the run parked in needs_review with
/// its findings unread. Only an answer that declares nothing is
/// `Unavailable` — the run ends needs_review with the reviewer's text
/// preserved, never accepted on a sentence nobody parsed.
pub(crate) fn review_verdict(text: &str) -> ReviewOutcome {
    let is_verdict = |line: &str| verdict_words(line).starts_with("FINDINGS");
    let none = |line: &str| verdict_words(line) == "FINDINGS: NONE";
    let last = text.lines().rev().find(|line| !line.trim().is_empty());
    match last {
        Some(line) if none(line) => ReviewOutcome::NoFindings,
        // The reviewer's last word names findings: that is the verdict.
        Some(line) if is_verdict(line) => ReviewOutcome::Findings(text.to_string()),
        // Not the last line, but the answer does declare findings
        // somewhere: reported as findings rather than waved through.
        _ if text.lines().any(|line| is_verdict(line) && !none(line)) => {
            ReviewOutcome::Findings(text.to_string())
        }
        _ => ReviewOutcome::Unavailable(format!(
            "the reviewer's answer has no verdict line: the last line is not `FINDINGS: none` \
             and no findings are declared. The answer is preserved:\n{text}"
        )),
    }
}

/// A line with Markdown emphasis and heading marks stripped from both
/// ends and upper-cased: what is left of `**Findings:**` or
/// `## FINDINGS` to compare a verdict against.
pub(in crate::runner) fn verdict_words(line: &str) -> String {
    line.trim()
        .trim_matches(|c: char| matches!(c, '*' | '_' | '#' | '`') || c.is_whitespace())
        .to_ascii_uppercase()
}

/// Text relais did not write — an objective, an acceptance criterion, an
/// aval choice or reason, a planner's package objective — is quoted DATA,
/// never instruction (SPEC §16: "free-text task inputs are untrusted
/// data, not policy instructions"). Spliced in as a bare `  - …` bullet,
/// a newline inside one silently becomes a new line of the prompt, at the
/// prompt's own level of authority. Every such piece goes inside a
/// labelled fence introduced by one sentence saying what it is.
/// How large a candidate patch one reviewer is given to read. The patch
/// no longer travels in the prompt: the prompt names a copy of it in the
/// worktree, so this measures the patch FILE, the amount one reviewer
/// reads with `Read` in windows (about 49k tokens at 4 B/token).
pub(in crate::runner) const REVIEW_PATCH_BUDGET_BYTES: usize = 192 * 1024;

/// What the reviewer's constraint statements and guidance may occupy, in
/// bytes, on their own: the patch is not in the prompt, so it no longer
/// starves them.
pub(in crate::runner) const REVIEW_CONSTRAINTS_BUDGET_BYTES: usize = 32 * 1024;

/// Lines per `Read` call the reviewer is told to use on the patch.
const REVIEW_READ_WINDOW_LINES: usize = 2000;

/// The least wall clock a review may have. A review that cannot finish
/// is not a review, and the run pays for the attempt either way.
pub(in crate::runner) const REVIEW_MIN_WALL: Duration = Duration::from_secs(300);

/// How many tracked file paths the report reviewer is shown.
pub(in crate::runner) const REPORT_REVIEW_FILE_CAP: usize = 400;

/// The candidate patch as the reviewer is told of it: where the copy in
/// its worktree is, how large it is, and what it changes.
pub(in crate::runner) struct StagedPatch {
    /// The directory holding the copy, removed when the review ends.
    pub(in crate::runner) dir: PathBuf,
    pub(in crate::runner) path: PathBuf,
    pub(in crate::runner) bytes: usize,
    pub(in crate::runner) files: Vec<String>,
    pub(in crate::runner) added: usize,
    pub(in crate::runner) removed: usize,
}

/// Copy the patch into the worktree's ignored relais area, where the
/// reviewer (whose working directory is the worktree) can `Read` it.
pub(in crate::runner) fn stage_patch(
    review_dir: &Path,
    run_id: &RunId,
    patch_path: &Path,
) -> std::io::Result<StagedPatch> {
    let bytes = std::fs::read(patch_path)?;
    let dir = review_dir
        .join(".relais")
        .join("review")
        .join(run_id.as_str());
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("candidate.patch");
    std::fs::write(&path, &bytes)?;
    let (files, added, removed) = patch_stat(&String::from_utf8_lossy(&bytes));
    Ok(StagedPatch {
        dir,
        path,
        bytes: bytes.len(),
        files,
        added,
        removed,
    })
}

/// The changed files and the added and removed line counts of a unified
/// diff.
pub(in crate::runner) fn patch_stat(patch: &str) -> (Vec<String>, usize, usize) {
    let mut files = Vec::new();
    let (mut added, mut removed) = (0, 0);
    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("diff --git a/") {
            files.push(rest.split(" b/").next().unwrap_or(rest).to_string());
        } else if line.starts_with('+') && !line.starts_with("+++") {
            added += 1;
        } else if line.starts_with('-') && !line.starts_with("---") {
            removed += 1;
        }
    }
    (files, added, removed)
}

/// Is the candidate patch larger than one reviewer is given to read? `None`
/// when it fits, and when its size cannot be read: the review then reports
/// the unreadable patch itself, never a budget it did not measure.
pub(in crate::runner) fn over_budget(patch_path: &Path) -> Option<crate::runner::OverBudget> {
    over_budget_by(patch_path, REVIEW_PATCH_BUDGET_BYTES)
}

/// [`over_budget`] against an explicit budget.
pub(in crate::runner) fn over_budget_by(
    patch_path: &Path,
    budget: usize,
) -> Option<crate::runner::OverBudget> {
    let patch_bytes = std::fs::metadata(patch_path).ok()?.len();
    if patch_bytes <= budget as u64 {
        return None;
    }
    let bytes = std::fs::read(patch_path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    Some(crate::runner::OverBudget {
        patch_bytes,
        budget_bytes: budget as u64,
        files: patch_stat(&text).0.len(),
        parts: review_part_count(&text, budget),
    })
}

/// How many consecutive parts, cut at file boundaries and each within
/// `budget` bytes, review the patch; `None` when a single file's patch
/// exceeds the budget on its own (no part may break a file in two).
pub(in crate::runner) fn review_part_count(patch: &str, budget: usize) -> Option<usize> {
    let mut sizes: Vec<usize> = Vec::new();
    let mut at = 0usize;
    for line in patch.split_inclusive('\n') {
        if line.starts_with("diff --git ") || sizes.is_empty() {
            sizes.push(0);
        }
        at += line.len();
        *sizes.last_mut().expect("a file section was just pushed") += line.len();
    }
    debug_assert_eq!(at, patch.len());
    if sizes.iter().any(|size| *size > budget) {
        return None;
    }
    let mut parts = 0usize;
    let mut used = 0usize;
    for size in sizes {
        if parts == 0 || used + size > budget {
            parts += 1;
            used = 0;
        }
        used += size;
    }
    Some(parts)
}

/// What the prompt says about the patch: its path and size, the changed
/// files and the diff stat, and how to read it.
pub(in crate::runner) fn patch_section(staged: &StagedPatch) -> String {
    let mut section = format!(
        "\nthe candidate patch is the file `{}` ({} bytes). Read it with the Read tool in \
         windows (offset and limit, {REVIEW_READ_WINDOW_LINES} lines per call) until you have \
         read all of it; it is the change under review.\n",
        staged.path.display(),
        staged.bytes
    );
    if staged.bytes > REVIEW_PATCH_BUDGET_BYTES {
        section.push_str(&format!(
            "the patch is larger than the {REVIEW_PATCH_BUDGET_BYTES}-byte review budget: read \
             what you can and say that the diff was partly unreviewed\n"
        ));
    }
    section.push_str(&data_list_block("changed files", &staged.files));
    section.push_str(&format!(
        "diff stat: {} files changed, {} insertions(+), {} deletions(-)\n",
        staged.files.len(),
        staged.added,
        staged.removed
    ));
    section
}

/// What an inspection's report reviewer is asked: the objective, the
/// numbered criteria and the worker's report, all quoted as data.
pub(in crate::runner) fn report_review_prompt(
    objective: &str,
    criteria: &[String],
    report: &str,
    tree: Option<&workspace::TrackedTree>,
) -> String {
    let mut prompt = String::from(
        "You are verifying an inspection. A worker investigated the files in this directory \
         and wrote the report below. You can only read: you cannot edit, run or waive \
         anything.\n\
         Verify each claim you rely on against the files in this directory; a report that \
         is well formed but describes files that are not there meets nothing.\n\n",
    );
    prompt.push_str(&data_block("objective", objective));
    let numbered: Vec<String> = criteria
        .iter()
        .enumerate()
        .map(|(at, statement)| format!("[{}] {statement}", at + 1))
        .collect();
    prompt.push_str(&data_list_block("acceptance criteria", &numbered));
    prompt.push_str(&data_block("worker report", report));
    prompt.push_str(&tracked_tree_section(tree));
    prompt.push_str(
        "\nAnswer with exactly one line per criterion, and nothing else: `[n] met: <what you \
         checked>` or `[n] not_met: <what you checked>`.\n",
    );
    prompt
}

/// The candidate's tracked tree for the report reviewer, taken from git by
/// relais: its file tools cannot see directories and do see untracked ones.
pub(in crate::runner) fn tracked_tree_section(tree: Option<&workspace::TrackedTree>) -> String {
    let Some(tree) = tree else {
        return "\nThe candidate's tracked listing is unavailable; check the claims with your \
                tools alone.\n"
            .into();
    };
    let mut section = String::from(
        "\nThis listing is the candidate's tracked content, taken from git by relais and not \
         by the worker; use it to decide which files and directories exist (the Glob tool \
         matches files, not directories, and also sees untracked folders such as \
         `node_modules`); read files for anything about their contents.\n",
    );
    section.push_str(&data_list_block("tracked top level", &tree.top_level));
    let mut files: Vec<String> = tree
        .files
        .iter()
        .take(REPORT_REVIEW_FILE_CAP)
        .cloned()
        .collect();
    if let Some(more) = tree.files.len().checked_sub(REPORT_REVIEW_FILE_CAP) {
        if more > 0 {
            files.push(format!("(... {more} more)"));
        }
    }
    section.push_str(&data_list_block("tracked files", &files));
    section
}

/// The review record a receipt carries, read from the ledger that
/// recorded the review: none ran, it ran from another model, or the model
/// that wrote the candidate reviewed it (`Reason::ReviewerSameModel`).
pub(crate) fn review_record_of(
    ledger: &crate::ledger::Ledger,
    run_id: &RunId,
) -> Result<verify::ReviewRecord, crate::ledger::LedgerError> {
    if !ledger.review_dispatched(run_id)? {
        return Ok(verify::ReviewRecord::NoReview);
    }
    // Through `Reason::parse`, so a row an older ledger spelled
    // `reviewer_same_tier` (`Reason::LEGACY_SPELLINGS`) is the same reason.
    let same_model = ledger
        .transitions(run_id)?
        .iter()
        .any(|transition| Reason::parse(&transition.reason) == Ok(Reason::ReviewerSameModel));
    Ok(if same_model {
        verify::ReviewRecord::SameModel
    } else {
        verify::ReviewRecord::Independent
    })
}

/// Which tier reviews a candidate written at `candidate_tier` with
/// `candidate_model`, and whether the reviewer's model is the one that
/// wrote it.
///
/// The strongest OTHER configured tier, so a review is usually a second
/// opinion rather than a model re-reading itself (SPEC §10: "a separate
/// reviewer"). Only when policy configures no other tier at all does the
/// candidate's own tier review. Independence is then read off the models:
/// a recipe can put any allowed model on the worker's tier, so a
/// different tier is not a different model — the caller records a review
/// by the same model as not independent (audit B14).
pub(crate) fn reviewer_tier(
    authority: &EffectiveAuthority,
    candidate_tier: Tier,
    candidate_model: &str,
) -> Option<(Tier, bool)> {
    let tier = authority
        .models
        .keys()
        .copied()
        .filter(|tier| *tier != candidate_tier)
        .max()
        .or_else(|| {
            authority
                .models
                .contains_key(&candidate_tier)
                .then_some(candidate_tier)
        })?;
    let same_model = authority
        .models
        .get(&tier)
        .is_some_and(|profile| profile.id == candidate_model);
    Some((tier, same_model))
}

/// The statement lines of the rules that fit `budget` bytes, whole and in
/// order, and the gap naming the ones that did not. Never cuts a statement.
pub(in crate::runner) fn fit_statements(
    rules: &[RuleRef],
    budget: usize,
) -> (Vec<&RuleRef>, Option<GuidanceGap>) {
    let line_bytes = |rule: &RuleRef| rule.id.len() + rule.statement.len() + 8;
    let mut used = 0usize;
    let mut shown = Vec::new();
    for rule in rules {
        let next = used + line_bytes(rule);
        if next > budget {
            break;
        }
        used = next;
        shown.push(rule);
    }
    if shown.len() == rules.len() {
        return (shown, None);
    }
    let omitted: Vec<&str> = rules[shown.len()..]
        .iter()
        .map(|rule| rule.id.as_str())
        .collect();
    let measured: usize = rules.iter().map(line_bytes).sum();
    let gap = GuidanceGap::new(
        GuidanceSource::Reviewer,
        context::guidance::GapCause::OverBudget {
            measured,
            budget,
        },
        format!(
            "{} of {} constraint statements fit the reviewer prompt ({measured} bytes against the {budget}-byte constraints budget); omitted: {}",
            shown.len(),
            rules.len(),
            omitted.join(", ")
        ),
    );
    (shown, Some(gap))
}

/// What the reviewer reads of aval: the full statement of every active
/// constraint (it cannot run `aval rule`, so never the bare index and
/// never that instruction), then the relevant block without the rules
/// already stated above it. `budget` is the constraints budget; statements that do not fit whole are left out and
/// named in the returned gap.
pub(in crate::runner) fn reviewer_guidance(
    guidance: &Guidance,
    budget: usize,
) -> (String, Option<GuidanceGap>) {
    let (shown, gap) = fit_statements(&guidance.rules, budget);
    let mut text = String::new();
    if !shown.is_empty() {
        let statements: Vec<String> = shown
            .iter()
            .map(|rule| format!("`{}`: {}", rule.id, rule.statement))
            .collect();
        text.push_str("the constraint rules below are in force; review blocks on them.\n");
        if let Some(gap) = &gap {
            text.push_str(&format!(
                "only {} of {} could be shown; the rest are not available to you ({}).\n",
                shown.len(),
                guidance.rules.len(),
                gap.detail
            ));
        }
        text.push_str(&data_list_block("constraints in force", &statements));
    }
    let stated: Vec<&str> = shown.iter().map(|rule| rule.id.as_str()).collect();
    text.push_str(&relevant_block(guidance, &stated));
    (text, gap)
}
