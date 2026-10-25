//! The candidate lines in the reviewer's prompt, the tally of what the
//! reviewer made of them, and the rulings file (SPEC §7).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;

use serde::Serialize;

use crate::context::candidates::{Candidate, CandidateList, Ruling, Verdict};
use crate::context::GuidanceGap;
use crate::ids::sha256_hex;
use crate::runner::{data_list_block, RunEngine};
use crate::verify::CandidatesRecord;

/// The format token of a line of `rulings.jsonl`.
const RULING_FORMAT: &str = "adr-lint.ruling/1";

/// The file in the run's artifact directory the rulings are appended to.
const RULINGS_FILE: &str = "rulings.jsonl";

/// The candidates one review dispatch is asked about: those on files of the
/// patch it reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::runner) struct PartCandidates {
    pub(in crate::runner) items: Vec<Candidate>,
    pub(in crate::runner) overflow: BTreeMap<String, u64>,
}

impl PartCandidates {
    /// The candidates of `list` whose path is among `files`.
    pub(in crate::runner) fn of(list: &CandidateList, files: &[String]) -> Self {
        Self {
            items: list
                .candidates
                .iter()
                .filter(|candidate| files.contains(&candidate.path))
                .cloned()
                .collect(),
            overflow: list.overflow.clone(),
        }
    }
}

/// What the run knows of the candidates of one candidate commit: what
/// adr-lint said, which candidates were put to a reviewer, and the rulings.
pub(in crate::runner) struct CandidatesRun {
    sha: String,
    fetched: Result<CandidateList, GuidanceGap>,
    listed: BTreeSet<String>,
    ruled: BTreeMap<String, Verdict>,
}

impl CandidatesRun {
    pub(in crate::runner) fn new(sha: &str, fetched: Result<CandidateList, GuidanceGap>) -> Self {
        Self {
            sha: sha.to_string(),
            fetched,
            listed: BTreeSet::new(),
            ruled: BTreeMap::new(),
        }
    }

    pub(in crate::runner) fn is_for(&self, sha: &str) -> bool {
        self.sha == sha
    }

    pub(in crate::runner) fn fetched(&self) -> &Result<CandidateList, GuidanceGap> {
        &self.fetched
    }

    pub(in crate::runner) fn list(&mut self, items: &[Candidate]) {
        self.listed.extend(items.iter().map(|item| item.id.clone()));
    }

    pub(in crate::runner) fn rule(&mut self, rulings: &BTreeMap<String, Ruling>) {
        for ruling in rulings.values() {
            self.ruled.insert(ruling.id.clone(), ruling.verdict);
        }
    }

    pub(in crate::runner) fn record(&self) -> CandidatesRecord {
        let list = match &self.fetched {
            Ok(list) => list,
            Err(gap) => {
                return CandidatesRecord::Unavailable {
                    detail: gap.detail.clone(),
                }
            }
        };
        let count = |wanted: Verdict| {
            self.ruled
                .iter()
                .filter(|(id, verdict)| self.listed.contains(*id) && **verdict == wanted)
                .count() as u64
        };
        let (violation, fine) = (count(Verdict::Violation), count(Verdict::Fine));
        let listed = self.listed.len() as u64;
        CandidatesRecord::Counted {
            listed,
            violation,
            fine,
            undisposed: listed - violation - fine,
            overflow: list.overflow.values().sum(),
        }
    }
}

impl RunEngine<'_> {
    /// What the receipt says of the candidates of the commit `sha`;
    /// nothing is recorded when no review asked for them.
    pub(in crate::runner) fn candidates_record(&self, sha: &str) -> CandidatesRecord {
        self.candidates_run
            .as_ref()
            .filter(|run| run.is_for(sha))
            .map_or(CandidatesRecord::NotRecorded, CandidatesRun::record)
    }
}

