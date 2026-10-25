//! adr-lint's candidate lines for a candidate commit, and the rulings the
//! reviewer writes about them (SPEC §7).
//!
//! A candidate is a line to look at, never a finding. The pure parts live
//! here: the `--json` document, the argv, and the reading of a reviewer's
//! `Candidates:` block. The one bounded spawn goes through
//! `guidance::run_tool`.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use super::guidance::{self, GapCause, GuidanceGap, GuidanceSource};

/// The `format` token of the document this module reads.
pub const CANDIDATES_FORMAT: &str = "adr-lint.candidates/1";

/// The environment variable naming the adr-lint program to spawn.
pub const ADR_LINT_ENV: &str = "RELAIS_ADR_LINT";

/// How many candidates adr-lint is asked for.
pub const CANDIDATES_MAX: usize = 40;

/// The shortest id prefix a ruling may use.
pub const MIN_ID_PREFIX: usize = 12;

/// One line adr-lint asks the reviewer to look at.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Candidate {
    pub id: String,
    pub rule: String,
    pub pattern_id: String,
    pub pattern: String,
    pub path: String,
    pub line: u64,
    pub col: u64,
    pub text: String,
}

/// What adr-lint listed: the candidates, and how many more each rule had
/// beyond the cap.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CandidateList {
    pub candidates: Vec<Candidate>,
    pub overflow: BTreeMap<String, u64>,
}

#[derive(Deserialize)]
struct RawCandidates {
    #[serde(default)]
    format: String,
    #[serde(default)]
    candidates: Vec<Candidate>,
    #[serde(default)]
    overflow: BTreeMap<String, u64>,
}

fn gap(cause: GapCause, detail: impl Into<String>) -> GuidanceGap {
    GuidanceGap::new(GuidanceSource::Candidates, cause, detail)
}

/// Parse `adr-lint candidates --json` (format `adr-lint.candidates/1`). A
/// missing or different `format` token is a gap; unknown fields are ignored.
pub fn parse_candidates_output(stdout: &str) -> Result<CandidateList, GuidanceGap> {
    let raw: RawCandidates = serde_json::from_str(stdout.trim()).map_err(|e| {
        gap(
            GapCause::Unparseable,
            format!("unparseable adr-lint output: {e}"),
        )
    })?;
    if raw.format != CANDIDATES_FORMAT {
        return Err(gap(
            GapCause::Unparseable,
            format!(
                "adr-lint output has format {:?}, expected {CANDIDATES_FORMAT:?}",
                raw.format
            ),
        ));
    }
    if let Some(bad) = raw.candidates.iter().find(|c| !is_candidate_id(&c.id)) {
        return Err(gap(
            GapCause::Unparseable,
            format!(
                "adr-lint candidate id {:?} is not 64 lowercase hex characters",
                bad.id
            ),
        ));
    }
    Ok(CandidateList {
        candidates: raw.candidates,
        overflow: raw.overflow,
    })
}

fn is_candidate_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The argv of `adr-lint candidates` between the run's base and a candidate.
pub fn candidates_args(base: &str, head: &str) -> Vec<String> {
    [
        "candidates",
        "--base",
        base,
        "--head",
        head,
        "--json",
        "--max",
        &CANDIDATES_MAX.to_string(),
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

/// Spawn `program` (adr-lint) in `dir`, bounded and cancellable, and parse
/// its document. Every failure is a gap of `Candidates` naming adr-lint.
pub fn fetch_candidates(
    program: &str,
    dir: &Path,
    base: &str,
    head: &str,
    cancel: Option<&AtomicBool>,
) -> Result<CandidateList, GuidanceGap> {
    let stdout = guidance::run_tool(
        "adr-lint",
        program,
        &candidates_args(base, head),
        dir,
        GuidanceSource::Candidates,
        crate::tooling::GUIDANCE_TIMEOUT,
        cancel,
    )?;
    parse_candidates_output(&stdout)
}

/// How the reviewer disposed of a candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Violation,
    Fine,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Violation => "violation",
            Self::Fine => "fine",
        }
    }
}

/// One ruling, resolved to the full id of a listed candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ruling {
    pub id: String,
    pub verdict: Verdict,
    pub clause: String,
}

