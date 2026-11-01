//! What amont says a tree carries a verdict for (decisions programme "one
//! verdict per tree", t03/t04; amont ADR-0015).
//!
//! A profile whose commands are `amont run --stamp <gate>` lets amont run the
//! gate and record the verdict on the tree it judged, so the person's later
//! commit and push of that exact tree skip the suite. relais then asks
//! `amont stamps --json`, through this one documented interface, whether the
//! verdict was actually recorded. It reads nothing else: no git notes, no
//! signatures (SPEC §10, §18).
//!
//! Two questions are asked:
//!
//! - **after the candidate's profile** ([`stamp_gaps`]): a gate the answer
//!   reports `deferred` or `absent`, or an answer that could not be had, is a
//!   verification gap. A gate that ran green but left no stamp means the
//!   push will run it again, and a deferred one never ran here at all; either
//!   way the run must not read as "amont vouches for this tree";
//! - **before the base's profile** ([`base_is_proven`]): a base on which every
//!   profile gate is `stamped` or `attested` was already proven green on this
//!   platform, so its baseline run is skipped.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use crate::policy::VerificationProfile;
use crate::procs::Ended;

/// How long `amont stamps` may take: it reads notes, and for a tree other
/// than HEAD's it checks one out to read the attestation, so it is bounded
/// and cancellable like every other probe (audit V6).
pub const STAMPS_TIMEOUT: Duration = Duration::from_secs(60);

/// The argv prefix of a profile command that runs an amont gate and stamps it.
pub const STAMP_PREFIX: [&str; 3] = ["amont", "run", "--stamp"];

/// One gate's state, as `amont stamps --json` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Stamped,
    Attested,
    Deferred,
    Absent,
}

impl State {
    fn parse(word: &str) -> Option<State> {
        match word {
            "stamped" => Some(State::Stamped),
            "attested" => Some(State::Attested),
            "deferred" => Some(State::Deferred),
            "absent" => Some(State::Absent),
            _ => None,
        }
    }

    /// Whether this state is a verdict amont vouches for.
    pub fn vouched(self) -> bool {
        match self {
            State::Stamped | State::Attested => true,
            State::Deferred | State::Absent => false,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            State::Stamped => "stamped",
            State::Attested => "attested",
            State::Deferred => "deferred",
            State::Absent => "absent",
        }
    }
}

/// `amont stamps --json`'s answer: the tree asked about, the platform it
/// answered for, and each gate's state keyed by check id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub tree: String,
    pub platform: String,
    pub gates: BTreeMap<String, State>,
}

/// Why there is no answer. Every one of them is a gap, never a pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StampsError {
    /// Nothing was asked: the amont integration is off, or a fixture asks
    /// nothing.
    NotAsked,
    /// `amont stamps` could not be started, timed out, or was cancelled.
    NotRun { detail: String },
    /// It ran and did not exit 0: a usage error, such as an amont before
    /// 1.66 that has no `stamps` subcommand, or a gate name it does not know.
    Refused { ended: String },
    /// It exited 0 and printed something this relais cannot read.
    Unreadable { detail: String },
}

impl std::fmt::Display for StampsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StampsError::NotAsked => write!(f, "not asked"),
            StampsError::NotRun { detail } => write!(f, "`amont stamps` did not run: {detail}"),
            StampsError::Refused { ended } => write!(
                f,
                "`amont stamps` refused ({ended}); amont 1.66 or later answers it"
            ),
            StampsError::Unreadable { detail } => {
                write!(
                    f,
                    "`amont stamps --json` answered something unreadable: {detail}"
                )
            }
        }
    }
}

/// The gates a profile runs through `amont run --stamp`, in profile order,
/// each once. Setup steps never stamp; a command that is not a stamp command
/// contributes nothing.
pub fn profile_gates(profile: &VerificationProfile) -> Vec<String> {
    let mut gates: Vec<String> = Vec::new();
    for command in &profile.commands {
        let argv = &command.argv;
        if argv.len() > STAMP_PREFIX.len()
            && argv
                .iter()
                .zip(STAMP_PREFIX)
                .all(|(word, want)| word == want)
        {
            for gate in &argv[STAMP_PREFIX.len()..] {
                if !gates.contains(gate) {
                    gates.push(gate.clone());
                }
            }
        }
    }
    gates
}

