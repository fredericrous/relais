//! `relais router export-classifier`: the kept classifier calls that
//! parsed, as the training set for a local model (relais ADR-0002). Each
//! call is its text (from the day files) joined to its answer (from the
//! ledger) by session and call id, split by time, behind a header line that
//! carries the capability table and its version. Pure: the command reads
//! the ledger and the day files and writes the result.

use std::collections::HashMap;

use serde::Serialize;

use super::capture::DayLine;
use super::table::CapabilityTable;
use crate::ledger::RouterClassificationRow;

/// The share of the newest calls held out as `test`, and the next share as
/// `calibration`, in percent. The rest is `train`.
const TEST_PERCENT: usize = 20;
const CALIBRATION_PERCENT: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Split {
    Train,
    Calibration,
    Test,
}

/// The split of the call at `index` of `total`, oldest first: the newest
/// 20% test, the 10% before them calibration, the rest train.
pub fn split_of(index: usize, total: usize) -> Split {
    let test = total * TEST_PERCENT / 100;
    let calibration = total * CALIBRATION_PERCENT / 100;
    if index >= total - test {
        Split::Test
    } else if index >= total - test - calibration {
        Split::Calibration
    } else {
        Split::Train
    }
}

/// What the export wrote and what it left out, for stderr.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Counts {
    pub written: usize,
    pub train: usize,
    pub calibration: usize,
    pub test: usize,
    /// Kept calls whose reply did not parse.
    pub unparsed: usize,
    /// Parsed calls before `--since`.
    pub before_since: usize,
    /// Parsed calls whose text is gone (past the 90 days, or never kept).
    pub without_text: usize,
}

