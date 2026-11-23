//! Carrying passed checks to an unchanged candidate (SPEC §10): when a
//! repair or an escalation returns the sha the previous attempt verified,
//! the checks that failed there run again with every check before the last
//! of them (profile commands run in order in one tree, so a later command
//! may read what an earlier one built), and the passes after it are carried.
//! Everything here is a calculation over values; the runner owns the I/O.

use crate::ids::sha256_hex;
use crate::policy::VerificationProfile;
use crate::verify::CheckOutcome;

/// What the last verification that ran the profile's commands left behind:
/// the candidate it ran on and the inputs it ran under.
#[derive(Debug, Clone)]
pub(super) struct LastVerified {
    pub(super) sha: String,
    /// The log prefix that verification's checks were logged under.
    pub(super) phase: String,
    pub(super) fingerprint: String,
    pub(super) inputs_changed: Vec<String>,
    /// One outcome per profile command, in profile order.
    pub(super) outcomes: Vec<CheckOutcome>,
}

/// Where the candidate is verified from: a run in place seeds its trees from
/// a checkout somebody else may be writing while the run goes on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Placement {
    InPlace,
    Isolated,
}

/// The profile's commands and setup, hashed: what a carried pass was run
/// under.
pub(super) fn profile_fingerprint(profile: &VerificationProfile) -> String {
    let declared = serde_json::to_string(&(&profile.setup, &profile.commands))
        .expect("a command spec is plain data");
    sha256_hex(declared.as_bytes())
}

/// Why a verification that could have carried passes re-ran every check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NotCarried {
    InPlace,
    ProfileChanged,
    InputsChanged,
}

impl NotCarried {
    pub(super) fn reason(self) -> String {
        let cause = match self {
            Self::InPlace => "in-place run",
            Self::ProfileChanged => "profile changed",
            Self::InputsChanged => "inputs changed",
        };
        format!("re-ran every check: {cause}")
    }
}

/// A carry: the first `rerun` commands run again, the outcomes after them
/// are carried from `outcomes`, which ran under `from`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CarryPlan<'a> {
    pub(super) from: &'a str,
    pub(super) outcomes: &'a [CheckOutcome],
    pub(super) rerun: usize,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum Carry<'a> {
    /// No earlier verification of this candidate, or nothing after its last
    /// failure to carry: every check runs, as it always did.
    Nothing,
    /// An earlier verification of this candidate exists, and why its passes
    /// are not carried.
    Refused(NotCarried),
    Plan(CarryPlan<'a>),
}

/// Whose outcomes stand in for running the profile's commands.
#[derive(Debug, Clone, Copy)]
pub(crate) enum CheckReuse<'a> {
    /// Every command runs.
    None,
    /// The candidate is the base tree: the baseline's outcomes are its own.
    Base(&'a [CheckOutcome]),
    Carry(CarryPlan<'a>),
}

/// The candidate being verified and the inputs it is verified under.
#[derive(Debug, Clone, Copy)]
pub(super) struct CarryQuery<'a> {
    pub(super) candidate_sha: &'a str,
    pub(super) placement: Placement,
    pub(super) profile: &'a VerificationProfile,
    pub(super) inputs_changed: &'a [String],
}

/// Whether, and how much, of the last verification is carried to this one.
pub(super) fn decide_carry<'a>(
    last: Option<&'a LastVerified>,
    query: &CarryQuery<'_>,
) -> Carry<'a> {
    let Some(last) = last.filter(|last| last.sha == query.candidate_sha) else {
        return Carry::Nothing;
    };
    if query.placement == Placement::InPlace {
        return Carry::Refused(NotCarried::InPlace);
    }
    if last.fingerprint != profile_fingerprint(query.profile) {
        return Carry::Refused(NotCarried::ProfileChanged);
    }
    if last.inputs_changed != query.inputs_changed {
        return Carry::Refused(NotCarried::InputsChanged);
    }
    if last.outcomes.len() != query.profile.commands.len() {
        return Carry::Nothing;
    }
    let rerun = last
        .outcomes
        .iter()
        .rposition(CheckOutcome::failed)
        .map_or(0, |last_failed| last_failed + 1);
    if rerun == last.outcomes.len() {
        return Carry::Nothing;
    }
    Carry::Plan(CarryPlan {
        from: &last.phase,
        outcomes: &last.outcomes,
        rerun,
    })
}