/// The instruction, the fenced list of ids and the details of the
/// candidates a reviewer is asked to rule on. Nothing when there is none.
pub(in crate::runner) fn candidates_block(part: &PartCandidates) -> String {
    if part.items.is_empty() {
        return String::new();
    }
    let entries: Vec<String> = part
        .items
        .iter()
        .map(|c| format!("{} {} {} {}:{}", c.id, c.rule, c.pattern_id, c.path, c.line))
        .collect();
    let digest = sha256_hex(entries.join("\n").as_bytes());
    let mut block = String::from(
        "adr-lint lists the candidates below: a candidate is a line to look at, not a finding. \
         Your answer must contain, BEFORE the final FINDINGS line, a `Candidates:` block with \
         one line per listed id: `<id> violation: <finding>` or `<id> fine: <one clause quoted \
         from the rule body>`. A violation is also listed under FINDINGS.\n",
    );
    block.push_str(&format!(
        "<<<CANDIDATES sha={digest} n={}>>>\n{}\n<<<END CANDIDATES>>>\n",
        entries.len(),
        entries.join("\n")
    ));
    let details: Vec<String> = part
        .items
        .iter()
        .map(|c| format!("{}: {}\n{}", c.id, one_line(&c.pattern), one_line(&c.text)))
        .collect();
    block.push_str(&data_list_block("candidate details", &details));
    if !part.overflow.is_empty() {
        let overflow: Vec<String> = part
            .overflow
            .iter()
            .map(|(rule, count)| format!("{rule}: {count} more not listed"))
            .collect();
        block.push_str(&data_list_block("candidates over the cap", &overflow));
    }
    block
}

fn one_line(text: &str) -> String {
    text.lines().collect::<Vec<_>>().join(" ")
}

/// The prompt line for candidates that could not be had.
pub(in crate::runner) fn candidates_unavailable_line(gap: &GuidanceGap) -> String {
    format!(
        "adr-lint's candidates are unavailable ({}): no ruling is needed\n",
        gap.detail
    )
}

#[derive(Serialize)]
struct RulingLine<'a> {
    format: &'static str,
    review: &'a str,
    tool: &'static str,
    repo: &'a str,
    tree: &'a str,
    candidate: &'a str,
    rule: &'a str,
    pattern_id: &'a str,
    pattern: &'a str,
    ruling: &'static str,
    clause: &'a str,
    at: &'a str,
}

/// What a ruling line is about: the reviewer dispatch and the patch it read.
pub(in crate::runner) struct RulingContext<'a> {
    pub(in crate::runner) review: &'a str,
    pub(in crate::runner) repo: &'a str,
    pub(in crate::runner) patch_bytes: &'a [u8],
    pub(in crate::runner) at: &'a str,
}

