//! Reading a reviewer's answer and building its prompts.

use super::prompt::{fit_statements, relevant_block, RenderedRule, RuleKind};
use super::{
    context, data_block, data_list_block, verify, workspace, Duration, EffectiveAuthority,
    Guidance, GuidanceGap, GuidanceSource, Path, PathBuf, Reason, RunId, Tier,
};

pub use super::machine::Severity;
pub(in crate::runner) use super::machine::REVIEW_MAX_PARTS;

mod candidates;
mod engine;
mod parts;
pub(in crate::runner) use candidates::CandidatesRun;
pub(in crate::runner) use parts::write_immutable;
mod types;
pub(crate) use types::{ReviewOutcome, ReviewRequest};
pub(in crate::runner) use types::{ReviewPurpose, ReviewSeat, UnavailableCause};

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

fn parse_severity(word: &str) -> Option<Severity> {
    match word.trim().to_ascii_lowercase().as_str() {
        "blocking" => Some(Severity::Blocking),
        "high" => Some(Severity::High),
        "medium" => Some(Severity::Medium),
        "low" => Some(Severity::Low),
        _ => None,
    }
}

/// The severity of each finding in a reviewer's answer: a finding is a
/// numbered line (`1.` or `1)`) under the verdict line that starts with
/// `[blocking|high|medium|low]`. `None` when any finding lacks a severity
/// or when no finding is tagged.
pub fn finding_severities(text: &str) -> Option<Vec<Severity>> {
    let mut lines = text.lines().skip_while(|line| {
        let words = verdict_words(line);
        !(words.starts_with("FINDINGS") && words != "FINDINGS: NONE")
    });
    lines.next()?;
    let severities = lines
        .filter_map(|line| {
            let line = line.trim_start();
            let digits = line.chars().take_while(char::is_ascii_digit).count();
            let rest = line[digits..].strip_prefix(['.', ')'])?;
            (digits > 0).then(|| {
                let rest = rest.trim_start().strip_prefix('[')?;
                parse_severity(rest.split_once(']')?.0)
            })
        })
        .collect::<Option<Vec<_>>>()?;
    (!severities.is_empty()).then_some(severities)
}

/// The instruction every findings-reporting reviewer prompt carries.
pub(in crate::runner) fn report_severity_instruction() -> &'static str {
    "Start each finding with its severity in brackets, `[blocking|high|medium|low]`, as a \
     numbered line, for example `1. [high] path:line — defect. Fix.`\n"
}

/// A line with Markdown emphasis and heading marks stripped from both
/// ends and upper-cased: what is left of `**Findings:**` or
/// `## FINDINGS` to compare a verdict against.
pub(in crate::runner) fn verdict_words(line: &str) -> String {
    line.trim()
        .trim_matches(|c: char| matches!(c, '*' | '_' | '#' | '`') || c.is_whitespace())
        .to_ascii_uppercase()
}

/// How large a candidate patch one reviewer is given to read. The patch
/// no longer travels in the prompt: the prompt names a copy of it in the
/// worktree, so this measures the patch FILE, the amount one reviewer
/// reads with `Read` in windows (about 49k tokens at 4 B/token).
pub(in crate::runner) const REVIEW_PATCH_BUDGET_BYTES: usize = 192 * 1024;

/// What the reviewer's constraint statements and guidance may occupy, in
/// bytes, on their own: the patch is not in the prompt, so it no longer
/// starves them.
pub(in crate::runner) const REVIEW_CONSTRAINTS_BUDGET_BYTES: usize = 96 * 1024;

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
    if let Err(e) = std::fs::write(&path, &bytes) {
        // Nothing is staged, so nothing would remove the directory later.
        return Err(match std::fs::remove_dir_all(&dir) {
            Ok(()) => e,
            Err(cleanup) => std::io::Error::new(
                e.kind(),
                format!(
                    "{e}; the directory {} could not be removed either: {cleanup}",
                    dir.display()
                ),
            ),
        });
    }
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

/// How long ONE part's reviewer may run. A review in parts is sequential,
/// so its worst case is [`REVIEW_MAX_PARTS`] times this (SPEC §10).
pub(in crate::runner) const REVIEW_PART_TIMEOUT: Duration = Duration::from_secs(20 * 60);

