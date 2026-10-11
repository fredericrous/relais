//! The plan-review panel's reviewers, per class (plan-review-learns-model,
//! Behaviour 7): how many labelled reviews each class has, how they came
//! out, and what they cost in tokens. Every class is still learning: no
//! class has been decided yet.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::ledger::{Ledger, MergeTotals, ReviewClassTotals};

/// Classes shown; the rest count under `more`.
const TOP_CLASSES: usize = 5;
/// The widest line the section prints.
const WIDTH: usize = 80;
const STILL_LEARNING: &str = "still learning";
const EMPTY: &str = "no reviewer outcomes yet: relais learn cycle ingests them";

/// One class's row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReviewerClass {
    pub class: String,
    /// The labelled reviews: clean plus miss.
    pub n: u64,
    pub clean: u64,
    pub miss: u64,
    pub unknown: u64,
    pub tokens_by_model: BTreeMap<String, u64>,
}

/// The `Reviewers` section.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReviewerReport {
    pub classes: Vec<ReviewerClass>,
    /// Classes beyond the ones listed.
    pub more: usize,
    pub empty: bool,
    /// Why the ledger could not answer; the section says so instead of
    /// claiming there is nothing yet.
    pub unreadable: Option<String>,
    /// Router tasks a fallback subagent's merge row settled (plan-review-
    /// edits-apply, Behaviour 5). What `plan-apply` applied by command
    /// leaves no task and no ledger row here: it is counted in the ingest's
    /// own summary line.
    pub merges_by_subagent: u64,
    pub merges_verified: u64,
}

/// The section from the ledger. Reading it never fails a report: a ledger
/// that cannot answer is a one-line section naming why.
pub fn reviewer_report(ledger: &Ledger) -> ReviewerReport {
    match ledger
        .review_class_totals()
        .and_then(|totals| Ok((totals, ledger.merge_outcome_totals()?)))
    {
        Ok((totals, merges)) => build(totals, merges),
        Err(e) => ReviewerReport {
            classes: Vec::new(),
            more: 0,
            empty: false,
            unreadable: Some(e.to_string()),
            merges_by_subagent: 0,
            merges_verified: 0,
        },
    }
}

fn build(totals: Vec<ReviewClassTotals>, merges: MergeTotals) -> ReviewerReport {
    let mut classes: Vec<ReviewerClass> = totals
        .into_iter()
        .map(|t| ReviewerClass {
            n: t.clean + t.miss,
            class: t.class,
            clean: t.clean,
            miss: t.miss,
            unknown: t.unknown,
            tokens_by_model: t.tokens_by_model,
        })
        .collect();
    classes.sort_by(|a, b| (b.n, b.unknown, &a.class).cmp(&(a.n, a.unknown, &b.class)));
    let more = classes.len().saturating_sub(TOP_CLASSES);
    classes.truncate(TOP_CLASSES);
    ReviewerReport {
        empty: classes.is_empty() && merges.by_subagent == 0,
        classes,
        more,
        unreadable: None,
        merges_by_subagent: merges.by_subagent,
        merges_verified: merges.verified,
    }
}

/// `120000` as `120k`; small counts as they are.
fn thousands(tokens: u64) -> String {
    if tokens >= 1000 {
        format!("{}k", (tokens + 500) / 1000)
    } else {
        tokens.to_string()
    }
}

/// `fable-5-1 120k, sonnet-5-5 30k`: the busiest model first.
/// Tokens per model, largest first, as many whole entries as fit in `room`
/// characters; the rest are counted (`+2`), never cut mid-name.
fn tokens_text(by_model: &BTreeMap<String, u64>, room: usize) -> String {
    let mut models: Vec<(&str, u64)> = by_model
        .iter()
        .map(|(model, tokens)| (model.strip_prefix("claude-").unwrap_or(model), *tokens))
        .collect();
    models.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let entries: Vec<String> = models
        .iter()
        .map(|(model, tokens)| format!("{model} {}", thousands(*tokens)))
        .collect();
    for shown in (0..=entries.len()).rev() {
        let mut text = entries[..shown].join(", ");
        let rest = entries.len() - shown;
        if rest > 0 {
            text = if shown == 0 {
                format!("+{rest}")
            } else {
                format!("{text}, +{rest}")
            };
        }
        if text.chars().count() <= room {
            return text;
        }
    }
    clip(&format!("+{}", entries.len()), room)
}

/// Clip to `max` characters, never bytes, ending in `...` when it clipped:
/// ASCII, so a byte-counting `awk 'length>80'` sees the same width.
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max.saturating_sub(3)).collect();
    format!("{head}...")
}

