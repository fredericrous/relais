//! Guidance: the advisory half of what aval tells a worker (SPEC §7).
//!
//! Binding decisions (contract keys and mappings) live in the manifest's
//! `constraints` and can block a run. Guidance never does: the constraint
//! index, the full constraint statements and aval's relevance ranking are
//! fetched through the `DecisionResolver` port, kept here as typed values,
//! and every failure is a `GuidanceGap` the run continues past. Guidance
//! has its own byte budget and never counts toward a sizing problem.
//!
//! Everything in this module is a pure function over values, except
//! `run_aval`, the one bounded spawn.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// How much guidance a worker or planner prompt may carry: the index and
/// the relevant block together. Over it, the relevant block is dropped
/// first, then the index.
pub const GUIDANCE_BUDGET_BYTES: usize = 16 * 1024;

/// How much of the objective goes into aval's argv.
pub const OBJECTIVE_ARGV_BYTES: usize = 2048;

/// How many ranked entries aval is asked for.
pub const RELEVANT_TOP: usize = 5;

/// Which part of the guidance a gap is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuidanceSource {
    /// `aval rules`: the constraint index.
    Index,
    /// `aval relevant`: the relevance ranking.
    Relevant,
    /// The full statements the reviewer prompt carries.
    Reviewer,
}

/// Guidance that is missing, and why. Its own type: a guidance failure is
/// never a `ContextError`, which blocks a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuidanceGap {
    pub source: GuidanceSource,
    #[serde(default)]
    pub cause: GapCause,
    pub detail: String,
}

/// Why guidance is missing: one cause per gap, never a flag pair.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapCause {
    /// aval ran to this non-zero exit status.
    Exited(i32),
    /// aval timed out, was cancelled or died on a signal.
    #[default]
    NoAnswer,
    /// The `aval` binary could not be launched at all: asking it a second
    /// question would fail the same way.
    NotLaunched,
    /// The guidance was dropped to fit its byte budget.
    OverBudget { measured: usize, budget: usize },
    /// aval answered, and the answer was not the expected shape.
    Unparseable,
}

impl GuidanceGap {
    pub fn new(source: GuidanceSource, cause: GapCause, detail: impl Into<String>) -> Self {
        Self {
            source,
            cause,
            detail: detail.into(),
        }
    }

    /// Asking aval again would fail the same way.
    pub fn is_not_launched(&self) -> bool {
        self.cause == GapCause::NotLaunched
    }

    /// One phrase for the receipt: `relevant missing (aval exit 2)`.
    pub fn describe(&self) -> String {
        let what = match self.source {
            GuidanceSource::Index => "index missing",
            GuidanceSource::Relevant => "relevant missing",
            GuidanceSource::Reviewer => "reviewer constraints partial",
        };
        format!("{what} ({})", self.detail)
    }
}

/// The receipt line for a set of gaps; nothing when there is none.
pub fn gap_line(gaps: &[GuidanceGap]) -> Option<String> {
    if gaps.is_empty() {
        return None;
    }
    let phrases: Vec<String> = gaps.iter().map(GuidanceGap::describe).collect();
    Some(format!("guidance: {}", phrases.join("; ")))
}

/// One rule as aval states it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleRef {
    pub id: String,
    #[serde(default)]
    pub level: String,
    pub statement: String,
}

/// What `aval rules` left out of the list, as aval counts it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Omitted {
    #[serde(default)]
    pub constraints: u64,
    #[serde(default)]
    pub heuristics: u64,
    #[serde(default)]
    pub traits: Vec<String>,
}

/// The active constraint rules of the repository.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rules {
    pub rules: Vec<RuleRef>,
    pub omitted: Omitted,
}

/// A ranked key's state as aval reports it; anything but the two known
/// states is kept as aval wrote it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum RankState {
    Active,
    Undecided,
    Other(String),
}

impl From<String> for RankState {
    fn from(state: String) -> Self {
        match state.as_str() {
            "active" => Self::Active,
            "undecided" => Self::Undecided,
            _ => Self::Other(state),
        }
    }
}

impl From<RankState> for String {
    fn from(state: RankState) -> Self {
        match state {
            RankState::Active => "active".to_string(),
            RankState::Undecided => "undecided".to_string(),
            RankState::Other(other) => other,
        }
    }
}

