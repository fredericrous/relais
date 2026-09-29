//! Per-test results from a verification command's JUnit XML report
//! (SPEC §10, #51).
//!
//! A verification command reports an exit status and nothing finer, so a
//! contract could not name one test and have anything check it. A command
//! that also writes a JUnit report — cargo-nextest, pytest `--junitxml`,
//! the vitest and jest junit reporters — gives that a place to come from.
//! Parsing is a pure function over bytes; what the command's report
//! turned out to be is a [`JunitReport`], recorded on the check outcome
//! whether or not it could be read.

use std::collections::BTreeMap;

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use serde::{Deserialize, Serialize};

/// One test's outcome. Ordered by how much it matters: a test reported
/// twice (a parametrized name, a retried run) keeps the worst of its
/// outcomes, so a failure is never hidden behind a pass of the same id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestOutcome {
    Passed,
    Skipped,
    Failed,
}

impl std::fmt::Display for TestOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Passed => "Passed",
            Self::Failed => "Failed",
            Self::Skipped => "Skipped",
        })
    }
}

/// What a command's declared JUnit report turned out to be. Absent on a
/// check outcome (`None`) means the command declared none; a declared
/// report that could not be read is a recorded fact, never an empty
/// result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JunitReport {
    /// The command declared a report and it was not there afterwards.
    Missing,
    /// The file was there and is not a JUnit report.
    Unparseable { reason: String },
    /// Every test the report names, by test id (`classname::name`, or
    /// `name` when the report gives no classname).
    Reported {
        tests: BTreeMap<String, TestOutcome>,
        /// The copy of the report kept with the run's other evidence.
        /// Absent in a receipt written before the copy was kept.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact: Option<JunitArtifact>,
    },
}

/// A kept copy of a JUnit report: where it is and its sha256.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JunitArtifact {
    pub path: String,
    pub sha256: String,
}

impl JunitReport {
    /// The recorded fact when the report could not be used, in the words
    /// a receipt or a gap quotes; `None` for a report that was read.
    pub fn fact(&self) -> Option<String> {
        match self {
            Self::Missing => Some("junit: missing".to_string()),
            Self::Unparseable { reason } => Some(format!("junit: unparseable ({reason})")),
            Self::Reported { .. } => None,
        }
    }
}

/// Why a report is not JUnit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JunitError {
    /// The XML itself is malformed.
    Xml { position: u64, detail: String },
    /// Well-formed XML whose root is not `testsuites` or `testsuite`.
    NotJunit { root: String },
    /// The document has no root element.
    Empty,
    /// The document ended inside an open element.
    Truncated,
    /// A `testcase` with no `name`: nothing to settle a criterion by.
    Nameless,
}

impl std::fmt::Display for JunitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Xml { position, detail } => write!(f, "{detail} at byte {position}"),
            Self::NotJunit { root } => {
                write!(
                    f,
                    "root element is `{root}`, not `testsuites` or `testsuite`"
                )
            }
            Self::Empty => f.write_str("no root element"),
            Self::Truncated => f.write_str("the document ends inside an open element"),
            Self::Nameless => f.write_str("a testcase has no name"),
        }
    }
}

impl std::error::Error for JunitError {}

/// The elements this reader tells apart; everything else is `Other`.
enum Element {
    Suite,
    Case,
    /// `failure` or `error`.
    Failing,
    Skipped,
    Other,
}

fn classify(name: &str) -> Element {
    match name {
        "testsuite" | "testsuites" => Element::Suite,
        "testcase" => Element::Case,
        "failure" | "error" => Element::Failing,
        "skipped" => Element::Skipped,
        _ => Element::Other,
    }
}

