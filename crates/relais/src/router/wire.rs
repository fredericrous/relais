//! `relais native router-observe`'s payload (docs/router-protocol.md):
//! parsed and validated whole, before anything is written, so a bad record
//! drops the batch and names itself. Unknown fields are refused. Pure.

use serde::de::DeserializeOwned;
use serde::Deserialize;

use super::capture::{ClassifierInput, INPUT_MAX_BYTES};
use super::outcome::TaskOutcome;
use crate::ledger::{
    RouterCheck, RouterClass, RouterClassificationRow, RouterDecisionRow, RouterReassessRow,
    RouterRecord, RouterTaskOutcomeRow, RouterTaskRow, RouterUsageRow,
};

/// The wire schema this relais reads.
pub const WIRE_SCHEMA: u32 = 1;

/// A validated batch: the session and its records, ready to write.
#[derive(Debug, Clone, PartialEq)]
pub struct Batch {
    pub session: String,
    pub records: Vec<RouterRecord>,
    /// The text of each `classification` record, which the ledger does not
    /// hold (only its hash): for the local day file, if capture is on.
    pub inputs: Vec<ClassifierInput>,
}

/// Why a payload was refused: the record (by index and kind) and what is
/// wrong with it, or the envelope itself.
#[derive(Debug, Clone, PartialEq)]
pub struct WireError {
    /// `None`: the envelope (`schema`, `session`, `records`).
    pub record: Option<(usize, String)>,
    pub detail: String,
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.record {
            None => write!(f, "payload: {}", self.detail),
            Some((index, kind)) => write!(f, "records[{index}] ({kind}): {}", self.detail),
        }
    }
}