/// Append one `adr-lint.ruling/1` line to `rulings.jsonl` in `artifacts` for
/// each listed candidate the reviewer ruled on.
pub(in crate::runner) fn append_rulings(
    artifacts: &Path,
    context: &RulingContext<'_>,
    items: &[Candidate],
    rulings: &BTreeMap<String, Ruling>,
) -> std::io::Result<()> {
    let tree = sha256_hex(context.patch_bytes);
    let mut lines = String::new();
    for item in items {
        let Some(ruling) = rulings.get(&item.id) else {
            continue;
        };
        let line = RulingLine {
            format: RULING_FORMAT,
            review: context.review,
            tool: "relais",
            repo: context.repo,
            tree: &tree,
            candidate: &item.id,
            rule: &item.rule,
            pattern_id: &item.pattern_id,
            pattern: &item.pattern,
            ruling: ruling.verdict.as_str(),
            clause: &ruling.clause,
            at: context.at,
        };
        lines.push_str(&serde_json::to_string(&line).map_err(std::io::Error::other)?);
        lines.push('\n');
    }
    if lines.is_empty() {
        return Ok(());
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(artifacts.join(RULINGS_FILE))?
        .write_all(lines.as_bytes())
}

/// An RFC 3339 stamp as `YYYY-MM-DDTHH:MM:SSZ`; `None` when it is not one.
pub(in crate::runner) fn utc_seconds(rfc3339: &str) -> Option<String> {
    let parsed = chrono::DateTime::parse_from_rfc3339(rfc3339).ok()?;
    Some(
        parsed
            .with_timezone(&chrono::Utc)
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str, path: &str) -> Candidate {
        Candidate {
            id: id.to_string(),
            rule: "names.reveal-intent".into(),
            pattern_id: "names.reveal-intent/rust-weak-name".into(),
            pattern: "a weak name".into(),
            path: path.into(),
            line: 2,
            col: 9,
            text: "let data = 1;".into(),
        }
    }

    #[test]
    fn the_block_fences_the_ids_with_a_digest_of_the_entry_lines() {
        let part = PartCandidates {
            items: vec![candidate(&"a".repeat(64), "src/a.rs")],
            overflow: BTreeMap::from([("names.reveal-intent".to_string(), 3)]),
        };
        let block = candidates_block(&part);
        let entry = format!(
            "{} names.reveal-intent names.reveal-intent/rust-weak-name src/a.rs:2",
            "a".repeat(64)
        );
        let digest = sha256_hex(entry.as_bytes());
        assert!(
            block.contains(&format!(
                "<<<CANDIDATES sha={digest} n=1>>>\n{entry}\n<<<END CANDIDATES>>>\n"
            )),
            "{block}"
        );
        assert!(block.contains("not a finding"), "{block}");
        assert!(block.contains("BEFORE the final FINDINGS line"), "{block}");
        assert!(block.contains("let data = 1;"), "{block}");
        assert!(block.contains("names.reveal-intent: 3 more"), "{block}");
        assert_eq!(candidates_block(&PartCandidates::default()), "");
    }

    #[test]
    fn only_candidates_on_the_files_of_the_patch_are_listed() {
        let list = CandidateList {
            candidates: vec![candidate("1", "a.rs"), candidate("2", "b.rs")],
            overflow: BTreeMap::new(),
        };
        let part = PartCandidates::of(&list, &["b.rs".to_string()]);
        assert_eq!(part.items.len(), 1);
        assert_eq!(part.items[0].id, "2");
    }

    #[test]
    fn three_listed_and_two_ruled_leaves_one_undisposed() {
        let mut run = CandidatesRun::new(
            "sha",
            Ok(CandidateList {
                candidates: Vec::new(),
                overflow: BTreeMap::from([("r".to_string(), 4)]),
            }),
        );
        let items = vec![
            candidate("a", "x"),
            candidate("b", "x"),
            candidate("c", "x"),
        ];
        run.list(&items);
        let ruling = |id: &str, verdict| {
            (
                id.to_string(),
                Ruling {
                    id: id.to_string(),
                    verdict,
                    clause: "c".into(),
                },
            )
        };
        run.rule(&BTreeMap::from([
            ruling("a", Verdict::Violation),
            ruling("b", Verdict::Fine),
        ]));
        assert_eq!(
            run.record(),
            CandidatesRecord::Counted {
                listed: 3,
                violation: 1,
                fine: 1,
                undisposed: 1,
                overflow: 4
            }
        );
    }

    #[test]
    fn a_gap_is_unavailable_with_its_detail() {
        let gap = GuidanceGap::new(
            crate::context::GuidanceSource::Candidates,
            crate::context::guidance::GapCause::Exited(3),
            "adr-lint exit 3",
        );
        let run = CandidatesRun::new("sha", Err(gap.clone()));
        assert_eq!(
            run.record(),
            CandidatesRecord::Unavailable {
                detail: "adr-lint exit 3".into()
            }
        );
        assert_eq!(
            candidates_unavailable_line(&gap),
            "adr-lint's candidates are unavailable (adr-lint exit 3): no ruling is needed\n"
        );
    }

    #[test]
    fn the_stamp_is_whole_seconds_in_utc() {
        assert_eq!(
            utc_seconds("2026-10-25T12:00:01.123456+02:00").as_deref(),
            Some("2026-10-25T10:00:01Z")
        );
        assert_eq!(utc_seconds("yesterday"), None);
    }
}