/// Parse a JUnit XML report into each test's outcome, by test id.
///
/// The id is `classname::name` when the testcase carries a non-empty
/// classname, otherwise `name`. A testcase with a `failure` or `error`
/// child is Failed, one with a `skipped` child is Skipped, any other is
/// Passed — nextest's `flakyFailure` and `rerunFailure` children describe
/// attempts before a pass and do not change it.
pub fn parse_junit(bytes: &[u8]) -> Result<BTreeMap<String, TestOutcome>, JunitError> {
    let mut reader = Reader::from_reader(bytes);
    let mut tests: BTreeMap<String, TestOutcome> = BTreeMap::new();
    let mut open_elements = 0usize;
    let mut root_seen = false;
    let mut open_case: Option<(String, TestOutcome)> = None;
    loop {
        let event = reader.read_event().map_err(|e| JunitError::Xml {
            position: reader.error_position(),
            detail: e.to_string(),
        })?;
        match event {
            Event::Start(element) => {
                check_root(&element, &mut root_seen, open_elements)?;
                open_elements += 1;
                match classify(element.local_name().as_ref()) {
                    Element::Case if open_case.is_none() => {
                        open_case = Some((
                            test_id(&element, reader.buffer_position())?,
                            TestOutcome::Passed,
                        ));
                    }
                    Element::Failing => worsen(&mut open_case, TestOutcome::Failed),
                    Element::Skipped => worsen(&mut open_case, TestOutcome::Skipped),
                    Element::Case | Element::Suite | Element::Other => {}
                }
            }
            Event::Empty(element) => {
                check_root(&element, &mut root_seen, open_elements)?;
                match classify(element.local_name().as_ref()) {
                    Element::Case if open_case.is_none() => {
                        let id = test_id(&element, reader.buffer_position())?;
                        record(&mut tests, id, TestOutcome::Passed);
                    }
                    Element::Failing => worsen(&mut open_case, TestOutcome::Failed),
                    Element::Skipped => worsen(&mut open_case, TestOutcome::Skipped),
                    Element::Case | Element::Suite | Element::Other => {}
                }
            }
            Event::End(element) => {
                open_elements = open_elements.saturating_sub(1);
                if let Element::Case = classify(element.local_name().as_ref()) {
                    if let Some((id, outcome)) = open_case.take() {
                        record(&mut tests, id, outcome);
                    }
                }
            }
            Event::Eof => break,
            // Text, CDATA, comments, the declaration and processing
            // instructions carry no test outcome.
            _ => {}
        }
    }
    if !root_seen {
        return Err(JunitError::Empty);
    }
    if open_elements != 0 {
        return Err(JunitError::Truncated);
    }
    Ok(tests)
}

/// The first element of the document must be a suite (or suites).
fn check_root(
    element: &BytesStart<'_>,
    root_seen: &mut bool,
    open_elements: usize,
) -> Result<(), JunitError> {
    if open_elements == 0 && !*root_seen {
        *root_seen = true;
        if let Element::Suite = classify(element.local_name().as_ref()) {
            return Ok(());
        }
        return Err(JunitError::NotJunit {
            root: element.local_name().as_ref().to_string(),
        });
    }
    Ok(())
}

fn worsen(open_case: &mut Option<(String, TestOutcome)>, outcome: TestOutcome) {
    if let Some((_, current)) = open_case {
        *current = (*current).max(outcome);
    }
}

fn record(tests: &mut BTreeMap<String, TestOutcome>, id: String, outcome: TestOutcome) {
    tests
        .entry(id)
        .and_modify(|current| *current = (*current).max(outcome))
        .or_insert(outcome);
}

/// `position` is the reader's offset after the start tag that holds the
/// attribute — the END of that tag, not the attribute's own offset — so an
/// attribute that cannot be decoded is reported at the tag that carries
/// it, not at byte 0.
fn test_id(case: &BytesStart<'_>, position: u64) -> Result<String, JunitError> {
    let name = attribute(case, "name", position)?.ok_or(JunitError::Nameless)?;
    Ok(match attribute(case, "classname", position)? {
        Some(classname) if !classname.is_empty() => format!("{classname}::{name}"),
        Some(_) | None => name,
    })
}