/// Parse `amont stamps --json`. Pure, and tested: it is the whole of what
/// relais reads from amont about a tree's verdict.
pub fn parse(stdout: &str) -> Result<Answer, StampsError> {
    let unreadable = |detail: String| StampsError::Unreadable { detail };
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).map_err(|e| unreadable(e.to_string()))?;
    let text = |key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| unreadable(format!("no `{key}`")))
    };
    let tree = text("tree")?;
    let platform = text("platform")?;
    let gates = value
        .get("gates")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| unreadable("no `gates` object".into()))?;
    let mut states = BTreeMap::new();
    for (gate, state) in gates {
        let word = state.as_str().unwrap_or_default();
        let state = State::parse(word)
            .ok_or_else(|| unreadable(format!("gate `{gate}` has state {state}")))?;
        states.insert(gate.clone(), state);
    }
    Ok(Answer {
        tree,
        platform,
        gates: states,
    })
}

/// Gaps on a candidate whose profile ran stamp commands: every gate amont
/// reports must be vouched for, and an answer that names no gate at all, or
/// no answer, is a gap naming why.
pub fn stamp_gaps(asked: &[String], answer: &Result<Answer, StampsError>) -> Vec<String> {
    if asked.is_empty() {
        return Vec::new();
    }
    match answer {
        Err(StampsError::NotAsked) => Vec::new(),
        Err(e) => vec![format!("amont stamps: {e}")],
        Ok(answer) if answer.gates.is_empty() => vec![format!(
            "amont stamps: no gate reported for {} on tree {}",
            asked.join(", "),
            answer.tree
        )],
        Ok(answer) => answer
            .gates
            .iter()
            .filter(|(_, state)| !state.vouched())
            .map(|(gate, state)| {
                format!(
                    "amont gate `{gate}` is {} on the verified tree: its verdict was not recorded, \
                     so the push would run it again",
                    state.as_str()
                )
            })
            .collect(),
    }
}

/// Whether a base can skip its baseline run: every profile gate is vouched
/// for on this platform AND the toolchain the baseline would run on is
/// identified (`toolchain_known`). Anything else runs the baseline as before.
///
/// A stamp carries no toolchain of its own (amont ADR-0015 keys it by tree
/// and gate identity), so relais cannot prove the stamp was earned on the
/// toolchain it would use now. It requires what the baseline cache requires
/// instead: a toolchain it can name. A repository that pins its toolchain in
/// its tree (`rust-toolchain.toml`) makes the toolchain a function of the
/// tree; one that does not takes the residual risk the plan names.
pub fn base_is_proven(
    gates: &[String],
    answer: &Result<Answer, StampsError>,
    toolchain_known: bool,
) -> bool {
    if gates.is_empty() || !toolchain_known {
        return false;
    }
    match answer {
        Ok(answer) => !answer.gates.is_empty() && answer.gates.values().all(|s| s.vouched()),
        Err(_) => false,
    }
}

/// Run `amont stamps --json [--tree <tree>] <gates…>` in `dir`, bounded and
/// cancellable.
pub fn amont_stamps(
    dir: &Path,
    tree: Option<&str>,
    gates: &[String],
    cancel: Option<&AtomicBool>,
) -> Result<Answer, StampsError> {
    amont_stamps_with(Path::new("amont"), dir, tree, gates, cancel)
}