impl std::error::Error for WireError {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    schema: u32,
    session: String,
    records: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Tier {
    Research,
    Implementation,
    Escalation,
}

impl Tier {
    fn as_str(self) -> &'static str {
        match self {
            Tier::Research => "research",
            Tier::Implementation => "implementation",
            Tier::Escalation => "escalation",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Question,
    Edit,
    Debug,
    Design,
    Review,
}

#[derive(Debug, Clone, Copy, Deserialize)]
enum Scope {
    #[serde(rename = "local")]
    Local,
    #[serde(rename = "module")]
    Module,
    #[serde(rename = "cross-cutting")]
    CrossCutting,
    #[serde(rename = "unknown")]
    Unknown,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Uncertainty {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Class {
    kind: Kind,
    difficulty: u8,
    scope: Scope,
    uncertainty: Uncertainty,
    verifiable: bool,
    confidence: f64,
}

impl Class {
    fn row(&self) -> Result<RouterClass, String> {
        if !(1..=5).contains(&self.difficulty) {
            return Err(format!(
                "class.difficulty {} is not 1 to 5",
                self.difficulty
            ));
        }
        unit("class.confidence", Some(self.confidence))?;
        Ok(RouterClass {
            kind: match self.kind {
                Kind::Question => "question",
                Kind::Edit => "edit",
                Kind::Debug => "debug",
                Kind::Design => "design",
                Kind::Review => "review",
            }
            .into(),
            difficulty: self.difficulty,
            scope: match self.scope {
                Scope::Local => "local",
                Scope::Module => "module",
                Scope::CrossCutting => "cross-cutting",
                Scope::Unknown => "unknown",
            }
            .into(),
            uncertainty: match self.uncertainty {
                Uncertainty::Low => "low",
                Uncertainty::Medium => "medium",
                Uncertainty::High => "high",
            }
            .into(),
            verifiable: self.verifiable,
            confidence: self.confidence,
        })
    }
}

/// A closed vocabulary read as a string and checked against its list, so
/// the stored spelling is the wire's.
fn one_of(field: &str, value: &str, allowed: &[&str]) -> Result<String, String> {
    if allowed.contains(&value) {
        Ok(value.to_string())
    } else {
        Err(format!(
            "{field} `{value}` is not one of {}",
            allowed.join("|")
        ))
    }
}

fn unit(field: &str, value: Option<f64>) -> Result<(), String> {
    match value {
        Some(v) if !(v.is_finite() && (0.0..=1.0).contains(&v)) => {
            Err(format!("{field} {v} is not between 0 and 1"))
        }
        _ => Ok(()),
    }
}

fn timestamp(field: &str, value: &str) -> Result<(), String> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|_| ())
        .map_err(|e| format!("{field} `{value}` is not RFC 3339: {e}"))
}

/// File hashes as the contract has them: the first 16 hex digits of the
/// sha256 of a repo-relative path, lowercase. Returned sorted, once each.
fn file_hashes(files: Vec<String>) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::with_capacity(files.len());
    for hash in files {
        let valid = hash.len() == 16
            && hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if !valid {
            return Err(format!("files `{hash}` is not 16 lowercase hex digits"));
        }
        if !out.contains(&hash) {
            out.push(hash);
        }
    }
    out.sort();
    Ok(out)
}

fn named(field: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Err(format!("{field} is empty"))
    } else {
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Decision {
    task_id: String,
    turn_id: Option<String>,
    agent_id: Option<String>,
    relation: String,
    class: Option<Class>,
    tier: Tier,
    model: String,
    effort: Option<String>,
    reason: String,
    mode_effective: String,
    holdout: bool,
    applied: bool,
    explored: bool,
    propensity: Option<f64>,
    draw: Option<f64>,
    would_pass_gate: bool,
    #[serde(default)]
    table_version: Option<u32>,
    at: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Usage {
    task_id: String,
    turn_id: Option<String>,
    step: u32,
    agent_id: Option<String>,
    source: String,
    model: String,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_input_tokens: u64,
    cache_creation_input_tokens: u64,
    at: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reassess {
    task_id: String,
    agent_id: Option<String>,
    event: String,
    tier_from: Tier,
    tier_to: Tier,
    effort_to: Option<String>,
    escalating: bool,
    #[serde(default)]
    files: Vec<String>,
    at: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Task {
    task_id: String,
    agent_id: Option<String>,
    started_at: String,
    ended_at: Option<String>,
    class: Option<Class>,
    outcome: TaskOutcome,
    #[serde(default)]
    completed_at_end: Option<TaskOutcome>,
    #[serde(default)]
    inferred: Vec<String>,
    #[serde(default)]
    files: Vec<String>,
    escalations: u32,
    exhausted: bool,
    turns: u32,
    explicit_quote: Option<String>,
    #[serde(default)]
    checks: Vec<Check>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Check {
    name: String,
    kind: String,
    passed: bool,
}

/// A parent's outcome for a subagent task that ended before it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskOutcomeRecord {
    task_id: String,
    agent_id: Option<String>,
    outcome: TaskOutcome,
    at: String,
}

/// One classifier call (Phase 5 of the learning loop). The answer is the
/// plugin's parse of the reply, every field; absent when it did not parse.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Classification {
    call_id: String,
    purpose: String,
    model: String,
    prompt_version: String,
    input: String,
    #[serde(default)]
    answer: Option<Answer>,
    parse_ok: bool,
    #[serde(default)]
    latency_ms: Option<u64>,
    at: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Answer {
    relation: String,
    kind: String,
    difficulty: u8,
    scope: String,
    uncertainty: String,
    verifiable: bool,
    #[serde(default)]
    explicit: Explicit,
    #[serde(default)]
    user_model: Option<String>,
    #[serde(default)]
    user_model_for: Option<String>,
    confidence: f64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Explicit {
    value: String,
    #[serde(default)]
    quote: Option<String>,
}

impl Default for Explicit {
    fn default() -> Self {
        Self {
            value: "none".into(),
            quote: None,
        }
    }
}

/// The answer as stored (the person's quoted words are text: they go to the
/// day file, not the ledger) and the quote.
fn answer_json(answer: &Answer) -> Result<(String, Option<String>), String> {
    let relation = one_of(
        "answer.relation",
        &answer.relation,
        &["new_task", "continuation", "correction"],
    )?;
    let kind = one_of(
        "answer.kind",
        &answer.kind,
        &["question", "edit", "debug", "design", "review"],
    )?;
    if !(1..=5).contains(&answer.difficulty) {
        return Err(format!(
            "answer.difficulty {} is not 1 to 5",
            answer.difficulty
        ));
    }
    let scope = one_of(
        "answer.scope",
        &answer.scope,
        &["local", "module", "cross-cutting", "unknown"],
    )?;
    let uncertainty = one_of(
        "answer.uncertainty",
        &answer.uncertainty,
        &["low", "medium", "high"],
    )?;
    let explicit = one_of(
        "answer.explicit.value",
        &answer.explicit.value,
        &["accept", "correct", "none"],
    )?;
    if let Some(who) = &answer.user_model_for {
        one_of("answer.user_model_for", who, &["session", "subagent"])?;
    }
    unit("answer.confidence", Some(answer.confidence))?;
    let json = serde_json::json!({
        "relation": relation,
        "kind": kind,
        "difficulty": answer.difficulty,
        "scope": scope,
        "uncertainty": uncertainty,
        "verifiable": answer.verifiable,
        "explicit": { "value": explicit },
        "user_model": answer.user_model,
        "user_model_for": answer.user_model_for,
        "confidence": answer.confidence,
    });
    Ok((json.to_string(), answer.explicit.quote.clone()))
}

fn classification(
    session: &str,
    c: Classification,
) -> Result<(RouterRecord, ClassifierInput), String> {
    named("call_id", &c.call_id)?;
    named("model", &c.model)?;
    timestamp("at", &c.at)?;
    one_of("purpose", &c.purpose, &["prompt", "spawn"])?;
    let version_ok = c.prompt_version.len() == 64
        && c.prompt_version
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !version_ok {
        return Err(format!(
            "prompt_version `{}` is not 64 lowercase hex digits",
            c.prompt_version
        ));
    }
    if c.input.len() > INPUT_MAX_BYTES {
        return Err(format!(
            "input is {} bytes; at most {INPUT_MAX_BYTES}",
            c.input.len()
        ));
    }
    let (answer_json, quote) = match &c.answer {
        Some(answer) => {
            let (json, quote) = answer_json(answer)?;
            (Some(json), quote)
        }
        None => (None, None),
    };
    if c.parse_ok && answer_json.is_none() {
        return Err("parse_ok is true but there is no answer".into());
    }
    if !c.parse_ok && answer_json.is_some() {
        return Err("parse_ok is false but there is an answer".into());
    }
    let row = RouterClassificationRow {
        input_sha256: crate::ids::sha256_hex(c.input.as_bytes()),
        call_id: c.call_id.clone(),
        purpose: c.purpose,
        model: c.model,
        prompt_version: c.prompt_version,
        answer_json,
        parse_ok: c.parse_ok,
        latency_ms: c.latency_ms,
        at: c.at.clone(),
    };
    let input = ClassifierInput {
        session: session.to_string(),
        call_id: c.call_id,
        at: c.at,
        input: c.input,
        quote,
    };
    Ok((RouterRecord::Classification(row), input))
}

fn decision(d: Decision) -> Result<RouterRecord, String> {
    named("task_id", &d.task_id)?;
    named("model", &d.model)?;
    timestamp("at", &d.at)?;
    unit("propensity", d.propensity)?;
    unit("draw", d.draw)?;
    Ok(RouterRecord::Decision(RouterDecisionRow {
        relation: one_of(
            "relation",
            &d.relation,
            &["new_task", "continuation", "correction", "subagent"],
        )?,
        class: d.class.as_ref().map(Class::row).transpose()?,
        reason: one_of(
            "reason",
            &d.reason,
            &[
                "table",
                "recovery",
                "pin",
                "user_model",
                "explicit_kept",
                "timeout_kept",
                "abstained",
                "cache_gate",
            ],
        )?,
        mode_effective: one_of(
            "mode_effective",
            &d.mode_effective,
            &["on", "shadow", "off"],
        )?,
        task_id: d.task_id,
        turn_id: d.turn_id,
        agent_id: d.agent_id,
        tier: d.tier.as_str().into(),
        model: d.model,
        effort: d.effort,
        holdout: d.holdout,
        applied: d.applied,
        explored: d.explored,
        propensity: d.propensity,
        draw: d.draw,
        would_pass_gate: d.would_pass_gate,
        table_version: d.table_version,
        at: d.at,
    }))
}

fn usage(u: Usage) -> Result<RouterRecord, String> {
    named("task_id", &u.task_id)?;
    named("model", &u.model)?;
    timestamp("at", &u.at)?;
    Ok(RouterRecord::Usage(RouterUsageRow {
        source: one_of("source", &u.source, &["step", "classifier"])?,
        task_id: u.task_id,
        turn_id: u.turn_id,
        step: u.step,
        agent_id: u.agent_id,
        model: u.model,
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
        cache_read_input_tokens: u.cache_read_input_tokens,
        cache_creation_input_tokens: u.cache_creation_input_tokens,
        at: u.at,
    }))
}

fn reassess(r: Reassess) -> Result<RouterRecord, String> {
    named("task_id", &r.task_id)?;
    timestamp("at", &r.at)?;
    Ok(RouterRecord::Reassess(RouterReassessRow {
        event: one_of(
            "event",
            &r.event,
            &[
                "failed_verification",
                "repair_failed",
                "scope_growth",
                "spawn",
                "correction",
                "flag",
                "revert",
            ],
        )?,
        files: file_hashes(r.files)?,
        task_id: r.task_id,
        agent_id: r.agent_id,
        tier_from: r.tier_from.as_str().into(),
        tier_to: r.tier_to.as_str().into(),
        effort_to: r.effort_to,
        escalating: r.escalating,
        at: r.at,
    }))
}

fn task(t: Task) -> Result<RouterRecord, String> {
    named("task_id", &t.task_id)?;
    timestamp("started_at", &t.started_at)?;
    if let Some(ended) = &t.ended_at {
        timestamp("ended_at", ended)?;
    }
    let mut checks = Vec::with_capacity(t.checks.len());
    for check in t.checks {
        named("checks.name", &check.name)?;
        checks.push(RouterCheck {
            kind: one_of(
                "checks.kind",
                &check.kind,
                &["test", "lint", "build", "gate"],
            )?,
            name: check.name,
            passed: check.passed,
        });
    }
    let mut inferred = Vec::with_capacity(t.inferred.len());
    for signal in &t.inferred {
        let signal = one_of(
            "inferred",
            signal,
            &["aborted", "no_complaint", "respawned"],
        )?;
        if !inferred.contains(&signal) {
            inferred.push(signal);
        }
    }
    inferred.sort();
    Ok(RouterRecord::Task(RouterTaskRow {
        class: t.class.as_ref().map(Class::row).transpose()?,
        outcome: t.outcome.as_str().into(),
        outcome_rank: t.outcome.rank(),
        completed_at_end: t.completed_at_end.map(|o| o.as_str().to_string()),
        files: file_hashes(t.files)?,
        task_id: t.task_id,
        agent_id: t.agent_id,
        started_at: t.started_at,
        ended_at: t.ended_at,
        inferred,
        escalations: t.escalations,
        exhausted: t.exhausted,
        turns: t.turns,
        explicit_quote: t.explicit_quote,
        outcome_source: Some("own".into()),
        checks,
    }))
}

fn task_outcome(o: TaskOutcomeRecord) -> Result<RouterRecord, String> {
    named("task_id", &o.task_id)?;
    timestamp("at", &o.at)?;
    Ok(RouterRecord::TaskOutcome(RouterTaskOutcomeRow {
        outcome: o.outcome.as_str().into(),
        outcome_rank: o.outcome.rank(),
        task_id: o.task_id,
        agent_id: o.agent_id,
        at: o.at,
    }))
}

fn typed<T: DeserializeOwned>(value: serde_json::Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|e| e.to_string())
}

/// Parse and validate a whole payload. Any bad record refuses the batch.
pub fn parse_batch(text: &str) -> Result<Batch, WireError> {
    let envelope_error = |detail: String| WireError {
        record: None,
        detail,
    };
    let envelope: Envelope =
        serde_json::from_str(text).map_err(|e| envelope_error(e.to_string()))?;
    if envelope.schema != WIRE_SCHEMA {
        return Err(envelope_error(format!(
            "schema {} is not {WIRE_SCHEMA}",
            envelope.schema
        )));
    }
    named("session", &envelope.session).map_err(envelope_error)?;
    let mut records = Vec::with_capacity(envelope.records.len());
    let mut inputs = Vec::new();
    for (index, value) in envelope.records.into_iter().enumerate() {
        let mut value = value;
        let kind = value
            .as_object_mut()
            .and_then(|object| object.remove("kind"))
            .and_then(|kind| kind.as_str().map(str::to_string));
        let Some(kind) = kind else {
            return Err(WireError {
                record: Some((index, "?".into())),
                detail: "not an object with a string `kind`".into(),
            });
        };
        let record = match kind.as_str() {
            "decision" => typed(value).and_then(decision),
            "usage" => typed(value).and_then(usage),
            "reassess" => typed(value).and_then(reassess),
            "task" => typed(value).and_then(task),
            "task_outcome" => typed(value).and_then(task_outcome),
            "classification" => typed(value)
                .and_then(|c| classification(&envelope.session, c))
                .map(|(record, input)| {
                    inputs.push(input);
                    record
                }),
            _ => Err(
                "kind is not decision|usage|reassess|task|task_outcome|classification".to_string(),
            ),
        };
        records.push(record.map_err(|detail| WireError {
            record: Some((index, kind.clone())),
            detail,
        })?);
    }
    Ok(Batch {
        session: envelope.session,
        records,
        inputs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_batch(session: &str) -> String {
        serde_json::json!({
            "schema": 1,
            "session": session,
            "records": [
                { "kind": "decision", "task_id": "t1", "turn_id": "u1", "agent_id": null,
                  "relation": "new_task",
                  "class": { "kind": "edit", "difficulty": 1, "scope": "local", "uncertainty": "low", "verifiable": true, "confidence": 0.9 },
                  "tier": "research", "model": "claude-haiku-5-5", "effort": null, "reason": "table",
                  "mode_effective": "shadow", "holdout": false, "applied": false,
                  "explored": false, "propensity": null, "draw": null, "would_pass_gate": true,
                  "at": "2026-10-07T10:00:00Z" },
                { "kind": "usage", "task_id": "t1", "turn_id": "u1", "step": 0, "agent_id": null,
                  "source": "step", "model": "claude-sonnet-5-5", "input_tokens": 100, "output_tokens": 20,
                  "cache_read_input_tokens": 1000, "cache_creation_input_tokens": 0, "at": "2026-10-07T10:00:01Z" },
                { "kind": "reassess", "task_id": "t1", "agent_id": null, "event": "failed_verification",
                  "tier_from": "research", "tier_to": "implementation", "effort_to": null, "escalating": true,
                  "at": "2026-10-07T10:00:02Z" },
                { "kind": "task", "task_id": "t1", "agent_id": null, "started_at": "2026-10-07T10:00:00Z",
                  "ended_at": null, "class": null, "outcome": "unknown", "inferred": ["no_complaint"],
                  "escalations": 1, "exhausted": false, "turns": 1, "explicit_quote": null }
            ]
        })
        .to_string()
    }

    #[test]
    fn the_contracts_four_record_kinds_parse() {
        let batch = parse_batch(&sample_batch("s1")).unwrap();
        assert_eq!(batch.session, "s1");
        assert_eq!(batch.records.len(), 4);
        assert!(matches!(&batch.records[3], RouterRecord::Task(t) if t.outcome_rank == 0));
    }

    #[test]
    fn a_bad_record_is_named_by_index_and_kind() {
        let mut value: serde_json::Value = serde_json::from_str(&sample_batch("s1")).unwrap();
        value["records"][1]["cost_micros"] = serde_json::json!(5);
        let error = parse_batch(&value.to_string()).unwrap_err();
        assert_eq!(error.record, Some((1, "usage".into())));
        assert!(error.detail.contains("cost_micros"), "{error}");

        let mut value: serde_json::Value = serde_json::from_str(&sample_batch("s1")).unwrap();
        value["records"][0]["class"]["difficulty"] = serde_json::json!(6);
        let error = parse_batch(&value.to_string()).unwrap_err();
        assert_eq!(error.record, Some((0, "decision".into())));

        let mut value: serde_json::Value = serde_json::from_str(&sample_batch("s1")).unwrap();
        value["records"][3]["outcome"] = serde_json::json!("inadequate");
        assert_eq!(
            parse_batch(&value.to_string()).unwrap_err().record,
            Some((3, "task".into()))
        );

        let mut value: serde_json::Value = serde_json::from_str(&sample_batch("s1")).unwrap();
        value["records"][2]["at"] = serde_json::json!("yesterday");
        assert_eq!(
            parse_batch(&value.to_string()).unwrap_err().record,
            Some((2, "reassess".into()))
        );
    }

    /// The plugin's classifier rows: a synthetic turn id and a task that
    /// may not exist yet.
    #[test]
    fn a_classifier_usage_row_for_an_unassigned_task_parses() {
        let payload = serde_json::json!({
            "schema": 1, "session": "s", "records": [
                { "kind": "usage", "task_id": "unassigned", "turn_id": "classifier:2026-10-07T10:00:00Z:3",
                  "step": 0, "agent_id": null, "source": "classifier", "model": "claude-haiku-5-5",
                  "input_tokens": 900, "output_tokens": 6, "cache_read_input_tokens": 0,
                  "cache_creation_input_tokens": 0, "at": "2026-10-07T10:00:00Z" }
            ]
        });
        let batch = parse_batch(&payload.to_string()).unwrap();
        assert!(matches!(&batch.records[0], RouterRecord::Usage(u) if u.task_id == "unassigned"));
    }

    #[test]
    fn file_hashes_and_the_end_outcome_parse_and_a_bad_hash_is_refused() {
        let mut value: serde_json::Value = serde_json::from_str(&sample_batch("s1")).unwrap();
        value["records"][3]["files"] = serde_json::json!(["bbbbbbbbbbbbbbbb", "0123456789abcdef"]);
        value["records"][3]["completed_at_end"] = serde_json::json!("completed_verified");
        value["records"][2]["event"] = serde_json::json!("revert");
        value["records"][2]["escalating"] = serde_json::json!(false);
        value["records"][2]["files"] = serde_json::json!(["0123456789abcdef"]);
        let batch = parse_batch(&value.to_string()).unwrap();
        assert!(matches!(&batch.records[3], RouterRecord::Task(t)
            if t.files == vec!["0123456789abcdef", "bbbbbbbbbbbbbbbb"]
                && t.completed_at_end.as_deref() == Some("completed_verified")));
        assert!(matches!(&batch.records[2], RouterRecord::Reassess(r)
            if r.event == "revert" && r.files == vec!["0123456789abcdef"]));
        value["records"][3]["files"] = serde_json::json!(["src/lib.rs"]);
        assert_eq!(
            parse_batch(&value.to_string()).unwrap_err().record,
            Some((3, "task".into()))
        );
    }

    /// What the plugin sent before this step: none of the new fields.
    #[test]
    fn an_old_plugins_payload_is_recorded() {
        let batch = parse_batch(&sample_batch("old")).unwrap();
        assert!(
            matches!(&batch.records[0], RouterRecord::Decision(d) if d.table_version.is_none())
        );
        assert!(matches!(&batch.records[3], RouterRecord::Task(t)
            if t.checks.is_empty() && t.outcome_source.as_deref() == Some("own")));
    }

    #[test]
    fn the_new_fields_and_the_task_outcome_record_parse() {
        let mut value: serde_json::Value = serde_json::from_str(&sample_batch("s1")).unwrap();
        value["records"][0]["table_version"] = serde_json::json!(2);
        value["records"][3]["checks"] = serde_json::json!([
            { "name": "go vet ./...", "kind": "lint", "passed": true },
            { "name": "amont-gate@abc1234", "kind": "gate", "passed": false }
        ]);
        value["records"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "kind": "task_outcome", "task_id": "t1", "agent_id": "a1",
                "outcome": "completed_verified", "at": "2026-10-07T10:05:00Z"
            }));
        let batch = parse_batch(&value.to_string()).unwrap();
        assert!(
            matches!(&batch.records[0], RouterRecord::Decision(d) if d.table_version == Some(2))
        );
        assert!(matches!(&batch.records[3], RouterRecord::Task(t) if t.checks.len() == 2));
        assert!(matches!(&batch.records[4], RouterRecord::TaskOutcome(o)
            if o.outcome == "completed_verified" && o.outcome_rank == 1
                && o.agent_id.as_deref() == Some("a1")));
        value["records"][3]["checks"][0]["kind"] = serde_json::json!("vibes");
        assert_eq!(
            parse_batch(&value.to_string()).unwrap_err().record,
            Some((3, "task".into()))
        );
    }

    fn classification_record() -> serde_json::Value {
        serde_json::json!({
            "kind": "classification", "call_id": "c1", "purpose": "prompt",
            "model": "claude-haiku-5-5", "prompt_version": "a".repeat(64),
            "input": "ACTIVE TASK: none\n\nREQUEST:\nrename foo to bar",
            "answer": { "relation": "new_task", "kind": "edit", "difficulty": 1, "scope": "local",
                        "uncertainty": "low", "verifiable": true,
                        "explicit": { "value": "correct", "quote": "that is wrong" },
                        "user_model": "opus", "user_model_for": "session", "confidence": 0.9 },
            "parse_ok": true, "latency_ms": 420, "at": "2026-10-07T10:00:00Z"
        })
    }

    fn with_record(record: serde_json::Value) -> String {
        serde_json::json!({ "schema": 1, "session": "s1", "records": [record] }).to_string()
    }

    #[test]
    fn a_classification_keeps_its_text_apart_from_the_row() {
        let batch = parse_batch(&with_record(classification_record())).unwrap();
        let RouterRecord::Classification(row) = &batch.records[0] else {
            panic!("not a classification: {:?}", batch.records[0]);
        };
        assert_eq!(
            row.input_sha256,
            crate::ids::sha256_hex("ACTIVE TASK: none\n\nREQUEST:\nrename foo to bar".as_bytes())
        );
        assert!(row.parse_ok && row.latency_ms == Some(420));
        let answer = row.answer_json.as_deref().unwrap();
        // The row has the answer's every field and none of the words.
        for field in [
            "relation",
            "kind",
            "difficulty",
            "scope",
            "uncertainty",
            "verifiable",
            "user_model",
            "confidence",
        ] {
            assert!(answer.contains(field), "{field}: {answer}");
        }
        assert!(answer.contains("correct") && answer.contains("opus"));
        assert!(!answer.contains("that is wrong"), "{answer}");
        assert!(!format!("{row:?}").contains("rename foo"));
        assert_eq!(batch.inputs.len(), 1);
        assert_eq!(
            batch.inputs[0].input,
            "ACTIVE TASK: none\n\nREQUEST:\nrename foo to bar"
        );
        assert_eq!(batch.inputs[0].quote.as_deref(), Some("that is wrong"));
    }

    #[test]
    fn an_unparsed_call_and_a_minimal_answer_are_recorded() {
        let mut value = classification_record();
        value["answer"] = serde_json::Value::Null;
        value["parse_ok"] = serde_json::json!(false);
        value.as_object_mut().unwrap().remove("latency_ms");
        let batch = parse_batch(&with_record(value)).unwrap();
        assert!(matches!(&batch.records[0], RouterRecord::Classification(c)
            if !c.parse_ok && c.answer_json.is_none() && c.latency_ms.is_none()));

        // `explicit`, `user_model` and `user_model_for` default.
        let mut value = classification_record();
        let answer = value["answer"].as_object_mut().unwrap();
        answer.remove("explicit");
        answer.remove("user_model");
        answer.remove("user_model_for");
        assert!(parse_batch(&with_record(value)).is_ok());
    }

    #[test]
    fn a_bad_classification_names_itself() {
        let bad = |edit: &dyn Fn(&mut serde_json::Value)| {
            let mut value = classification_record();
            edit(&mut value);
            parse_batch(&with_record(value)).unwrap_err()
        };
        let error = bad(&|v| v["purpose"] = serde_json::json!("chat"));
        assert_eq!(error.record, Some((0, "classification".into())));
        assert!(bad(&|v| v["prompt_version"] = serde_json::json!("abc"))
            .detail
            .contains("prompt_version"));
        assert!(bad(&|v| v["input"] = serde_json::json!("x".repeat(3073)))
            .detail
            .contains("3072"));
        assert!(bad(&|v| v["answer"]["difficulty"] = serde_json::json!(9))
            .detail
            .contains("difficulty"));
        assert!(bad(&|v| v["parse_ok"] = serde_json::json!(false))
            .detail
            .contains("parse_ok"));
        assert!(bad(&|v| v["cost"] = serde_json::json!(1))
            .detail
            .contains("cost"));
        let mut value = classification_record();
        value["input"] = serde_json::json!("x".repeat(3072));
        assert!(parse_batch(&with_record(value)).is_ok());
    }

    #[test]
    fn the_envelope_is_checked_too() {
        assert!(parse_batch("{}").unwrap_err().record.is_none());
        assert!(parse_batch(r#"{"schema":2,"session":"s","records":[]}"#).is_err());
        assert!(parse_batch(r#"{"schema":1,"session":" ","records":[]}"#).is_err());
        assert!(parse_batch(r#"{"schema":1,"session":"s","records":[],"x":1}"#).is_err());
        assert!(parse_batch(r#"{"schema":1,"session":"s","records":[{"kind":"other"}]}"#).is_err());
    }
}
