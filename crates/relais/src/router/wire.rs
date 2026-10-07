//! `relais native router-observe`'s payload (docs/router-protocol.md):
//! parsed and validated whole, before anything is written, so a bad record
//! drops the batch and names itself. Unknown fields are refused. Pure.

use serde::de::DeserializeOwned;
use serde::Deserialize;

use super::outcome::TaskOutcome;
use crate::ledger::{
    RouterClass, RouterDecisionRow, RouterReassessRow, RouterRecord, RouterTaskRow, RouterUsageRow,
};

/// The wire schema this relais reads.
pub const WIRE_SCHEMA: u32 = 1;

/// A validated batch: the session and its records, ready to write.
#[derive(Debug, Clone, PartialEq)]
pub struct Batch {
    pub session: String,
    pub records: Vec<RouterRecord>,
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
            _ => Err("kind is not decision|usage|reassess|task".to_string()),
        };
        records.push(record.map_err(|detail| WireError {
            record: Some((index, kind.clone())),
            detail,
        })?);
    }
    Ok(Batch {
        session: envelope.session,
        records,
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

    #[test]
    fn the_envelope_is_checked_too() {
        assert!(parse_batch("{}").unwrap_err().record.is_none());
        assert!(parse_batch(r#"{"schema":2,"session":"s","records":[]}"#).is_err());
        assert!(parse_batch(r#"{"schema":1,"session":" ","records":[]}"#).is_err());
        assert!(parse_batch(r#"{"schema":1,"session":"s","records":[],"x":1}"#).is_err());
        assert!(parse_batch(r#"{"schema":1,"session":"s","records":[{"kind":"other"}]}"#).is_err());
    }
}
