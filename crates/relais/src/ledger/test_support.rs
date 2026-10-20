#![cfg(test)]
//! Fixtures several ledger tests share.

use super::schema::migrations::MIGRATIONS;
use super::*;
use crate::ids::{DispatchId, PackageId, RunId, TaskId, TrialId};
use crate::money::{CostCompleteness, CostKind, MicroUsd};
use crate::orchestration::{Speed, TranscriptSource};
use crate::outcome::OutcomeKind;
use crate::outcome::{Outcome, OutcomeDetail};
use crate::policy::Tier;
use rusqlite::params;

pub(super) fn run(id: &str) -> RunId {
    RunId::from_stored(id)
}

pub(super) fn dispatch(id: &str) -> DispatchId {
    DispatchId::from_stored(id)
}

pub(super) fn package(id: &str) -> PackageId {
    PackageId::from_stored(id)
}

pub(super) fn task(id: &str) -> TaskId {
    TaskId::from_stored(format!("task-{id}"))
}

/// Pid AND an in-process counter, pre-cleaned: the test binary runs
/// these in parallel threads of one process, so a pid-only name is
/// one directory two tests share (P12).
pub(super) fn temp_dir(label: &str) -> crate::test_support::TempDir {
    crate::test_support::temp_dir(&format!("ledger-{label}"))
}

pub(super) fn temp_ledger() -> (Ledger, crate::test_support::TempDir) {
    let dir = temp_dir("open");
    let path = dir.join("ledger.sqlite");
    let ledger = Ledger::open(&path).expect("ledger opens");
    (ledger, dir)
}

pub(super) fn trial(id: &str) -> TrialId {
    TrialId::from_stored(id)
}

pub(super) fn new_trial<'a>(
    id: &'a TrialId,
    task_id: &'a TaskId,
    source_run: &'a RunId,
) -> NewTrial<'a> {
    NewTrial {
        trial_id: id,
        task_id,
        source_run_id: source_run,
        incumbent_recipe_id: "recipe-incumbent",
        arm_recipe_id: "recipe-arm",
        arm_index: 1,
        assignment_probability: 0.25,
        seed: 42,
        base_sha: "base-sha",
        contract_hash: "contract-hash",
        verification_profile_hash: "profile-hash",
        workspace_isolation: "worktree",
        arms_json: None,
        arm_run_id: source_run,
        design_sha256: None,
    }
}

pub(super) fn usage_row(message_id: &str, session_id: &str, at: &str) -> OrchestrationUsageRow {
    OrchestrationUsageRow {
        message_id: message_id.into(),
        session_id: session_id.into(),
        transcript: TranscriptSource::Main,
        model: "claude-haiku-4-5".into(),
        speed: Speed::Standard,
        input_tokens: 100,
        output_tokens: 50,
        cache_read_tokens: 0,
        cache_write_5m_tokens: 0,
        cache_write_1h_tokens: 0,
        cost: Some(MicroUsd::from_micros(1_000)),
        pricing_version: Some("2026-09-01".into()),
        completeness: CostCompleteness::Actual,
        at: at.into(),
    }
}

pub(super) fn sample_outcome(kind: OutcomeKind) -> Outcome {
    Outcome::new(
        kind,
        OutcomeDetail {
            candidate_sha: Some("cand123".into()),
            strategy: crate::outcome::Strategy {
                tier: Tier::Implementation,
                models: vec!["sonnet".into()],
                escalated: false,
            },
            correction_magnitude: None,
            evidence: vec!["run-a/review.txt".into()],
            actor: "a reviewer".into(),
            note: None,
            source: None,
        },
    )
    .expect("a valid outcome")
}

/// A ledger pinned at v25 with one decision and one sign-off per
/// actor spelling in `spellings`, plus an open decision (no actor).
pub(super) fn v26_ledger_with(path: &std::path::Path, spellings: &[&str]) {
    let conn = Connection::open(path).expect("open");
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version TEXT PRIMARY KEY,
            applied_at TEXT NOT NULL
        );",
    )
    .expect("migrations table");
    for (version, sql) in &MIGRATIONS[..26] {
        conn.execute_batch(sql).expect("step");
        conn.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
            params![version, "2026-10-01T00:00:00+00:00"],
        )
        .expect("record");
    }
    for (i, spelling) in spellings.iter().enumerate() {
        conn.execute(
            "INSERT INTO decisions (run, task, raised_at, raised_state, raised_reason,
                                    resolved_at, resolution, actor)
             VALUES (?1, 't', '2026-10-01T00:00:00+00:00', 'needs_decision', 'scope_exceeded',
                     '2026-10-01T00:01:00+00:00', 'decision_approved', ?2)",
            params![format!("run-{i}"), spelling],
        )
        .expect("decision");
        conn.execute(
            "INSERT INTO human_signoffs (run_id, criterion_id, actor, recorded_at)
             VALUES (?1, 'c', ?2, '2026-10-01T00:01:00+00:00')",
            params![format!("run-{i}"), spelling],
        )
        .expect("signoff");
    }
    conn.execute(
        "INSERT INTO decisions (run, task, raised_at, raised_state, raised_reason)
         VALUES ('run-open', 't', '2026-10-01T00:00:00+00:00', 'needs_review', 'x')",
        [],
    )
    .expect("open decision");
}

pub(super) fn event(event_id: &str, run_id: &str, micros: i64) -> UsageEvent {
    UsageEvent {
        event_id: event_id.into(),
        run_id: run(run_id),
        attempt_id: None,
        parent_event_id: None,
        model: Some("sonnet".into()),
        input_tokens: Some(100),
        output_tokens: Some(10),
        cache_read_tokens: None,
        cache_write_tokens: None,
        cost: Some(MicroUsd::from_micros(micros)),
        cost_kind: CostKind::ApiSpend,
        completeness: CostCompleteness::Actual,
        inclusive: false,
        at: now_rfc3339(),
        phase: None,
        duration_ms: None,
        requested_model: None,
        requested_effort: None,
        harness: None,
    }
}

pub(super) fn driver_of(
    ledger: &Ledger,
    id: &str,
) -> (Option<i64>, Option<String>, Option<String>) {
    ledger
        .conn
        .query_row(
            "SELECT driver_pid, driver_started, driver_since FROM runs WHERE id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("driver columns")
}