/// The longest a decision's reason runs, in characters.
const REASON_COLUMNS: usize = 80;

/// The first `shown` labels, then `+N more` for the rest.
fn label_list(labels: &[&str], shown: usize) -> String {
    let head = labels
        .iter()
        .take(shown)
        .copied()
        .collect::<Vec<_>>()
        .join(", ");
    match labels.len().saturating_sub(shown) {
        0 => head,
        more => format!("{head} +{more} more"),
    }
}

/// `carried 2 passes from attempt1 (make, test); re-ran lint`: at most three
/// labels a list, then `+N more`, and fewer labels still when the line would
/// pass [`REASON_COLUMNS`]; the line is clipped only when one label a list
/// does not fit. For people to read; never parsed.
pub(super) fn carried_reason(from: &str, carried: &[&str], rerun: &[&str]) -> String {
    const MOST_LABELS_SHOWN: usize = 3;
    let passes = if carried.len() == 1 { "pass" } else { "passes" };
    let line = |shown: usize| {
        let mut reason = format!(
            "carried {} {passes} from {from} ({})",
            carried.len(),
            label_list(carried, shown)
        );
        if !rerun.is_empty() {
            reason.push_str(&format!("; re-ran {}", label_list(rerun, shown)));
        }
        reason
    };
    let fitting = (1..=MOST_LABELS_SHOWN)
        .rev()
        .map(line)
        .find(|reason| reason.chars().count() <= REASON_COLUMNS);
    fitting.unwrap_or_else(|| {
        let mut clipped: String = line(1).chars().take(REASON_COLUMNS - 1).collect();
        clipped.push('…');
        clipped
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::CommandSpec;
    use crate::procs::Ended;
    use crate::verify::CheckEnd;

    fn spec(program: &str) -> CommandSpec {
        serde_json::from_value(serde_json::json!({ "argv": [program] })).expect("a command")
    }

    fn profile(programs: &[&str]) -> VerificationProfile {
        VerificationProfile {
            commands: programs.iter().map(|program| spec(program)).collect(),
            ..Default::default()
        }
    }

    fn outcome(label: &str, code: i32) -> CheckOutcome {
        CheckOutcome {
            label: label.to_string(),
            argv: vec![label.to_string()],
            ended: CheckEnd::new(Ended::Exited(code)),
            log_path: format!("/logs/{label}.log"),
            log_sha256: "00".to_string(),
            junit: None,
            attempts: Vec::new(),
            flaky: false,
            flaky_reason: None,
            applied_limit_ms: 0,
            clipped: false,
            timing: None,
            carried_from: None,
        }
    }

    fn last(profile: &VerificationProfile, codes: &[i32]) -> LastVerified {
        LastVerified {
            sha: "abc".to_string(),
            phase: "attempt1".to_string(),
            fingerprint: profile_fingerprint(profile),
            inputs_changed: Vec::new(),
            outcomes: codes
                .iter()
                .enumerate()
                .map(|(at, code)| outcome(&format!("c{at}"), *code))
                .collect(),
        }
    }

    fn query<'a>(profile: &'a VerificationProfile, inputs: &'a [String]) -> CarryQuery<'a> {
        CarryQuery {
            candidate_sha: "abc",
            placement: Placement::Isolated,
            profile,
            inputs_changed: inputs,
        }
    }

    #[test]
    fn the_passes_after_the_last_failure_are_carried() {
        let profile = profile(&["lint", "make", "test"]);
        let last = last(&profile, &[1, 0, 0]);
        let Carry::Plan(plan) = decide_carry(Some(&last), &query(&profile, &[])) else {
            panic!("a carry");
        };
        assert_eq!((plan.rerun, plan.from), (1, "attempt1"));
    }

    #[test]
    fn a_later_failure_means_the_earlier_passes_run_again_too() {
        let profile = profile(&["make", "test"]);
        let last = last(&profile, &[0, 1]);
        assert!(matches!(
            decide_carry(Some(&last), &query(&profile, &[])),
            Carry::Nothing
        ));
    }

    #[test]
    fn all_passed_carries_everything() {
        let profile = profile(&["make", "test"]);
        let last = last(&profile, &[0, 0]);
        let Carry::Plan(plan) = decide_carry(Some(&last), &query(&profile, &[])) else {
            panic!("a carry");
        };
        assert_eq!(plan.rerun, 0);
    }

    #[test]
    fn another_sha_carries_nothing() {
        let profile = profile(&["lint", "make"]);
        let last = last(&profile, &[1, 0]);
        let other = CarryQuery {
            candidate_sha: "def",
            ..query(&profile, &[])
        };
        assert!(matches!(decide_carry(Some(&last), &other), Carry::Nothing));
        assert!(matches!(
            decide_carry(None, &query(&profile, &[])),
            Carry::Nothing
        ));
    }

    #[test]
    fn an_in_place_run_refuses_and_says_so() {
        let profile = profile(&["lint", "make"]);
        let last = last(&profile, &[1, 0]);
        let in_place = CarryQuery {
            placement: Placement::InPlace,
            ..query(&profile, &[])
        };
        let Carry::Refused(why) = decide_carry(Some(&last), &in_place) else {
            panic!("a refusal");
        };
        assert_eq!(why.reason(), "re-ran every check: in-place run");
    }

    #[test]
    fn a_different_profile_or_inputs_list_refuses_the_carry() {
        let profile = profile(&["lint", "make"]);
        let last = last(&profile, &[1, 0]);
        let changed = self::profile(&["lint", "make", "extra"]);
        assert!(matches!(
            decide_carry(Some(&last), &query(&changed, &[])),
            Carry::Refused(NotCarried::ProfileChanged)
        ));
        let inputs = vec!["tests/a.rs".to_string()];
        assert!(matches!(
            decide_carry(Some(&last), &query(&profile, &inputs)),
            Carry::Refused(NotCarried::InputsChanged)
        ));
    }

    #[test]
    fn a_carried_flaky_pass_stays_flaky_and_is_never_red() {
        let mut flaky = outcome("make", 0);
        flaky.flaky = true;
        flaky.flaky_reason = Some("timed_out_then_passed".to_string());
        let carried = flaky.carried("attempt2");
        assert!(carried.flaky && !carried.failed());
        assert_eq!(carried.carried_from.as_deref(), Some("attempt2"));
    }

    #[test]
    fn a_chain_of_carries_names_the_attempt_that_ran_the_command() {
        let once = outcome("make", 0).carried("attempt1");
        let twice = once.carried("attempt2");
        assert_eq!(twice.carried_from.as_deref(), Some("attempt1"));
    }

    #[test]
    fn the_reason_fits_80_columns_and_cuts_label_lists_to_three() {
        assert_eq!(
            carried_reason("attempt1", &["make", "test"], &["lint"]),
            "carried 2 passes from attempt1 (make, test); re-ran lint"
        );
        assert_eq!(
            carried_reason("attempt1", &["a", "b", "c", "d", "e"], &[]),
            "carried 5 passes from attempt1 (a, b, c +2 more)"
        );
        let carried = ["x@1a2b3c4d"; 5];
        let reason = carried_reason("attempt1", &carried, &["lint@01234567"]);
        assert!(reason.chars().count() <= REASON_COLUMNS, "{reason}");
        assert!(reason.contains("more") && reason.contains("re-ran lint@01234567"));
    }
}