fn attribute(
    element: &BytesStart<'_>,
    key: &str,
    position: u64,
) -> Result<Option<String>, JunitError> {
    for attribute in element.attributes() {
        let attribute = attribute.map_err(|e| JunitError::Xml {
            position,
            detail: format!("{e}, in a start tag ending"),
        })?;
        if attribute.key.as_ref() == key {
            let value = attribute
                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .map_err(|e| JunitError::Xml {
                    position,
                    detail: format!("{e}, in a start tag ending"),
                })?;
            return Ok(Some(value.into_owned()));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        let path = format!("{}/tests/fixtures/junit/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read(&path).unwrap_or_else(|e| panic!("fixture {path}: {e}"))
    }

    #[test]
    fn a_nextest_report_names_each_test_by_binary_and_path() {
        let tests = parse_junit(&fixture("nextest.xml")).expect("parses");
        assert_eq!(tests.len(), 3);
        assert_eq!(
            tests.get(
                "relais::acceptance::tests::a_bare_string_parses_and_is_mandatory_with_no_evidence"
            ),
            Some(&TestOutcome::Passed)
        );
        assert_eq!(
            tests.get("relais::verify::tests::a_named_check_is_met_when_it_succeeded"),
            Some(&TestOutcome::Passed),
            "a CDATA child does not disturb the testcase"
        );
        assert_eq!(
            tests.get("relais::verify::tests::a_flaky_test_passes_on_retry"),
            Some(&TestOutcome::Passed),
            "a flakyFailure is an attempt before a pass"
        );
    }

    #[test]
    fn a_pytest_report_uses_classname_when_present_and_name_otherwise() {
        let tests = parse_junit(&fixture("pytest.xml")).expect("parses");
        assert_eq!(
            tests.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "test_without_a_class",
                "tests.test_api.TestLimits::test_accepts_the_maximum",
                "tests.test_api::test_rejects_malformed_input",
            ]
        );
        assert!(tests
            .values()
            .all(|outcome| *outcome == TestOutcome::Passed));
    }

    #[test]
    fn an_undecodable_attribute_is_reported_at_the_end_of_its_start_tag() {
        let xml = b"<testsuite>\n<testcase name=\"a &bogus; b\"/>\n</testsuite>";
        match parse_junit(xml) {
            // `<testsuite>\n` is 12 bytes and the 30-byte `<testcase …/>`
            // tag ends at 42: the end of the tag, not the attribute.
            Err(JunitError::Xml { position, detail }) => {
                assert_eq!(position, 42);
                assert!(detail.ends_with("in a start tag ending"), "{detail}");
            }
            other => panic!("expected an Xml error, got {other:?}"),
        }
    }

    #[test]
    fn failure_and_error_children_are_failed() {
        let tests = parse_junit(&fixture("failure.xml")).expect("parses");
        assert_eq!(tests["tests.test_api::test_passes"], TestOutcome::Passed);
        assert_eq!(tests["tests.test_api::test_fails"], TestOutcome::Failed);
        assert_eq!(tests["tests.test_api::test_errors"], TestOutcome::Failed);
    }

    #[test]
    fn a_skipped_child_is_skipped_and_entities_are_unescaped() {
        let tests = parse_junit(&fixture("skipped.xml")).expect("parses");
        assert_eq!(
            tests["src/api.test.ts::api > rejects malformed input"],
            TestOutcome::Passed
        );
        assert_eq!(
            tests["src/api.test.ts::api > accepts the maximum"],
            TestOutcome::Skipped
        );
    }

    #[test]
    fn a_truncated_report_is_an_error_not_an_empty_result() {
        let error = parse_junit(&fixture("malformed.xml")).unwrap_err();
        assert!(!error.to_string().is_empty(), "{error:?}");
    }

    #[test]
    fn what_is_not_a_junit_report_is_an_error() {
        assert_eq!(
            parse_junit(b"<html><body/></html>").unwrap_err(),
            JunitError::NotJunit {
                root: "html".into()
            }
        );
        assert_eq!(parse_junit(b"").unwrap_err(), JunitError::Empty);
        assert_eq!(
            parse_junit(b"<testsuite><testcase classname=\"a\"/></testsuite>").unwrap_err(),
            JunitError::Nameless
        );
        assert_eq!(
            parse_junit(b"<testsuite><testcase name=\"a\">").unwrap_err(),
            JunitError::Truncated
        );
    }

    #[test]
    fn a_report_with_no_testcases_is_empty_but_valid() {
        assert!(parse_junit(b"<testsuites/>").expect("parses").is_empty());
    }

    #[test]
    fn a_test_reported_twice_keeps_its_worst_outcome() {
        let report = br#"<testsuite>
            <testcase name="a"/>
            <testcase name="a"><failure/></testcase>
            <testcase name="a"/>
        </testsuite>"#;
        assert_eq!(
            parse_junit(report).expect("parses")["a"],
            TestOutcome::Failed
        );
    }

    #[test]
    fn an_unreadable_report_states_its_fact() {
        assert_eq!(
            JunitReport::Missing.fact().as_deref(),
            Some("junit: missing")
        );
        assert_eq!(
            JunitReport::Unparseable {
                reason: "no root element".into()
            }
            .fact()
            .as_deref(),
            Some("junit: unparseable (no root element)")
        );
        assert_eq!(
            JunitReport::Reported {
                tests: BTreeMap::new(),
                artifact: None,
            }
            .fact(),
            None
        );
    }
}