fn is_candidates_header(line: &str) -> bool {
    let marks = |c: char| matches!(c, '*' | '#' | ' ');
    let trimmed = line.trim().trim_matches(marks);
    let trimmed = trimmed.strip_suffix(':').unwrap_or(trimmed);
    trimmed
        .trim_matches(marks)
        .eq_ignore_ascii_case("Candidates")
}

/// `<id> violation: <clause>` or `<id> fine: <clause>`, with the leading
/// list marks, backticks and emphasis a model adds. The id is returned as
/// written (lowercased), not yet resolved.
fn parse_ruling_line(line: &str) -> Option<(String, Verdict, String)> {
    let line = line.trim_start_matches(['-', '*', '`', ' ']);
    let id_len = line.chars().take_while(char::is_ascii_hexdigit).count();
    if id_len == 0 {
        return None;
    }
    let id = line[..id_len].to_ascii_lowercase();
    let rest = line[id_len..]
        .trim_start_matches('`')
        .trim_start()
        .trim_start_matches('*');
    let word_len = rest.chars().take_while(char::is_ascii_alphabetic).count();
    let verdict = match rest[..word_len].to_ascii_lowercase().as_str() {
        "violation" => Verdict::Violation,
        "fine" => Verdict::Fine,
        _ => return None,
    };
    let after = rest[word_len..].trim_start_matches('*');
    let clause = after.strip_prefix(':')?;
    let clause = clause.trim_start_matches('*').trim().to_string();
    Some((id, verdict, clause))
}