/// Why a patch cannot be reviewed in parts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PartsRefusal {
    /// One file's patch alone exceeds the budget; no part may break a file
    /// in two.
    FileOverBudget {
        file: String,
        bytes: usize,
        budget: usize,
    },
    /// Cutting at file boundaries takes more than [`REVIEW_MAX_PARTS`].
    TooManyParts { needed: usize },
}

impl std::fmt::Display for PartsRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FileOverBudget {
                file,
                bytes,
                budget,
            } => write!(
                f,
                "the patch of `{file}` alone is {bytes} bytes, over the {budget}-byte review budget, \
                 and a part never breaks a file in two, so the candidate cannot be reviewed in parts"
            ),
            Self::TooManyParts { needed } => write!(
                f,
                "a review in parts would take {needed} parts, more than the {REVIEW_MAX_PARTS} \
                 a review may take, so the candidate cannot be reviewed in parts"
            ),
        }
    }
}

/// The patch cut at file boundaries into consecutive parts that each fit
/// `budget` bytes, in order and whole: concatenated they are the patch. A
/// patch within the budget is one part.
pub(crate) fn split_into_parts(patch: &[u8], budget: usize) -> Result<Vec<Vec<u8>>, PartsRefusal> {
    let parts = cut_into_parts(patch, budget)?;
    if parts.len() > REVIEW_MAX_PARTS {
        return Err(PartsRefusal::TooManyParts {
            needed: parts.len(),
        });
    }
    Ok(parts)
}

/// [`split_into_parts`] without the limit on how many parts a review may
/// take: the cut itself, which only a file over the budget prevents.
fn cut_into_parts(patch: &[u8], budget: usize) -> Result<Vec<Vec<u8>>, PartsRefusal> {
    // One section per file; the bytes before the first `diff --git` line
    // belong to the first section.
    let mut sections: Vec<&[u8]> = Vec::new();
    let mut start = 0usize;
    let mut at = 0usize;
    for line in patch.split_inclusive(|byte| *byte == b'\n') {
        if line.starts_with(b"diff --git ") && at > start {
            sections.push(&patch[start..at]);
            start = at;
        }
        at += line.len();
    }
    if at > start || sections.is_empty() {
        sections.push(&patch[start..at]);
    }
    if let Some(section) = sections.iter().find(|section| section.len() > budget) {
        let head = String::from_utf8_lossy(section);
        let file = head
            .lines()
            .next()
            .and_then(|line| line.strip_prefix("diff --git a/"))
            .map(|rest| rest.split(" b/").next().unwrap_or(rest).to_string())
            .unwrap_or_else(|| "the patch".to_string());
        return Err(PartsRefusal::FileOverBudget {
            file,
            bytes: section.len(),
            budget,
        });
    }
    let mut parts: Vec<Vec<u8>> = Vec::new();
    for section in sections {
        match parts.last_mut() {
            Some(last) if last.len() + section.len() <= budget => last.extend_from_slice(section),
            _ => parts.push(section.to_vec()),
        }
    }
    Ok(parts)
}

