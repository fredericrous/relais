//! What running a holdout established (ADR-0045, SPEC §10): the verdict a
//! deterministic command and its parsed JUnit report settle, kept on the
//! receipt without the store, the script, the overlay or any log line.

use serde::{Deserialize, Serialize};

use super::{CheckOutcome, JunitReport, TestOutcome};
use crate::procs::Ended;

/// How a holdout ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldoutVerdict {
    /// Exit 0, a parsed report, at least one test and every test passed.
    Passed,
    /// A failed or skipped test, or a non-zero exit over a parsed report.
    Failed,
    /// No usable report: the holdout could not be built or wrote none.
    DidNotBuild,
    /// The command was killed by its timeout.
    TimedOut,
}

impl HoldoutVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::DidNotBuild => "did_not_build",
            Self::TimedOut => "timed_out",
        }
    }
}

/// One test a holdout's report named.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HoldoutTest {
    pub id: String,
    pub outcome: TestOutcome,
}

/// The receipt's record of one holdout run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HoldoutRun {
    pub name: String,
    pub sha: String,
    pub digest: String,
    pub outcome: HoldoutVerdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<i32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<HoldoutTest>,
    /// The kept copy of the report, inside the run's artifacts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<String>,
    /// The kept copy of the command's log, inside the run's artifacts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
    /// Wall-clock milliseconds the holdout took, copy and setup included.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_ms: Option<u64>,
}

impl HoldoutRun {
    /// A holdout that never produced a report.
    pub fn did_not_build(name: &str, sha: &str, digest: &str) -> Self {
        Self {
            name: name.to_string(),
            sha: sha.to_string(),
            digest: digest.to_string(),
            outcome: HoldoutVerdict::DidNotBuild,
            exit: None,
            tests: Vec::new(),
            report: None,
            log: None,
            wall_ms: None,
        }
    }

    /// Settle a holdout from its command's outcome: the exit status
    /// together with the parsed report, nothing else.
    pub fn settled(name: &str, sha: &str, digest: &str, check: &CheckOutcome) -> Self {
        if matches!(check.ended.recorded(), Some(Ended::TimedOut)) {
            return Self {
                outcome: HoldoutVerdict::TimedOut,
                ..Self::did_not_build(name, sha, digest)
            };
        }
        let tests: Vec<HoldoutTest> = match &check.junit {
            Some(JunitReport::Reported { tests, .. }) => tests
                .iter()
                .map(|(id, outcome)| HoldoutTest {
                    id: id.clone(),
                    outcome: *outcome,
                })
                .collect(),
            Some(JunitReport::Missing | JunitReport::Unparseable { .. }) | None => {
                return Self::did_not_build(name, sha, digest)
            }
        };
        let exit = match check.ended.recorded() {
            Some(Ended::Exited(code)) => Some(code),
            Some(Ended::TimedOut | Ended::Cancelled | Ended::Signalled) | None => None,
        };
        let all_passed = tests.iter().all(|test| test.outcome == TestOutcome::Passed);
        let outcome = match (exit, tests.is_empty(), all_passed) {
            (Some(0), false, true) => HoldoutVerdict::Passed,
            (Some(0), true, _) => HoldoutVerdict::DidNotBuild,
            _ => HoldoutVerdict::Failed,
        };
        Self {
            outcome,
            exit,
            tests,
            ..Self::did_not_build(name, sha, digest)
        }
    }