/// [`amont_stamps`] with the binary named: the seam the tests use.
pub fn amont_stamps_with(
    amont: &Path,
    dir: &Path,
    tree: Option<&str>,
    gates: &[String],
    cancel: Option<&AtomicBool>,
) -> Result<Answer, StampsError> {
    let mut command = Command::new(amont);
    command.arg("stamps").arg("--json").current_dir(dir);
    if let Some(tree) = tree {
        command.arg("--tree").arg(tree);
    }
    command.args(gates);
    let end =
        crate::procs::run_with_timeout(command, STAMPS_TIMEOUT, None, cancel).map_err(|e| {
            StampsError::NotRun {
                detail: e.to_string(),
            }
        })?;
    if end.ended != Ended::Exited(0) {
        return Err(StampsError::Refused {
            ended: end.ended.describe(),
        });
    }
    parse(&end.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(gates: &[(&str, State)]) -> Result<Answer, StampsError> {
        Ok(Answer {
            tree: "t".into(),
            platform: "x86_64-macos".into(),
            gates: gates.iter().map(|(g, s)| ((*g).to_string(), *s)).collect(),
        })
    }

    fn names(gates: &[&str]) -> Vec<String> {
        gates.iter().map(|g| (*g).to_string()).collect()
    }

    /// amont's documented shape parses, and a state it never prints is
    /// unreadable rather than guessed.
    #[test]
    fn the_documented_answer_parses() {
        let parsed = parse(
            r#"{"tree":"abc","platform":"x86_64-macos","gates":{"pre-commit-cargo-test":"stamped","pre-push-cargo-test":"attested","pre-commit-clippy":"deferred","pre-commit-plugin-test":"absent"}}"#,
        )
        .expect("parses");
        assert_eq!(parsed.tree, "abc");
        assert_eq!(parsed.platform, "x86_64-macos");
        assert_eq!(parsed.gates["pre-commit-cargo-test"], State::Stamped);
        assert_eq!(parsed.gates["pre-push-cargo-test"], State::Attested);
        assert_eq!(parsed.gates["pre-commit-clippy"], State::Deferred);
        assert_eq!(parsed.gates["pre-commit-plugin-test"], State::Absent);
        for bad in [
            "",
            "not json",
            r#"{"tree":"a","gates":{}}"#,
            r#"{"tree":"a","platform":"p","gates":{"g":"maybe"}}"#,
        ] {
            assert!(
                matches!(parse(bad), Err(StampsError::Unreadable { .. })),
                "{bad:?}"
            );
        }
    }

    /// Only `amont run --stamp` commands name gates, each once.
    #[test]
    fn a_profile_names_its_stamp_gates() {
        let profile: VerificationProfile = toml::from_str(
            r#"
            [[commands]]
            argv = ["amont", "run", "--stamp", "cargo-test"]
            [[commands]]
            argv = ["make", "lint"]
            [[commands]]
            argv = ["amont", "run", "--stamp", "clippy", "cargo-test"]
            [[commands]]
            argv = ["amont", "run", "--stamp"]
            "#,
        )
        .expect("profile");
        assert_eq!(profile_gates(&profile), names(&["cargo-test", "clippy"]));
    }

    /// Absent and deferred gates are gaps; vouched ones are not.
    #[test]
    fn an_unrecorded_verdict_is_a_gap() {
        let asked = names(&["cargo-test"]);
        assert!(stamp_gaps(
            &asked,
            &answer(&[("pre-commit-cargo-test", State::Stamped)])
        )
        .is_empty());
        assert!(
            stamp_gaps(&asked, &answer(&[("pre-push-cargo-test", State::Attested)])).is_empty()
        );
        let gaps = stamp_gaps(
            &asked,
            &answer(&[
                ("pre-commit-cargo-test", State::Absent),
                ("pre-push-cargo-test", State::Deferred),
            ]),
        );
        assert_eq!(gaps.len(), 2, "{gaps:?}");
        assert!(gaps[0].contains("pre-commit-cargo-test") && gaps[0].contains("absent"));
        assert!(gaps[1].contains("pre-push-cargo-test") && gaps[1].contains("deferred"));
        // An answer naming no gate is no proof either.
        assert_eq!(stamp_gaps(&asked, &answer(&[])).len(), 1);
    }

    /// No answer is a gap naming why; nothing asked is no gap; a profile
    /// with no stamp command asks for nothing.
    #[test]
    fn no_answer_is_a_gap_and_nothing_asked_is_none() {
        let asked = names(&["cargo-test"]);
        let old = Err(StampsError::Refused {
            ended: "exit 2".into(),
        });
        let gaps = stamp_gaps(&asked, &old);
        assert_eq!(gaps.len(), 1);
        assert!(gaps[0].contains("1.66"), "{gaps:?}");
        assert!(stamp_gaps(&asked, &Err(StampsError::NotAsked)).is_empty());
        assert!(stamp_gaps(&[], &old).is_empty());
    }

    /// A base is proven only when every gate is vouched for and the
    /// toolchain is known.
    #[test]
    fn a_base_is_proven_only_when_every_gate_is_vouched_for() {
        let gates = names(&["cargo-test", "clippy"]);
        let all = answer(&[
            ("pre-commit-cargo-test", State::Stamped),
            ("pre-commit-clippy", State::Attested),
        ]);
        assert!(base_is_proven(&gates, &all, true));
        let one_absent = answer(&[
            ("pre-commit-cargo-test", State::Stamped),
            ("pre-commit-clippy", State::Absent),
        ]);
        assert!(!base_is_proven(&gates, &one_absent, true));
        assert!(
            !base_is_proven(&gates, &all, false),
            "unknown toolchain runs it"
        );
        assert!(!base_is_proven(&gates, &answer(&[]), true));
        assert!(!base_is_proven(&[], &all, true));
        assert!(!base_is_proven(
            &gates,
            &Err(StampsError::NotRun { detail: "x".into() }),
            true
        ));
    }

    /// relais's own profile names only gates its own `amont.conf` and amont's
    /// built-ins declare: a renamed gate would otherwise make every relais
    /// run on this repository a usage error at verification time. Skipped,
    /// saying so, where amont is not installed (CI's runners).
    #[test]
    fn this_repositorys_profile_names_real_amont_gates() {
        let policy =
            crate::policy::RepoPolicy::from_toml_str(include_str!("../../../../relais.toml"))
                .expect("this repository's relais.toml parses");
        let gates = profile_gates(&policy.verification.profiles["default"]);
        assert!(
            !gates.is_empty(),
            "the default profile runs its gates through amont"
        );
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let listed = Command::new("amont")
            .args(["list", "--json"])
            .current_dir(&root)
            .output();
        let Ok(out) = listed.as_ref().map(|o| (o.status.success(), &o.stdout)) else {
            eprintln!("skipped: amont is not installed here, so its gates cannot be listed");
            return;
        };
        if !out.0 {
            eprintln!("skipped: `amont list --json` did not exit 0 here");
            return;
        }
        let value: serde_json::Value = serde_json::from_slice(out.1).expect("amont list --json");
        let names: Vec<String> = value["checks"]
            .as_array()
            .expect("checks")
            .iter()
            .flat_map(|c| {
                ["id", "short_name"]
                    .into_iter()
                    .filter_map(|k| {
                        c.get(k)
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        // An amont before 1.58 lists no `short_name`; the id's tail after the
        // stage is the same name.
        let known = |gate: &str| {
            names.iter().any(|n| {
                n == gate
                    || n.strip_prefix("pre-commit-") == Some(gate)
                    || n.strip_prefix("pre-push-") == Some(gate)
            })
        };
        let unknown: Vec<&String> = gates.iter().filter(|g| !known(g)).collect();
        assert!(
            unknown.is_empty(),
            "profile gates amont does not list: {unknown:?}"
        );
    }

    #[cfg(unix)]
    fn fake_amont(dir: &Path, stdout: &str, code: i32) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join("fake-amont");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\necho \"$@\" > \"$0.args\"\ncat <<'EOF'\n{stdout}\nEOF\nexit {code}\n"
            ),
        )
        .expect("fake amont");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        crate::test_support::prime_script(&bin);
        bin
    }

    /// The probe passes the tree and the gates, and reads the answer.
    #[cfg(unix)]
    #[test]
    fn the_probe_asks_for_the_tree_and_gates() {
        let dir = crate::test_support::temp_dir("stamps-probe");
        let bin = fake_amont(
            &dir,
            r#"{"tree":"t1","platform":"p","gates":{"pre-commit-cargo-test":"stamped"}}"#,
            0,
        );
        let got = amont_stamps_with(&bin, &dir, Some("t1"), &names(&["cargo-test"]), None)
            .expect("answer");
        assert_eq!(got.gates["pre-commit-cargo-test"], State::Stamped);
        let args = std::fs::read_to_string(bin.with_extension("args")).expect("args");
        assert_eq!(args.trim(), "stamps --json --tree t1 cargo-test");
    }

    /// An amont with no `stamps` subcommand exits 2 on usage: refused, which
    /// is a gap, never a crash or a pass.
    #[cfg(unix)]
    #[test]
    fn an_old_amont_is_refused() {
        let dir = crate::test_support::temp_dir("stamps-old");
        let bin = fake_amont(&dir, "amont: unknown command \"stamps\"", 2);
        let got = amont_stamps_with(&bin, &dir, None, &names(&["cargo-test"]), None);
        assert!(matches!(got, Err(StampsError::Refused { .. })), "{got:?}");
        assert_eq!(stamp_gaps(&names(&["cargo-test"]), &got).len(), 1);
    }
}