impl Counts {
    pub fn summary(&self) -> String {
        format!(
            "relais router export-classifier: {} call(s) written (train {}, calibration {}, test {}); \
             left out: {} unparsed, {} before --since, {} without their text",
            self.written,
            self.train,
            self.calibration,
            self.test,
            self.unparsed,
            self.before_since,
            self.without_text
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Export {
    /// The header line, then one line per call.
    pub lines: Vec<String>,
    pub counts: Counts,
}

impl Export {
    /// The file's text: every line, newline-terminated.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for line in &self.lines {
            out.push_str(line);
            out.push('\n');
        }
        out
    }
}

/// Join and split. `rows` are the parsed calls in the window (any order),
/// `lines` every day line on disk; `all_kept` and `all_parsed` are the
/// ledger's totals, so the unparsed and out-of-window ones are counted.
pub fn build(
    rows: Vec<(String, RouterClassificationRow)>,
    lines: &[DayLine],
    table: &CapabilityTable,
    all_kept: u64,
    all_parsed: u64,
) -> Export {
    // A call sent twice is on disk twice; the last line wins.
    let mut text: HashMap<(&str, &str), &DayLine> = HashMap::new();
    for line in lines {
        text.insert((line.session.as_str(), line.call_id.as_str()), line);
    }
    let mut joined: Vec<(String, String, serde_json::Value)> = Vec::new();
    let mut counts = Counts {
        unparsed: all_kept.saturating_sub(all_parsed) as usize,
        before_since: (all_parsed as usize).saturating_sub(rows.len()),
        ..Counts::default()
    };
    for (session, row) in rows {
        let Some(line) = text.get(&(session.as_str(), row.call_id.as_str())) else {
            counts.without_text += 1;
            continue;
        };
        let mut answer: serde_json::Value = row
            .answer_json
            .as_deref()
            .and_then(|json| serde_json::from_str(json).ok())
            .unwrap_or(serde_json::Value::Null);
        if let Some(explicit) = answer.get_mut("explicit").and_then(|e| e.as_object_mut()) {
            explicit.insert(
                "quote".into(),
                line.quote
                    .clone()
                    .map_or(serde_json::Value::Null, Into::into),
            );
        }
        joined.push((
            row.at.clone(),
            format!("{session}\u{0}{}", row.call_id),
            serde_json::json!({
                "call_id": row.call_id,
                "at": row.at,
                "prompt_version": row.prompt_version,
                "input": line.input,
                "answer": answer,
            }),
        ));
    }
    joined.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    let total = joined.len();
    let mut out = Vec::with_capacity(total + 1);
    out.push(
        serde_json::json!({
            "kind": "header",
            "schema": 1,
            "capability_table_version": table.version,
            "capability_table": table,
            "calls": total,
        })
        .to_string(),
    );
    for (index, (_, _, mut record)) in joined.into_iter().enumerate() {
        let split = split_of(index, total);
        match split {
            Split::Train => counts.train += 1,
            Split::Calibration => counts.calibration += 1,
            Split::Test => counts.test += 1,
        }
        record["split"] = serde_json::to_value(split).expect("a split serializes");
        out.push(record.to_string());
    }
    counts.written = total;
    Export { lines: out, counts }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router::table::default_table;

    fn row(call: &str, at: &str) -> (String, RouterClassificationRow) {
        (
            "s".into(),
            RouterClassificationRow {
                call_id: call.into(),
                purpose: "prompt".into(),
                model: "m".into(),
                prompt_version: "a".repeat(64),
                input_sha256: "b".repeat(64),
                answer_json: Some(
                    r#"{"relation":"new_task","explicit":{"value":"correct"},"confidence":0.9}"#
                        .into(),
                ),
                parse_ok: true,
                latency_ms: Some(1),
                at: at.into(),
            },
        )
    }

    fn line(call: &str, input: &str) -> DayLine {
        DayLine {
            session: "s".into(),
            call_id: call.into(),
            at: "2026-10-11T00:00:00Z".into(),
            input: input.into(),
            quote: Some("that is wrong".into()),
            redactions: 0,
        }
    }

    #[test]
    fn the_newest_fifth_is_test_the_tenth_before_it_calibration() {
        let splits: Vec<Split> = (0..10).map(|i| split_of(i, 10)).collect();
        assert_eq!(splits.iter().filter(|s| **s == Split::Train).count(), 7);
        assert_eq!(
            &splits[7..],
            &[Split::Calibration, Split::Test, Split::Test]
        );
        assert_eq!(split_of(0, 1), Split::Train);
    }

    #[test]
    fn the_export_joins_text_and_answer_splits_by_time_and_counts_what_is_left_out() {
        // Out of order on purpose: the split follows `at`, not the input.
        let mut rows: Vec<_> = (0..10)
            .rev()
            .map(|i| row(&format!("c{i}"), &format!("2026-10-{:02}T10:00:00Z", i + 1)))
            .collect();
        rows.push(row("gone", "2026-10-20T10:00:00Z"));
        let lines: Vec<DayLine> = (0..10)
            .map(|i| line(&format!("c{i}"), &format!("text {i}")))
            .collect();
        let export = build(rows, &lines, &default_table(), 13, 11);
        assert_eq!(export.lines.len(), 11);
        let header: serde_json::Value = serde_json::from_str(&export.lines[0]).unwrap();
        assert_eq!(header["capability_table_version"], 1);
        assert_eq!(
            header["capability_table"]["rules"]
                .as_array()
                .unwrap()
                .len(),
            4
        );
        let first: serde_json::Value = serde_json::from_str(&export.lines[1]).unwrap();
        assert_eq!(first["call_id"], "c0");
        assert_eq!(first["input"], "text 0");
        assert_eq!(first["split"], "train");
        assert_eq!(first["answer"]["explicit"]["quote"], "that is wrong");
        let last: serde_json::Value = serde_json::from_str(&export.lines[10]).unwrap();
        assert_eq!(
            (last["call_id"].as_str(), last["split"].as_str()),
            (Some("c9"), Some("test"))
        );
        assert_eq!(
            (
                export.counts.written,
                export.counts.train,
                export.counts.calibration,
                export.counts.test
            ),
            (10, 7, 1, 2)
        );
        assert_eq!(export.counts.without_text, 1);
        assert_eq!(export.counts.unparsed, 2);
        assert_eq!(export.counts.before_since, 0);
        assert!(export.counts.summary().contains("1 without their text"));
        assert!(export.text().ends_with('\n'));
    }
}