/// One decision key aval ranked, with its state as aval reported it. A
/// ranking is a suggestion: a key that is not active is recorded and
/// never blocks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RankedKey {
    pub key: String,
    pub state: RankState,
    #[serde(default)]
    pub adr: Option<String>,
    #[serde(default)]
    pub choice: Option<String>,
}

/// What `aval relevant` ranked for a query.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Relevant {
    pub keys: Vec<RankedKey>,
    pub rules: Vec<RuleRef>,
}

/// What a run asks aval to rank against.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RelevantQuery {
    pub paths: Vec<String>,
    pub text: String,
}

/// The guidance recorded in the manifest: evidence of what the prompts
/// were built from. Old manifests lack it and read as empty.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Guidance {
    #[serde(default)]
    pub aval_version: Option<String>,
    /// The ids the worker's constraint index carries; empty when aval
    /// gave none or the budget dropped it.
    #[serde(default)]
    pub index: Vec<String>,
    /// Every active constraint rule with its statement: what the reviewer
    /// is shown in place of the index, since it cannot run aval.
    #[serde(default)]
    pub rules: Vec<RuleRef>,
    #[serde(default)]
    pub omitted: Omitted,
    /// The ranked keys, with their state, that are not already binding.
    #[serde(default)]
    pub relevant_keys: Vec<RankedKey>,
    #[serde(default)]
    pub relevant_rules: Vec<RuleRef>,
    #[serde(default)]
    pub gaps: Vec<GuidanceGap>,
}

impl Guidance {
    /// The index as the prompt prints it: full ids, grouped by prefix.
    pub fn index_lines(&self) -> Vec<String> {
        group_ids(self.index.iter().map(String::as_str))
    }

    /// The relevant block's entries, one per active decision and rule.
    /// `skip_rules` names rule ids already shown in full elsewhere.
    pub fn relevant_entries(&self, skip_rules: &[&str]) -> Vec<String> {
        let keys = self
            .relevant_keys
            .iter()
            .filter(|ranked| ranked.state == RankState::Active)
            .filter_map(|ranked| {
                let choice = ranked.choice.as_deref()?;
                Some(match &ranked.adr {
                    Some(adr) => format!("`{}` ({adr}): {choice}", ranked.key),
                    None => format!("`{}`: {choice}", ranked.key),
                })
            });
        let rules = self
            .relevant_rules
            .iter()
            .filter(|rule| !skip_rules.contains(&rule.id.as_str()))
            .map(|rule| format!("rule `{}` ({}): {}", rule.id, rule.level, rule.statement));
        keys.chain(rules).collect()
    }

    /// Record the gaps of a fetch.
    pub fn record(&mut self, gap: GuidanceGap) {
        self.gaps.push(gap);
    }
}

/// Bytes a list of lines takes in a prompt.
pub fn rendered_bytes(lines: &[String]) -> usize {
    lines.iter().map(|line| line.len() + 1).sum()
}

/// Keep the guidance inside `budget`: the relevant block goes first, then
/// the index. Each drop is a gap with the measured and the budget bytes.
/// The reviewer's statements are not counted here.
pub fn fit_budget(guidance: &mut Guidance, budget: usize) {
    let index = rendered_bytes(&guidance.index_lines());
    let relevant = rendered_bytes(&guidance.relevant_entries(&[]));
    if index + relevant > budget && relevant > 0 {
        guidance.relevant_keys.clear();
        guidance.relevant_rules.clear();
        guidance.record(GuidanceGap::new(
            GuidanceSource::Relevant,
            GapCause::OverBudget {
                measured: index + relevant,
                budget,
            },
            format!(
                "dropped over budget: guidance measured {} bytes against a {budget}-byte budget",
                index + relevant
            ),
        ));
    }
    if index > budget {
        guidance.index.clear();
        guidance.record(GuidanceGap::new(
            GuidanceSource::Index,
            GapCause::OverBudget {
                measured: index,
                budget,
            },
            format!(
                "dropped over budget: index measured {index} bytes against a {budget}-byte budget"
            ),
        ));
    }
}

/// `prefix: id, id` lines, ids in full, prefixes and ids sorted. The
/// prefix is the id up to its first dot.
pub fn group_ids<'a>(ids: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut groups: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for id in ids {
        let prefix = id.split('.').next().unwrap_or(id);
        groups.entry(prefix).or_default().insert(id);
    }
    groups
        .into_iter()
        .map(|(prefix, ids)| {
            format!(
                "{prefix}: {}",
                ids.into_iter().collect::<Vec<_>>().join(", ")
            )
        })
        .collect()
}