/// How many parts a cut at file boundaries takes, however many; `None` when
/// a file over the budget prevents the cut. A review may take at most
/// [`REVIEW_MAX_PARTS`], which [`split_into_parts`] enforces.
pub(in crate::runner) fn review_part_count(patch: &str, budget: usize) -> Option<usize> {
    cut_into_parts(patch.as_bytes(), budget)
        .ok()
        .map(|parts| parts.len())
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
    let parts = ledger.review_parts(run_id)?;
    if parts.is_empty() && !ledger.review_dispatched(run_id)? {
        return Ok(verify::ReviewRecord::NoReview);
    }
    // Through `Reason::parse`, so a row an older ledger spelled
    // `reviewer_same_tier` (`Reason::LEGACY_SPELLINGS`) is the same reason.
    let same_model = ledger
        .transitions(run_id)?
        .iter()
        .any(|transition| Reason::parse(&transition.reason) == Ok(Reason::ReviewerSameModel));
    if !parts.is_empty() {
        return Ok(verify::ReviewRecord::Parts {
            same_model,
            parts: parts
                .into_iter()
                .map(|part| verify::ReviewPartRecord {
                    index: part.index,
                    of: part.count,
                    patch_sha256: part.patch_sha256,
                    result_sha256: part.result_sha256,
                    verdict: part.status.as_str().to_string(),
                })
                .collect(),
        });
    }
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

/// The reviewer's note on the paths a successor's scope was widened to
/// (relais's own test widen, or a person's `--widen-scope`): edits the
/// original contract did not cover, to check closely. Empty when nothing was
/// widened. The paths come out of a diff, so they are quoted as data.
pub(in crate::runner) fn widened_scope_block(paths: &[String]) -> String {
    if paths.is_empty() {
        return String::new();
    }
    let mut block = String::from(
        "this candidate was verified with its scope WIDENED: the paths below are OUT-OF-SCOPE \
         edits the original contract did not cover. Check them closely: an edit nobody asked \
         for is a finding, and in a test a deleted, skipped or loosened case is too.\n",
    );
    block.push_str(&super::data_list_block("widened paths", paths));
    block
}

/// What the reviewer reads of aval: the full text of every active code
/// rule, then the statement of every other active constraint (it cannot
/// run `aval rule`, so never the bare index and never that instruction),
/// then the relevant block without the rules already stated above it.
/// `budget` is the constraints budget; rules that do not fit whole are left
/// out and named in the returned gap.
pub(in crate::runner) fn reviewer_guidance(
    guidance: &Guidance,
    budget: usize,
) -> (String, Option<GuidanceGap>) {
    let code: Vec<RenderedRule> = guidance
        .code_rules
        .iter()
        .filter_map(RenderedRule::full_text_of)
        .collect();
    let others: Vec<RenderedRule> = guidance
        .rules
        .iter()
        .filter(|rule| !code.iter().any(|full| full.id == rule.id))
        .map(RenderedRule::statement_of)
        .collect();
    let unreadable: Vec<&str> = guidance
        .code_rules
        .iter()
        .filter(|rule| RenderedRule::full_text_of(rule).is_none())
        .map(|rule| rule.id.as_str())
        .collect();
    let unreadable_gap = (!unreadable.is_empty()).then(|| {
        GuidanceGap::new(
            GuidanceSource::Reviewer,
            context::guidance::GapCause::TextUnavailable,
            format!(
                "the full text of these code rules is unavailable to the reviewer: {}",
                unreadable.join(", ")
            ),
        )
    });
    let (code_shown, code_gap) = fit_statements(
        &code,
        budget,
        0,
        GuidanceSource::Reviewer,
        RuleKind::CodeRules,
    );
    let code_bytes: usize = code_shown.iter().map(|rule| rule.bytes()).sum();
    let (shown, other_gap) = fit_statements(
        &others,
        budget,
        code_bytes,
        GuidanceSource::Reviewer,
        RuleKind::Statements,
    );
    let gap = merge_gaps([unreadable_gap, code_gap, other_gap]);
    let mut text = String::new();
    if !code_shown.is_empty() || !shown.is_empty() {
        text.push_str("the constraint rules below are in force; review blocks on them.\n");
    }
    let total = code.len() + others.len();
    let shown_count = code_shown.len() + shown.len();
    if let Some(gap) = gap.as_ref().filter(|_| shown_count < total) {
        text.push_str(&format!(
            "only {shown_count} of {total} could be shown; the rest are not available to you ({}).\n",
            gap.detail
        ));
    }
    if !code_shown.is_empty() {
        text.push_str(
            "the code rules below are given in full; a violation of one that this diff \
             introduces is a finding.\n",
        );
        let full: Vec<&str> = code_shown.iter().map(|rule| rule.text.as_str()).collect();
        text.push_str(&data_block("code rules in force", &full.join("\n\n")));
    }
    if !shown.is_empty() {
        let statements: Vec<String> = shown.iter().map(|rule| rule.text.clone()).collect();
        text.push_str(&data_list_block("constraints in force", &statements));
    }
    let stated: Vec<&str> = code_shown
        .iter()
        .chain(shown.iter())
        .map(|rule| rule.id.as_str())
        .collect();
    text.push_str(&relevant_block(guidance, &stated));
    (text, gap)
}

/// The decisions that apply to the files a candidate patch changes, as a
/// fenced block, within `budget` bytes less the `reserved` already used (whole entries, dropped from the end
/// with a gap). No file means no call and no gap; an aval failure is a gap
/// and an empty block, never a failed review.
pub(in crate::runner) fn reviewer_decisions(
    resolver: &dyn context::DecisionResolver,
    changed_files: &[String],
    budget: usize,
    reserved: usize,
) -> (String, Option<GuidanceGap>) {
    if changed_files.is_empty() {
        return (String::new(), None);
    }
    // The choice aval ranks beside each key is the answer at the repository's
    // declared `in_scopes` (aval 1.11.0, ADR-0046); the row names the scope.
    let ranked = match resolver.decisions_for(changed_files) {
        Ok(ranked) => ranked,
        Err(gap) => return (decisions_gap_line(&gap), Some(gap)),
    };
    let entries: Vec<RenderedRule> = ranked
        .iter()
        .filter(|key| key.state == context::guidance::RankState::Active)
        .map(|key| {
            let choice = key.choice.as_deref().unwrap_or("(choice unavailable)");
            let record = key.adr.as_deref().unwrap_or("no record");
            let origin = match key.scope.as_deref() {
                Some(scope) => format!("{record}, {scope}"),
                None => record.to_string(),
            };
            RenderedRule {
                id: key.key.clone(),
                text: format!("`{}` ({origin}): {choice}", key.key),
            }
        })
        .collect();
    let (shown, gap) = fit_statements(
        &entries,
        budget,
        reserved,
        GuidanceSource::Decisions,
        RuleKind::Decisions,
    );
    let gap_line = gap.as_ref().map(decisions_gap_line).unwrap_or_default();
    if shown.is_empty() {
        return (gap_line, gap);
    }
    let lines: Vec<String> = shown.iter().map(|entry| entry.text.clone()).collect();
    let text = format!(
        "the decisions below apply to the files this diff changes.\n{}{gap_line}",
        data_list_block("decisions that apply", &lines)
    );
    (text, gap)
}

/// The prompt line that tells the reviewer the decisions it was given are
/// missing or partial, so it reports that as uncertainty.
fn decisions_gap_line(gap: &GuidanceGap) -> String {
    format!(
        "the decisions that apply to this diff are missing or partial ({}); \
         report that as uncertainty, not as approval.\n",
        gap.detail
    )
}

/// The reviewer's constraints, decisions and candidates gaps as one gap that
/// keeps every label: the first gap present leads, each later one rides in
/// the detail under its own description, and the cause follows `merge_gaps`.
pub(in crate::runner) fn merge_reviewer_gaps(
    constraints_gap: Option<GuidanceGap>,
    decisions_gap: Option<GuidanceGap>,
    candidates_gap: Option<GuidanceGap>,
) -> Option<GuidanceGap> {
    let mut present = [constraints_gap, decisions_gap, candidates_gap]
        .into_iter()
        .flatten();
    let leading = present.next()?;
    let labelled = present.map(|gap| GuidanceGap {
        detail: gap.describe(),
        ..gap
    });
    merge_gaps(std::iter::once(leading).chain(labelled).map(Some))
}

/// One gap for several: the details joined, and a cause that covers every
/// figure (budget gaps add their measured bytes; a budget cause wins over
/// an unavailable-text cause, and the detail still names both).
pub(in crate::runner) fn merge_gaps(
    gaps: impl IntoIterator<Item = Option<GuidanceGap>>,
) -> Option<GuidanceGap> {
    let mut present = gaps.into_iter().flatten();
    let mut merged = present.next()?;
    for next in present {
        merged.cause = match (&merged.cause, &next.cause) {
            (
                context::guidance::GapCause::OverBudget { measured, budget },
                context::guidance::GapCause::OverBudget { measured: more, .. },
            ) => context::guidance::GapCause::OverBudget {
                measured: measured + more,
                budget: *budget,
            },
            (_, over @ context::guidance::GapCause::OverBudget { .. }) => over.clone(),
            _ => merged.cause.clone(),
        };
        merged.detail = format!("{}; {}", merged.detail, next.detail);
    }
    Some(merged)
}

#[cfg(test)]
mod severity_tests {
    use super::*;

    #[test]
    fn tagged_findings_give_their_severities() {
        let text = "Verdict: x\nFINDINGS:\n1. [low] a.rs:1 — d\n2. [medium] b.rs — e\n";
        assert_eq!(
            finding_severities(text),
            Some(vec![Severity::Low, Severity::Medium])
        );
    }

    #[test]
    fn prose_untagged_mixed_and_none_give_none() {
        assert_eq!(
            finding_severities("FINDINGS:\n- src/main.rs: stale\n"),
            None
        );
        assert_eq!(finding_severities("FINDINGS:\n1. [high] a\n2. b\n"), None);
        assert_eq!(finding_severities("FINDINGS: none"), None);
    }

    #[test]
    fn the_reviewer_prompt_asks_for_a_severity_tag() {
        assert!(report_severity_instruction().contains("[blocking|high|medium|low]"));
    }
}