/// The rulings in a reviewer's answer, keyed by the full id of the listed
/// candidate each one names. The block follows a `Candidates` header line;
/// blank lines after it are skipped, and it ends at the first other line
/// that is not a ruling. An id is the full id or a prefix of at least
/// `MIN_ID_PREFIX` hex digits matching exactly one listed id; an unknown or
/// ambiguous prefix rules nothing. The last ruling for an id wins.
pub fn parse_rulings(answer: &str, listed: &[&str]) -> BTreeMap<String, Ruling> {
    let mut rulings = BTreeMap::new();
    let mut lines = answer.lines();
    if !lines.any(is_candidates_header) {
        return rulings;
    }
    for line in lines.skip_while(|line| line.trim().is_empty()) {
        if line.trim().is_empty() {
            continue;
        }
        let Some((written, verdict, clause)) = parse_ruling_line(line) else {
            break;
        };
        if written.len() < MIN_ID_PREFIX {
            continue;
        }
        let mut matching = listed.iter().filter(|id| id.starts_with(written.as_str()));
        let (Some(id), None) = (matching.next(), matching.next()) else {
            continue;
        };
        rulings.insert(
            (*id).to_string(),
            Ruling {
                id: (*id).to_string(),
                verdict,
                clause,
            },
        );
    }
    rulings
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from adr-lint 0.3.0 (`adr-lint candidates --json`).
    const CANDIDATES_JSON: &str = r#"{"base":"e50d22fccef1f14949c620753adf8ffe376468bd","candidates":[{"col":16,"id":"610663331e1941e0e935e32f0b2443d99e3617f07cd522178cc07ab03e5c012a","line":2,"path":"src/a.rs","pattern":"`.unwrap()`, `.expect(` or `panic!`: a failure that should be a value in the return type, unless the invariant is proven","pattern_id":"failures.expected-vs-defect/rust-panic","rule":"failures.expected-vs-defect","text":"let data = std::fs::read(\"x\").unwrap();"},{"col":9,"id":"6998f43dd19b3a4891444a2b67b42ed87028c23713230bd4c05cece9b1acf6dc","line":2,"path":"src/a.rs","pattern":"a single-letter, placeholder or abbreviated name declared here (data, result, value, item, tmp, obj, cfg, ctx, mgr, svc)","pattern_id":"names.reveal-intent/rust-weak-name","rule":"names.reveal-intent","text":"let data = std::fs::read(\"x\").unwrap();"}],"format":"adr-lint.candidates/1","head":"2d94d48229cfa25910c8bb775d8547eef8ffd2dd","overflow":{}}"#;

    const FIRST: &str = "610663331e1941e0e935e32f0b2443d99e3617f07cd522178cc07ab03e5c012a";
    const SECOND: &str = "6998f43dd19b3a4891444a2b67b42ed87028c23713230bd4c05cece9b1acf6dc";

    #[test]
    fn the_captured_document_parses() {
        let list = parse_candidates_output(CANDIDATES_JSON).expect("parses");
        assert_eq!(list.candidates.len(), 2);
        assert_eq!(list.candidates[0].id, FIRST);
        assert_eq!(list.candidates[0].rule, "failures.expected-vs-defect");
        assert_eq!(
            list.candidates[1].pattern_id,
            "names.reveal-intent/rust-weak-name"
        );
        assert_eq!(list.candidates[1].path, "src/a.rs");
        assert_eq!((list.candidates[1].line, list.candidates[1].col), (2, 9));
        assert!(list.overflow.is_empty());
    }

    #[test]
    fn a_missing_or_other_format_token_is_an_unparseable_gap() {
        for bad in [
            r#"{"candidates":[],"overflow":{}}"#,
            r#"{"format":"adr-lint.candidates/2","candidates":[]}"#,
            "not json",
            "[]",
        ] {
            let gap = parse_candidates_output(bad).expect_err(bad);
            assert_eq!(gap.source, GuidanceSource::Candidates, "{bad}");
            assert_eq!(gap.cause, GapCause::Unparseable, "{bad}");
            assert!(gap.detail.contains("adr-lint"), "{}", gap.detail);
        }
    }

    #[test]
    fn a_candidate_id_that_is_not_64_lowercase_hex_is_an_unparseable_gap() {
        let upper = FIRST.to_uppercase();
        for id in [upper.as_str(), &FIRST[..63]] {
            let doc = format!(
                r#"{{"format":"adr-lint.candidates/1","candidates":[{{"id":"{id}","rule":"r","pattern_id":"p","pattern":"x","path":"a.rs","line":1,"col":1,"text":"t"}}]}}"#
            );
            let gap = parse_candidates_output(&doc).expect_err(id);
            assert_eq!(gap.source, GuidanceSource::Candidates, "{id}");
            assert_eq!(gap.cause, GapCause::Unparseable, "{id}");
        }
    }

    #[test]
    fn unknown_fields_are_ignored_and_overflow_is_read() {
        let list = parse_candidates_output(
            r#"{"format":"adr-lint.candidates/1","extra":1,"candidates":[],"overflow":{"names.reveal-intent":7}}"#,
        )
        .expect("parses");
        assert_eq!(list.overflow["names.reveal-intent"], 7);
    }

    #[test]
    fn the_argv_names_both_commits_and_the_cap() {
        assert_eq!(
            candidates_args("b", "h"),
            [
                "candidates",
                "--base",
                "b",
                "--head",
                "h",
                "--json",
                "--max",
                "40"
            ]
        );
    }

    #[test]
    fn rulings_follow_the_candidates_header_in_the_forms_models_write() {
        let answer = format!(
            "Verdict: ok\n\n**Candidates:**\n\n- `{}` violation: unwrap on input\n* {} **fine**: \"a defect is the only thing that panics\"\n\nFINDINGS:\n1. [low] a.rs:1 — x\n",
            &FIRST[..12],
            SECOND
        );
        let rulings = parse_rulings(&answer, &[FIRST, SECOND]);
        assert_eq!(rulings.len(), 2);
        assert_eq!(rulings[FIRST].verdict, Verdict::Violation);
        assert_eq!(rulings[FIRST].clause, "unwrap on input");
        assert_eq!(rulings[SECOND].verdict, Verdict::Fine);
        assert_eq!(
            rulings[SECOND].clause,
            "\"a defect is the only thing that panics\""
        );
    }

    #[test]
    fn short_unknown_and_ambiguous_ids_rule_nothing_and_the_last_ruling_wins() {
        let a = "aaaaaaaaaaaa1111";
        let b = "aaaaaaaaaaaa2222";
        let answer = "## Candidates\nabc fine: short\naaaaaaaaaaaa fine: ambiguous\ndeadbeefdead fine: unknown\naaaaaaaaaaaa1 fine: first\naaaaaaaaaaaa1 violation: second\nnot a ruling\naaaaaaaaaaaa2 fine: after the block\n";
        let rulings = parse_rulings(answer, &[a, b]);
        assert_eq!(rulings.len(), 1);
        assert_eq!(rulings[a].verdict, Verdict::Violation);
        assert_eq!(rulings[a].clause, "second");
    }

    #[test]
    fn no_header_means_no_rulings() {
        assert!(parse_rulings(&format!("{FIRST} fine: x\n"), &[FIRST]).is_empty());
    }
}