/// The index lines of a rule list.
pub fn index_lines(rules: &[RuleRef]) -> Vec<String> {
    group_ids(rules.iter().map(|rule| rule.id.as_str()))
}

#[derive(Deserialize)]
struct RawRule {
    id: String,
    #[serde(default)]
    level: Option<String>,
    statement: String,
    #[serde(default = "active_by_default")]
    active: bool,
}

fn active_by_default() -> bool {
    true
}

impl RawRule {
    fn into_rule(self) -> RuleRef {
        RuleRef {
            id: self.id,
            level: self.level.unwrap_or_else(|| "constraint".to_string()),
            statement: self.statement,
        }
    }
}

#[derive(Deserialize)]
struct RawRules {
    rules: Vec<RawRule>,
    #[serde(default)]
    omitted: Omitted,
}

/// Parse `aval rules --level constraint --json` (aval 1.9.0): the active
/// rules and what aval omitted. Any other shape is a gap.
pub fn parse_rules_output(stdout: &str) -> Result<Rules, GuidanceGap> {
    let raw: RawRules = serde_json::from_str(stdout.trim()).map_err(|e| {
        GuidanceGap::new(
            GuidanceSource::Index,
            GapCause::Unparseable,
            format!("unparseable aval rules output: {e}"),
        )
    })?;
    Ok(Rules {
        rules: raw
            .rules
            .into_iter()
            .filter(|rule| rule.active)
            .map(RawRule::into_rule)
            .collect(),
        omitted: raw.omitted,
    })
}

#[derive(Deserialize)]
struct RawKey {
    key: String,
    state: String,
    #[serde(default)]
    adr: Option<String>,
    #[serde(default)]
    choice: Option<String>,
}

#[derive(Deserialize)]
struct RawRelevant {
    #[serde(default)]
    keys: Option<Vec<RawKey>>,
    #[serde(default)]
    rules: Option<Vec<RawRule>>,
}

/// Parse `aval relevant … --json` (aval 1.9.0): the ranked keys with their
/// state and the ranked rules. An output with neither list is a gap.
pub fn parse_relevant_output(stdout: &str) -> Result<Relevant, GuidanceGap> {
    let gap =
        |detail: String| GuidanceGap::new(GuidanceSource::Relevant, GapCause::Unparseable, detail);
    let raw: RawRelevant = serde_json::from_str(stdout.trim())
        .map_err(|e| gap(format!("unparseable aval relevant output: {e}")))?;
    if raw.keys.is_none() && raw.rules.is_none() {
        return Err(gap(
            "aval relevant output carries neither keys nor rules".to_string()
        ));
    }
    Ok(Relevant {
        keys: raw
            .keys
            .unwrap_or_default()
            .into_iter()
            .map(|key| RankedKey {
                key: key.key,
                state: RankState::from(key.state),
                adr: key.adr,
                choice: key.choice,
            })
            .collect(),
        rules: raw
            .rules
            .unwrap_or_default()
            .into_iter()
            .map(RawRule::into_rule)
            .collect(),
    })
}

/// The longest prefix of `text` within `max` bytes that ends on a char
/// boundary.
pub fn truncate_on_char_boundary(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut cut = max;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    &text[..cut]
}

/// A write-scope glob or read hint reduced to the literal directory
/// prefix aval can match: `a/b/**/*.rs` is `a/b`. A glob character in the
/// first segment leaves nothing to match (`**`, `{a,b}/**`, `*/x`); a path
/// without one is kept as it is.
pub fn glob_to_path(glob: &str) -> Option<String> {
    let literal: Vec<&str> = glob
        .trim()
        .split('/')
        .filter(|segment| !segment.is_empty())
        .take_while(|segment| !segment.contains(['*', '?', '[', ']', '{', '}']))
        .collect();
    if literal.is_empty() {
        None
    } else {
        Some(literal.join("/"))
    }
}

/// The paths a query carries: each scope glob reduced, plus the read
/// hints, in order and without repeats.
pub fn query_paths(scope: &[String], read_hints: &[String]) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    for path in scope
        .iter()
        .chain(read_hints.iter())
        .filter_map(|glob| glob_to_path(glob))
    {
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths
}