impl ReviewerReport {
    /// The text section, every line within 80 columns.
    pub fn render(&self) -> String {
        if let Some(why) = &self.unreadable {
            return format!(
                "\n{}\n",
                clip(&format!("reviewer outcomes unreadable: {why}"), WIDTH)
            );
        }
        if self.empty {
            return format!("\n{EMPTY}\n");
        }
        if self.classes.is_empty() {
            return format!(
                "\nReviewers: {EMPTY}\n  merge: {} by subagent ({} verified)\n",
                self.merges_by_subagent, self.merges_verified
            );
        }
        // Every class is still learning until Phase 8 decides one, so the
        // header says it once and the row keeps its width for the tokens.
        let mut out = format!(
            "\nReviewers: every class {STILL_LEARNING}; n = clean + miss, unknown not in n\n"
        );
        out.push_str("  class            n clean miss unknown tokens by model\n");
        for class in &self.classes {
            let head = format!(
                "  {:<14} {:>3} {:>5} {:>4} {:>7} ",
                clip(&class.class, 14),
                class.n,
                class.clean,
                class.miss,
                class.unknown
            );
            let room = WIDTH.saturating_sub(head.chars().count());
            let tokens = tokens_text(&class.tokens_by_model, room);
            out.push_str(&format!("{head}{tokens}\n"));
        }
        if self.more > 0 {
            out.push_str(&format!("  +{} more\n", self.more));
        }
        if self.merges_by_subagent > 0 {
            out.push_str(&format!(
                "  merge: {} by subagent ({} verified)\n",
                self.merges_by_subagent, self.merges_verified
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn totals(
        class: &str,
        clean: u64,
        miss: u64,
        unknown: u64,
        tokens: &[(&str, u64)],
    ) -> ReviewClassTotals {
        ReviewClassTotals {
            class: class.into(),
            clean,
            miss,
            unknown,
            tokens_by_model: tokens.iter().map(|(m, t)| (m.to_string(), *t)).collect(),
        }
    }

    #[test]
    fn nothing_ingested_is_the_empty_sentence() {
        let report = build(Vec::new(), MergeTotals::default());
        assert!(report.empty && report.classes.is_empty() && report.more == 0);
        assert_eq!(
            report.render(),
            "\nno reviewer outcomes yet: relais learn cycle ingests them\n"
        );
    }

    #[test]
    fn rows_name_their_columns_and_say_still_learning() {
        let report = build(
            vec![totals(
                "backend",
                9,
                3,
                2,
                &[("claude-fable-5-1", 120_000), ("claude-sonnet-5-5", 30_000)],
            )],
            MergeTotals::default(),
        );
        let text = report.render();
        assert!(text.contains("tokens by model"), "{text}");
        let row = text.lines().find(|l| l.contains("backend")).expect("row");
        assert!(row.contains("fable-5-1 120k, sonnet-5-5 30k"), "{row}");
        assert!(text.contains("every class still learning"), "{text}");
        assert_eq!(report.classes[0].n, 12);
    }

    #[test]
    fn only_five_classes_are_listed_and_the_rest_are_counted() {
        let all: Vec<_> = (0..8u64)
            .map(|i| totals(&format!("role{i}"), i, 0, 0, &[]))
            .collect();
        let report = build(all, MergeTotals::default());
        assert_eq!(report.classes.len(), 5);
        assert_eq!(report.classes[0].class, "role7");
        assert_eq!(report.more, 3);
        assert!(report.render().contains("+3 more"));
    }

    #[test]
    fn no_line_passes_80_columns_however_long_the_names_and_counts() {
        let long = "é".repeat(60);
        let tokens: Vec<(String, u64)> = (0..6)
            .map(|i| (format!("claude-{long}{i}"), 9_999_999_999))
            .collect();
        let pairs: Vec<(&str, u64)> = tokens.iter().map(|(m, t)| (m.as_str(), *t)).collect();
        let report = build(
            (0..7)
                .map(|i| totals(&format!("{long}{i}"), 1_000_000, 2, 3, &pairs))
                .collect(),
            MergeTotals {
                by_subagent: 9_999_999,
                verified: 9_999_999,
            },
        );
        for line in report.render().lines() {
            assert!(
                line.chars().count() <= 80,
                "{} chars: {line}",
                line.chars().count()
            );
        }
    }

    #[test]
    fn models_that_do_not_fit_are_counted_not_cut() {
        let by: BTreeMap<String, u64> = [
            ("claude-opus-5-5", 56_000),
            ("claude-sonnet-5-5", 55_000),
            ("claude-fable-5-1", 36_000),
        ]
        .iter()
        .map(|(m, t)| (m.to_string(), *t))
        .collect();
        assert_eq!(tokens_text(&by, 40), "opus-5-5 56k, sonnet-5-5 55k, +1");
        assert_eq!(tokens_text(&by, 100).matches(", ").count(), 2);
        assert_eq!(tokens_text(&by, 3), "+3");
    }

    #[test]
    fn merges_by_subagent_print_their_line_with_or_without_classes() {
        let merges = MergeTotals {
            by_subagent: 3,
            verified: 2,
        };
        let alone = build(Vec::new(), merges);
        assert!(!alone.empty);
        assert!(
            alone.render().contains("merge: 3 by subagent (2 verified)"),
            "{}",
            alone.render()
        );
        let with = build(vec![totals("tui", 1, 0, 0, &[])], merges);
        let text = with.render();
        assert!(text.contains("  tui "), "{text}");
        assert!(
            text.ends_with("merge: 3 by subagent (2 verified)\n"),
            "{text}"
        );
        let none = build(vec![totals("tui", 1, 0, 0, &[])], MergeTotals::default());
        assert!(!none.render().contains("merge:"));
    }

    #[test]
    fn a_ledger_that_cannot_answer_says_so_not_that_nothing_is_there() {
        let report = ReviewerReport {
            classes: Vec::new(),
            more: 0,
            empty: false,
            unreadable: Some("no such table: review_outcomes".into()),
            merges_by_subagent: 0,
            merges_verified: 0,
        };
        let text = report.render();
        assert!(
            text.contains("reviewer outcomes unreadable: no such table"),
            "{text}"
        );
        assert!(!text.contains(EMPTY), "{text}");
    }
}