    /// The ids of the tests that did not pass.
    pub fn failing(&self) -> Vec<String> {
        self.tests
            .iter()
            .filter(|test| test.outcome != TestOutcome::Passed)
            .map(|test| test.id.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::verify::{AcceptanceGap, CheckEnd, MissingEvidence};

    fn check(ended: Ended, junit: Option<JunitReport>) -> CheckOutcome {
        CheckOutcome {
            label: "sh@00000000".into(),
            argv: vec!["sh".into()],
            ended: CheckEnd::new(ended),
            log_path: "log".into(),
            log_sha256: String::new(),
            junit,
            attempts: Vec::new(),
            flaky: false,
        }
    }

    fn reported(tests: &[(&str, TestOutcome)]) -> Option<JunitReport> {
        Some(JunitReport::Reported {
            tests: tests
                .iter()
                .map(|(id, outcome)| (id.to_string(), *outcome))
                .collect::<BTreeMap<_, _>>(),
            artifact: None,
            totals_consistent: true,
        })
    }

    fn settled(check: &CheckOutcome) -> HoldoutRun {
        HoldoutRun::settled("h", "abc", "def", check)
    }

    #[test]
    fn the_exit_status_and_the_report_together_decide_the_verdict() {
        use HoldoutVerdict::{DidNotBuild, Failed, Passed};
        use TestOutcome::{Failed as Red, Passed as Green, Skipped};
        let cases = [
            (Ended::Exited(0), reported(&[("a", Green)]), Passed, vec![]),
            (Ended::Exited(0), reported(&[]), DidNotBuild, vec![]),
            (Ended::Exited(0), None, DidNotBuild, vec![]),
            (
                Ended::Exited(0),
                Some(JunitReport::Missing),
                DidNotBuild,
                vec![],
            ),
            (
                Ended::Exited(0),
                reported(&[("a", Green), ("b", Skipped)]),
                Failed,
                vec!["b"],
            ),
            (Ended::Exited(1), reported(&[("a", Red)]), Failed, vec!["a"]),
            (Ended::Exited(1), reported(&[("a", Green)]), Failed, vec![]),
        ];
        for (ended, junit, verdict, failing) in cases {
            let run = settled(&check(ended, junit.clone()));
            assert_eq!(run.outcome, verdict, "{ended:?} {junit:?}");
            assert_eq!(run.failing(), failing, "{ended:?} {junit:?}");
        }
    }

    #[test]
    fn a_timed_out_command_settles_as_timed_out_with_or_without_a_report() {
        for junit in [
            reported(&[("a", TestOutcome::Passed)]),
            reported(&[("a", TestOutcome::Failed)]),
            Some(JunitReport::Missing),
            None,
        ] {
            let run = settled(&check(Ended::TimedOut, junit));
            assert_eq!(run.outcome, HoldoutVerdict::TimedOut);
            assert_eq!(run.outcome.as_str(), "timed_out");
            assert_eq!(
                serde_json::to_string(&run.outcome).unwrap(),
                "\"timed_out\""
            );
        }
    }

    #[test]
    fn a_timed_out_gap_says_to_raise_the_timeout() {
        let message = AcceptanceGap {
            criterion_id: "c-1".into(),
            missing: MissingEvidence::HoldoutTimedOut { name: "h".into() },
        }
        .message();
        assert!(message.contains("timed out after"), "{message}");
        assert!(message.contains("timeout_seconds"), "{message}");
    }

    #[test]
    fn a_gap_names_the_criterion_and_the_holdout_and_nothing_of_the_store() {
        let gaps = [
            MissingEvidence::HoldoutNotRun { name: "h".into() },
            MissingEvidence::HoldoutFailed {
                name: "h".into(),
                failing: vec!["t::a".into(), "t::b".into()],
            },
            MissingEvidence::HoldoutFailed {
                name: "h".into(),
                failing: Vec::new(),
            },
            MissingEvidence::HoldoutDidNotBuild { name: "h".into() },
        ];
        let expected = [
            "was not run",
            "failed: t::a, t::b",
            "failed: did not exit 0 (a non-zero exit, a timeout, a signal or a cancel)",
            "did not build or wrote no report",
        ];
        for (missing, tail) in gaps.into_iter().zip(expected) {
            let message = AcceptanceGap {
                criterion_id: "c-1".into(),
                missing,
            }
            .message();
            assert!(message.contains("acceptance criterion `c-1`"), "{message}");
            assert!(message.contains("holdout `h`"), "{message}");
            assert!(message.ends_with(tail), "{message}");
            for hidden in ["file://", "secret/path", "src/overlay.rs", "runner.toml"] {
                assert!(!message.contains(hidden), "{message}");
            }
        }
    }
}