/// The argv of `aval relevant`: joined `--path=`/`--text=` forms, so a
/// value that starts with `-` is a value and never a flag; the objective
/// is cut to `OBJECTIVE_ARGV_BYTES` on a char boundary.
pub fn relevant_args(query: &RelevantQuery) -> Vec<String> {
    let mut args = vec!["relevant".to_string()];
    args.extend(query.paths.iter().map(|path| format!("--path={path}")));
    args.push(format!(
        "--text={}",
        truncate_on_char_boundary(&query.text, OBJECTIVE_ARGV_BYTES)
    ));
    args.push("--top".to_string());
    args.push(RELEVANT_TOP.to_string());
    args.push("--json".to_string());
    args
}

/// The argv of `aval rules` for the constraint index.
pub fn rules_args() -> Vec<String> {
    ["rules", "--level", "constraint", "--json"]
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// Spawn `program args…` in `dir`, bounded and cancellable. Success is
/// stdout; every other end is a gap of `source`.
pub fn run_aval(
    program: &str,
    args: &[String],
    dir: &Path,
    source: GuidanceSource,
    timeout: Duration,
    cancel: Option<&AtomicBool>,
) -> Result<String, GuidanceGap> {
    let mut command = std::process::Command::new(program);
    command.args(args).current_dir(dir);
    match crate::procs::run_with_timeout(command, timeout, None, cancel) {
        Ok(end) => match end.ended {
            crate::procs::Ended::Exited(0) => Ok(end.stdout),
            crate::procs::Ended::Exited(code) => Err(GuidanceGap::new(
                source,
                GapCause::Exited(code),
                format!("aval exit {code}"),
            )),
            ended @ (crate::procs::Ended::TimedOut
            | crate::procs::Ended::Cancelled
            | crate::procs::Ended::Signalled) => Err(GuidanceGap::new(
                source,
                GapCause::NoAnswer,
                format!("aval did not answer: {}", ended.describe()),
            )),
        },
        Err(e) => Err(GuidanceGap::new(
            source,
            GapCause::NotLaunched,
            format!("aval could not be launched: {e}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from aval 1.9.0 (`aval rules --level constraint --json`),
    /// shortened to three rules; one is inactive.
    const RULES_JSON: &str = r#"{"omitted":{"constraints":11,"heuristics":0,"traits":["cli"]},
      "rules":[
      {"active":true,"adopts":"decisions:ADR-0015","file":".adr/packs/decisions.pack","id":"errors.never-swallowed","level":"constraint","pack":"decisions","source":"x","statement":"An error is handled or passed on, never swallowed."},
      {"active":true,"adopts":"decisions:ADR-0015","id":"errors.typed-values","level":"constraint","statement":"Failures are typed values."},
      {"active":false,"id":"functions.retired","level":"constraint","statement":"Not in force."},
      {"active":true,"id":"functions.no-flag-arguments","level":"constraint","statement":"No flag arguments."}]}"#;

    /// Captured from aval 1.9.0 (`aval relevant --path=… --json`).
    const RELEVANT_JSON: &str = r#"{"advisory":true,"kind":"suggestion","ranked":3,
      "keys":[
        {"adr":"ADR-0001","choice":"relais routes the model of the person's own session","key":"routing.session-model","state":"active","score":3.1},
        {"adr":"ADR-0009","key":"storage.engine","state":"undecided","score":2.0}],
      "rules":[
        {"adopts":"decisions:ADR-0015","id":"effects.at-the-boundary","level":"constraint","score":6.4,"statement":"Effects are pushed to the boundary."}]}"#;

    #[test]
    fn rules_json_parses_to_the_active_rules_and_the_omitted_counts() {
        let rules = parse_rules_output(RULES_JSON).expect("parses");
        let ids: Vec<&str> = rules.rules.iter().map(|rule| rule.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "errors.never-swallowed",
                "errors.typed-values",
                "functions.no-flag-arguments"
            ]
        );
        assert_eq!(rules.rules[0].level, "constraint");
        assert_eq!(rules.omitted.constraints, 11);
        assert_eq!(rules.omitted.traits, ["cli"]);
    }

    #[test]
    fn relevant_json_keeps_the_active_key_the_undecided_key_and_the_rules() {
        let relevant = parse_relevant_output(RELEVANT_JSON).expect("parses");
        assert_eq!(relevant.keys.len(), 2);
        assert_eq!(relevant.keys[0].state, RankState::Active);
        assert_eq!(relevant.keys[0].adr.as_deref(), Some("ADR-0001"));
        assert_eq!(relevant.keys[1].state, RankState::Undecided);
        assert_eq!(relevant.keys[1].choice, None);
        assert_eq!(relevant.rules[0].id, "effects.at-the-boundary");
        assert_eq!(relevant.rules[0].level, "constraint");
    }

    #[test]
    fn malformed_output_is_a_gap_naming_its_source() {
        for bad in ["", "not json", "[]", r#"{"rules":"nope"}"#] {
            let gap = parse_rules_output(bad).expect_err(bad);
            assert_eq!(gap.source, GuidanceSource::Index, "{bad}");
        }
        for bad in ["", "not json", "[]", "{}", r#"{"keys":[{"key":1}]}"#] {
            let gap = parse_relevant_output(bad).expect_err(bad);
            assert_eq!(gap.source, GuidanceSource::Relevant, "{bad}");
        }
    }

    #[test]
    fn index_lines_carry_full_ids_grouped_by_prefix() {
        let rules = parse_rules_output(RULES_JSON).expect("parses").rules;
        let lines = index_lines(&rules);
        assert_eq!(
            lines,
            [
                "errors: errors.never-swallowed, errors.typed-values",
                "functions: functions.no-flag-arguments"
            ]
        );
        // Every id token is exactly an id aval returned.
        let known: Vec<&str> = rules.iter().map(|rule| rule.id.as_str()).collect();
        for line in &lines {
            let (_, ids) = line.split_once(": ").expect("prefix: ids");
            for id in ids.split(", ") {
                assert!(known.contains(&id), "{id}");
            }
        }
    }

    #[test]
    fn a_glob_is_reduced_to_its_literal_directory() {
        assert_eq!(glob_to_path("a/b/**/*.rs").as_deref(), Some("a/b"));
        assert_eq!(
            glob_to_path("crates/relais/src/**").as_deref(),
            Some("crates/relais/src")
        );
        assert_eq!(glob_to_path("**"), None);
        assert_eq!(glob_to_path("{a,b}/**"), None);
        assert_eq!(glob_to_path("*/x"), None);
        assert_eq!(
            glob_to_path("docs/SPEC.md").as_deref(),
            Some("docs/SPEC.md")
        );
        assert_eq!(
            query_paths(
                &["a/b/**".to_string(), "**".to_string()],
                &["a/b".to_string(), "README.md".to_string()]
            ),
            ["a/b", "README.md"]
        );
    }

    #[test]
    fn argv_uses_the_joined_form_for_flag_looking_values() {
        let args = relevant_args(&RelevantQuery {
            paths: vec!["-rf".to_string()],
            text: "--json is the objective".to_string(),
        });
        assert!(args.contains(&"--path=-rf".to_string()), "{args:?}");
        assert!(
            args.contains(&"--text=--json is the objective".to_string()),
            "{args:?}"
        );
        assert!(!args.contains(&"-rf".to_string()));
        assert_eq!(args.last().map(String::as_str), Some("--json"));
    }

    #[test]
    fn an_oversize_objective_is_cut_to_2_kib_on_a_char_boundary() {
        // 'é' is two bytes: 2048 is even, so shift by one ASCII byte to
        // put the limit in the middle of a character.
        let text = format!("a{}", "é".repeat(2000));
        let args = relevant_args(&RelevantQuery {
            paths: vec![],
            text,
        });
        let value = args
            .iter()
            .find_map(|arg| arg.strip_prefix("--text="))
            .expect("text");
        assert!(value.len() <= OBJECTIVE_ARGV_BYTES);
        assert_eq!(value.len(), OBJECTIVE_ARGV_BYTES - 1);
        assert!(value.ends_with('é'));
    }

    fn sh(script: &str, source: GuidanceSource, timeout: Duration) -> Result<String, GuidanceGap> {
        run_aval(
            "sh",
            &["-c".to_string(), script.to_string()],
            Path::new("."),
            source,
            timeout,
            None,
        )
    }

    #[test]
    fn every_way_a_spawn_fails_is_a_gap() {
        let five = Duration::from_secs(5);
        assert_eq!(
            sh("echo hi", GuidanceSource::Index, five).as_deref(),
            Ok("hi\n")
        );

        let exit1 = sh("exit 1", GuidanceSource::Index, five).unwrap_err();
        assert_eq!(exit1.cause, GapCause::Exited(1));
        let exit2 = sh("exit 2", GuidanceSource::Relevant, five).unwrap_err();
        assert_eq!(exit2.cause, GapCause::Exited(2));
        assert_eq!(exit2.describe(), "relevant missing (aval exit 2)");

        let slow = sh(
            "sleep 5",
            GuidanceSource::Relevant,
            Duration::from_millis(100),
        )
        .unwrap_err();
        assert_eq!(slow.cause, GapCause::NoAnswer);
        assert!(slow.detail.contains("timed out"), "{}", slow.detail);

        let missing = run_aval(
            "relais-no-such-binary-4f3a",
            &[],
            Path::new("."),
            GuidanceSource::Index,
            five,
            None,
        )
        .unwrap_err();
        assert_eq!(missing.cause, GapCause::NotLaunched, "{missing:?}");
    }

    #[test]
    fn a_cancelled_spawn_returns_a_gap_well_before_the_timeout() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let cancel = AtomicBool::new(false);
        let timeout = Duration::from_secs(30);
        let started = std::time::Instant::now();
        let result = std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(200));
                cancel.store(true, Ordering::SeqCst);
            });
            run_aval(
                "sleep",
                &["30".to_string()],
                Path::new("."),
                GuidanceSource::Relevant,
                timeout,
                Some(&cancel),
            )
        });
        let gap = result.expect_err("a cancelled spawn is a gap");
        assert_eq!(gap.cause, GapCause::NoAnswer);
        assert_eq!(gap.source, GuidanceSource::Relevant);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
        assert!(started.elapsed() < timeout);
    }

    #[test]
    fn an_unknown_rank_state_is_kept_as_aval_wrote_it() {
        assert_eq!(
            RankState::from("retired".to_string()),
            RankState::Other("retired".into())
        );
        assert_eq!(String::from(RankState::Other("retired".into())), "retired");
    }

    fn guidance_with(index: usize, relevant: usize) -> Guidance {
        Guidance {
            index: (0..index)
                .map(|i| format!("group{i}.rule-{i:04}"))
                .collect(),
            relevant_rules: (0..relevant)
                .map(|i| RuleRef {
                    id: format!("rel.rule-{i}"),
                    level: "constraint".into(),
                    statement: "s".repeat(100),
                })
                .collect(),
            ..Guidance::default()
        }
    }

    #[test]
    fn over_budget_drops_relevant_first_then_the_index() {
        let mut both = guidance_with(10, 50);
        let index_bytes = rendered_bytes(&both.index_lines());
        fit_budget(&mut both, index_bytes + 10);
        assert!(both.relevant_rules.is_empty());
        assert_eq!(both.index.len(), 10, "the index survives while it fits");
        assert_eq!(both.gaps.len(), 1);
        assert_eq!(both.gaps[0].source, GuidanceSource::Relevant);
        assert!(
            both.gaps[0].detail.contains("bytes against a"),
            "{:?}",
            both.gaps
        );

        let mut tiny = guidance_with(10, 50);
        fit_budget(&mut tiny, 8);
        assert!(tiny.index.is_empty() && tiny.relevant_rules.is_empty());
        let sources: Vec<GuidanceSource> = tiny.gaps.iter().map(|gap| gap.source).collect();
        assert_eq!(sources, [GuidanceSource::Relevant, GuidanceSource::Index]);

        let mut fits = guidance_with(3, 2);
        fit_budget(&mut fits, GUIDANCE_BUDGET_BYTES);
        assert!(fits.gaps.is_empty());
    }

    #[test]
    fn the_receipt_line_exists_only_for_a_gap() {
        assert_eq!(gap_line(&[]), None);
        let gap = GuidanceGap::new(GuidanceSource::Relevant, GapCause::Exited(2), "aval exit 2");
        assert_eq!(
            gap_line(&[gap]).as_deref(),
            Some("guidance: relevant missing (aval exit 2)")
        );
    }

    #[test]
    fn a_guidance_less_manifest_value_reads_as_empty() {
        let parsed: Guidance = serde_json::from_str("{}").expect("parses");
        assert_eq!(parsed, Guidance::default());
    }
}
