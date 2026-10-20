//! `relais report` and `relais usage import`.

use super::ctx::load_price_table;
use super::ctx::open_ledger;
use super::ctx::print_document;
use super::outcome::operational;
use super::outcome::CliError;
use super::outcome::CliOutcome;
use relais::ledger::Ledger;
use relais::ledger::OrchestrationUsageRow;
use relais::money::CostCompleteness;
use relais::money::MicroUsd;
use relais::orchestration;
use relais::orchestration::PriceTable;
use relais::orchestration::TranscriptSource;
use relais::paths;
use relais::report;
use std::path::Path;

pub(crate) fn report_command(
    since: Option<&str>,
    json: bool,
    by: Option<report::Dimension>,
    no_import: bool,
) -> Result<CliOutcome, CliError> {
    let since = since.map(|since| since.to_string()).unwrap_or_else(|| {
        chrono::Utc::now()
            .format("%Y-%m-01T00:00:00+00:00")
            .to_string()
    });
    let ledger = open_ledger()?;
    let mut not_imported = NotImported::default();
    if !no_import {
        let projects_dir = paths::claude_projects_dir().map_err(CliError::Home)?;
        let price_table = load_price_table()?;
        not_imported = import_for_report(&ledger, &projects_dir, &price_table, &since)?;
    }
    let mut report = operational(report::runs_report(&ledger, &since, by), "report")?;
    report.session_routing = operational(
        relais::router::report_section(&ledger, &since, &load_price_table()?),
        "the session-routing section",
    )?;
    // Best-effort: a registry that will not open reads as no active artifact.
    let registry_for_report = paths::registry_dir()
        .ok()
        .and_then(|dir| relais::learn::registry::Registry::open(&dir).ok());
    report.learning = Some(operational(
        relais::learn::summary::learning_report(&ledger, registry_for_report.as_ref()),
        "the learning section",
    )?);
    report.review_outcomes = Some(report::reviewer_report(&ledger));
    report.orchestration.unattributable = not_imported.unattributable;
    report.orchestration.transcript_missing = not_imported.transcript_missing;
    // Best-effort: a report is still a report with no coordinator
    // reachable, and `EnforcementReport::observed` already says so
    // honestly rather than this command failing over it.
    report.enforcement = relais::coordinator::socket_path()
        .ok()
        .map(relais::coordinator::Client::new)
        .and_then(|client| client.status().ok())
        .map(|snapshot| {
            // A snapshot came back: a coordinator is serving. The
            // fallback below is the other half of the same question, so
            // neither is a free-standing claim about enforcement.
            report::EnforcementReport::from_snapshot(
                relais::admission::Enforcement::Coordinator,
                &snapshot,
            )
        })
        .unwrap_or_else(report::EnforcementReport::observed);
    if json {
        print_document(&report)?;
    } else {
        print!("{}", report.render());
    }
    Ok(CliOutcome::Accepted)
}

/// Every run recorded before the session-identity fix (#119) keyed a run
/// by the bare shell PID relais fell back to — a value that was never a
/// Claude Code session id, so it can never have a transcript. Reported
/// as `unattributable`, counted, never guessed at (SPEC §11); distinct
/// from a session that SHOULD have a transcript but none was found,
/// which is `transcript missing` instead.
pub(crate) fn is_bare_shell_pid(session_id: &str) -> bool {
    !session_id.is_empty() && session_id.chars().all(|c| c.is_ascii_digit())
}

/// One session's message count, token totals and cost, imported.
pub(crate) struct ImportSummary {
    pub(crate) messages: usize,
    /// Records left out because a relais run booked them (a native
    /// worker's messages, SPEC §11); not part of any other figure.
    pub(crate) skipped_native: usize,
    pub(crate) input_tokens: u64,
    pub(crate) output_tokens: u64,
    pub(crate) cache_read_tokens: u64,
    cache_write_5m_tokens: u64,
    cache_write_1h_tokens: u64,
    pub(crate) cost: MicroUsd,
    pub(crate) completeness: CostCompleteness,
}

/// Sessions `relais report` could not import, counted only for sessions
/// that started a run inside the report window.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct NotImported {
    pub(crate) unattributable: usize,
    pub(crate) transcript_missing: usize,
}

/// What `relais report` imports before it renders. EVERY orchestrating
/// session is imported, not only those whose run started inside the
/// window: a session that started a run on the 30th and kept reviewing
/// and landing work on the 1st and 2nd spends inside a window its run
/// predates, and that spend is attributed by each message's own
/// timestamp (SPEC §11) — but only once it has been imported. Import is
/// idempotent (`message_id` is unique), so re-reading a session costs a
/// parse and inserts nothing. The window narrows only the COUNTS of
/// sessions that could not be imported, so a pre-fix PID session from
/// months ago is not reported as missing on every later report.
pub(crate) fn import_for_report(
    ledger: &Ledger,
    projects_dir: &Path,
    price_table: &PriceTable,
    since: &str,
) -> Result<NotImported, CliError> {
    let in_window: std::collections::BTreeSet<String> =
        operational(ledger.distinct_root_sessions_since(since), "usage import")?
            .into_iter()
            .collect();
    let mut not_imported = NotImported::default();
    for session_id in operational(ledger.distinct_root_sessions(), "usage import")? {
        let counted = in_window.contains(&session_id);
        match import_session(ledger, projects_dir, price_table, &session_id)? {
            SessionImportOutcome::Imported(_) => {}
            SessionImportOutcome::Unattributable if counted => not_imported.unattributable += 1,
            SessionImportOutcome::TranscriptMissing if counted => {
                not_imported.transcript_missing += 1
            }
            SessionImportOutcome::Unattributable | SessionImportOutcome::TranscriptMissing => {}
        }
    }
    Ok(not_imported)
}

pub(crate) enum SessionImportOutcome {
    Imported(ImportSummary),
    Unattributable,
    TranscriptMissing,
}

/// Import one session's orchestration usage: its main transcript and
/// every subagent file beside it, except what a relais run booked: the
/// message ids in `native_usage_messages` and the whole file of an agent
/// a dispatch recorded (SPEC §11). Idempotent — every insert
/// goes through `record_orchestration_usage`'s `message_id` uniqueness,
/// so importing the same session twice inserts nothing the second time.
pub(crate) fn import_session(
    ledger: &Ledger,
    projects_dir: &Path,
    price_table: &PriceTable,
    session_id: &str,
) -> Result<SessionImportOutcome, CliError> {
    if is_bare_shell_pid(session_id) {
        return Ok(SessionImportOutcome::Unattributable);
    }
    let Some(main_path) = paths::find_transcript(projects_dir, session_id) else {
        return Ok(SessionImportOutcome::TranscriptMissing);
    };
    let mut files = vec![(TranscriptSource::Main, main_path.clone())];
    for path in paths::subagent_transcripts(&main_path) {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("subagent")
            .to_string();
        files.push((TranscriptSource::Subagent(name), path));
    }
    let mut summary = ImportSummary {
        messages: 0,
        skipped_native: 0,
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_5m_tokens: 0,
        cache_write_1h_tokens: 0,
        cost: MicroUsd::ZERO,
        completeness: CostCompleteness::Actual,
    };
    let mut parts = Vec::new();
    for (source, path) in files {
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(cause) => {
                return Err(CliError::Read {
                    what: "a transcript file",
                    path,
                    cause,
                })
            }
        };
        // A relais dispatch's agent is the run's, whatever message ids its
        // transcript carries: a dispatch whose ids could not be recorded
        // (`rollback_ids_missing`) is still never booked twice.
        let relais_agent = match paths::subagent_agent_id(&path) {
            Some(agent_id) => operational(ledger.is_relais_agent(agent_id), "usage import")?,
            None => false,
        };
        for record in orchestration::parse_transcript(&text) {
            if relais_agent
                || operational(ledger.is_native_booked(&record.message_id), "usage import")?
            {
                summary.skipped_native += 1;
                continue;
            }
            let priced = orchestration::price(&record, price_table);
            summary.messages += 1;
            summary.input_tokens += record.input_tokens;
            summary.output_tokens += record.output_tokens;
            summary.cache_read_tokens += record.cache_read_input_tokens;
            summary.cache_write_5m_tokens += record.cache_writes.ephemeral_5m_input_tokens;
            summary.cache_write_1h_tokens += record.cache_writes.ephemeral_1h_input_tokens;
            if let Some(cost) = priced.cost {
                summary.cost = summary.cost.saturating_add(cost);
            }
            parts.push(priced.completeness);
            let row = OrchestrationUsageRow {
                message_id: record.message_id,
                session_id: session_id.to_string(),
                transcript: source.clone(),
                model: record.model,
                speed: record.speed,
                input_tokens: record.input_tokens,
                output_tokens: record.output_tokens,
                cache_read_tokens: record.cache_read_input_tokens,
                cache_write_5m_tokens: record.cache_writes.ephemeral_5m_input_tokens,
                cache_write_1h_tokens: record.cache_writes.ephemeral_1h_input_tokens,
                cost: priced.cost,
                pricing_version: if priced.pricing_version.is_empty() {
                    None
                } else {
                    Some(priced.pricing_version)
                },
                completeness: priced.completeness,
                at: record.timestamp,
            };
            operational(ledger.record_orchestration_usage(&row), "usage import")?;
        }
    }
    summary.completeness = CostCompleteness::worst(parts);
    Ok(SessionImportOutcome::Imported(summary))
}

/// The summary line's tail when records were left to the run that booked them.
pub(crate) fn skipped_note(skipped: usize) -> String {
    match skipped {
        0 => String::new(),
        n => format!("; {n} record(s) skipped, already booked to a relais run"),
    }
}

pub(crate) fn usage_import_command(
    session: Option<&str>,
    projects_dir: Option<&Path>,
) -> Result<CliOutcome, CliError> {
    let ledger = open_ledger()?;
    let projects_dir = match projects_dir {
        Some(dir) => dir.to_path_buf(),
        None => paths::claude_projects_dir().map_err(CliError::Home)?,
    };
    let price_table = load_price_table()?;
    let sessions = match session {
        Some(id) => vec![id.to_string()],
        None => operational(ledger.distinct_root_sessions(), "usage import")?,
    };
    if sessions.is_empty() {
        println!("relais usage import: no session is the root of any run on record");
    }
    for session_id in sessions {
        match import_session(&ledger, &projects_dir, &price_table, &session_id)? {
            SessionImportOutcome::Imported(summary) => {
                println!(
                    "{session_id}: {} message(s), {} input + {} output tokens: {}{}",
                    summary.messages,
                    summary.input_tokens,
                    summary.output_tokens,
                    report::cost_line(summary.cost, summary.completeness),
                    skipped_note(summary.skipped_native),
                );
            }
            SessionImportOutcome::Unattributable => {
                println!(
                    "{session_id}: unattributable (root_session is a bare shell PID from \
                     before the session-identity fix; it never had a transcript)"
                );
            }
            SessionImportOutcome::TranscriptMissing => {
                println!("{session_id}: transcript missing");
            }
        }
    }
    Ok(CliOutcome::Accepted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::tests::temp_ledger;
    use relais::ledger::Ledger;
    use std::path::PathBuf;

    /// A session transcript: one assistant message with this id.
    fn assistant_line(message_id: &str) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"2026-10-06T10:00:00Z","message":{{"id":"{message_id}","model":"claude-opus-5","usage":{{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":0}}}}}}"#
        )
    }

    /// The projects directory of one session: its main transcript, and
    /// the transcripts of a relais agent (`a-relais`) and of the person's
    /// own subagent (`a-own`).
    fn session_with_two_subagents(dir: &Path) -> PathBuf {
        let projects = dir.join("projects");
        let slug = projects.join("-slug");
        let subagents = slug.join("sess-r").join("subagents");
        std::fs::create_dir_all(&subagents).expect("subagents dir");
        std::fs::write(slug.join("sess-r.jsonl"), assistant_line("msg-main")).expect("main");
        std::fs::write(
            subagents.join("agent-a-relais.jsonl"),
            assistant_line("msg-relais"),
        )
        .expect("relais agent");
        std::fs::write(
            subagents.join("agent-a-own.jsonl"),
            assistant_line("msg-own"),
        )
        .expect("own agent");
        projects
    }

    /// `usage import` books the session's own messages and the person's
    /// subagents, and nothing from the transcript of an agent the ledger
    /// records as a relais dispatch — even though none of that agent's
    /// message ids is in `native_usage_messages` (`rollback_ids_missing`).
    #[test]
    fn usage_import_skips_the_agent_file_of_a_relais_dispatch() {
        let (ledger, dir) = temp_ledger("import-agent-file");
        let projects = session_with_two_subagents(&dir);
        let run = relais::ids::RunId::from_stored("run-r");
        ledger
            .insert_run(
                &run,
                "/repo",
                Some("sess-r"),
                &relais::ids::TaskId::from_stored("task-r"),
                "rk",
            )
            .expect("run");
        let imported = |ledger: &Ledger| match import_session(
            ledger,
            &projects,
            &PriceTable::empty(),
            "sess-r",
        )
        .expect("imports")
        {
            SessionImportOutcome::Imported(summary) => summary,
            SessionImportOutcome::Unattributable | SessionImportOutcome::TranscriptMissing => {
                panic!("the session has a transcript")
            }
        };

        // No dispatch on record: all three files are the session's.
        let (fresh, fresh_dir) = temp_ledger("import-agent-file-fresh");
        assert_eq!(imported(&fresh).messages, 3);
        drop(fresh_dir);

        let dispatch = relais::ids::DispatchId::from_stored("d-r");
        ledger
            .record_dispatch_intent(
                &dispatch,
                &run,
                None,
                &serde_json::json!({}),
                0,
                relais::route::RoutedBy::ConservativeBaseline,
            )
            .expect("dispatch");
        ledger
            .record_native_dispatch(&dispatch, Some("a-relais"))
            .expect("agent bound");
        assert!(!ledger.is_native_booked("msg-relais").expect("lookup"));

        let summary = imported(&ledger);
        assert_eq!(
            summary.messages, 2,
            "the main transcript and the person's agent"
        );
        assert_eq!(summary.skipped_native, 1, "the relais agent's file");
        // Idempotent: a second import inserts nothing either.
        assert_eq!(imported(&ledger).messages, 2);
    }

    /// A session that started its run BEFORE the report window and kept
    /// orchestrating inside it: its in-window spend must be imported and
    /// counted by `relais report`'s own import path, not only by a
    /// hand-run `relais usage import`. Session S starts a run on 09-30,
    /// then spends on 10-02; the window opens on 10-01.
    ///
    /// FALSIFIED: with `import_for_report` iterating
    /// `distinct_root_sessions_since` (the reviewed patch), the in-window
    /// cost read back as $0 and this test failed; restored.
    #[test]
    fn report_imports_in_window_spend_of_a_session_whose_run_predates_the_window() {
        let dir = relais::test_support::short_temp_dir("main-orch-window");
        let ledger = Ledger::open_with_clock(
            &dir.join("ledger.sqlite"),
            Box::new(relais::ledger::FixedClock::new([
                "2026-09-30T23:00:00+00:00",
            ])),
        )
        .expect("ledger opens");
        ledger
            .insert_run(
                &relais::ids::RunId::from_stored("run-s"),
                "/repo",
                Some("sess-s"),
                &relais::ids::TaskId::from_stored("task-s"),
                "rk",
            )
            .expect("run before the window");
        let projects = dir.join("projects").join("-slug");
        std::fs::create_dir_all(&projects).expect("projects dir");
        std::fs::write(
            projects.join("sess-s.jsonl"),
            r#"{"type":"assistant","timestamp":"2026-10-02T10:00:00Z","message":{"id":"msg-oct","model":"claude-opus-5","usage":{"input_tokens":1000000,"output_tokens":0,"cache_read_input_tokens":0,"cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":0}}}}"#,
        )
        .expect("transcript");
        let table = PriceTable {
            version: "test".into(),
            models: vec![relais::orchestration::ModelPrice {
                ids: vec!["claude-opus-5".into()],
                input: 5_000_000,
                output: 25_000_000,
                cache_read: 500_000,
                cache_write_5m: 6_250_000,
                cache_write_1h: 10_000_000,
                fast_input: None,
                fast_output: None,
            }],
        };
        let since = "2026-10-01T00:00:00+00:00";
        let not_imported =
            import_for_report(&ledger, &dir.join("projects"), &table, since).expect("imports");
        assert_eq!(not_imported, NotImported::default());
        let (cost, _) = ledger
            .orchestration_spend_since(since)
            .expect("spend in the window");
        assert_eq!(cost, MicroUsd::from_micros(5_000_000));
    }

    /// Run first: the native attempt booked the worker's messages from
    /// its subagent transcript, so importing the session inserts the
    /// session's own messages only and reports what it left to the run.
    #[test]
    fn import_leaves_a_native_workers_messages_to_the_run_that_booked_them() {
        use relais::ids::{RunId, TaskId};
        use relais::ledger::UsageEvent;
        use relais::money::CostKind;
        let (ledger, dir) = temp_ledger("import-native");
        let run = RunId::from_stored("run-n");
        ledger
            .insert_run(
                &run,
                "/repo",
                Some("sess-n"),
                &TaskId::from_stored("task-n"),
                "rk",
            )
            .expect("run");
        let line = |id: &str| {
            format!(
                r#"{{"type":"assistant","timestamp":"2026-10-06T10:00:00Z","message":{{"id":"{id}","model":"claude-opus-5","usage":{{"input_tokens":1000000,"output_tokens":0,"cache_read_input_tokens":0,"cache_creation":{{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":0}}}}}}}}"#
            )
        };
        let slug = dir.join("projects").join("-slug");
        let subagents = slug.join("sess-n").join("subagents");
        std::fs::create_dir_all(&subagents).expect("subagents dir");
        std::fs::write(slug.join("sess-n.jsonl"), line("msg-session")).expect("main");
        std::fs::write(subagents.join("agent-a1.jsonl"), line("msg-worker")).expect("agent");
        let table = PriceTable {
            version: "test".into(),
            models: vec![relais::orchestration::ModelPrice {
                ids: vec!["claude-opus-5".into()],
                input: 5_000_000,
                output: 25_000_000,
                cache_read: 500_000,
                cache_write_5m: 6_250_000,
                cache_write_1h: 10_000_000,
                fast_input: None,
                fast_output: None,
            }],
        };
        let booked = UsageEvent {
            event_id: "d1".into(),
            run_id: run.clone(),
            attempt_id: None,
            parent_event_id: None,
            model: Some("claude-opus-5".into()),
            input_tokens: Some(1_000_000),
            output_tokens: Some(0),
            cache_read_tokens: None,
            cache_write_tokens: None,
            cost: Some(MicroUsd::from_micros(5_000_000)),
            cost_kind: CostKind::EstimatedApiEquivalent,
            completeness: CostCompleteness::Estimated,
            inclusive: false,
            at: ledger.now(),
            phase: None,
            duration_ms: None,
            requested_model: None,
            requested_effort: None,
            harness: None,
        };
        ledger
            .record_native_usage(&booked, &["msg-worker".to_string()])
            .expect("native booking");
        let SessionImportOutcome::Imported(summary) =
            import_session(&ledger, &dir.join("projects"), &table, "sess-n").expect("imports")
        else {
            panic!("the session has a transcript");
        };
        assert_eq!(summary.messages, 1);
        assert_eq!(summary.skipped_native, 1);
        assert!(skipped_note(summary.skipped_native).contains("1 record(s) skipped"));
        let (session_cost, _) = ledger
            .orchestration_spend_since("2026-01-01T00:00:00Z")
            .expect("spend");
        assert_eq!(
            session_cost,
            MicroUsd::from_micros(5_000_000),
            "the session's own only"
        );
        assert_eq!(
            ledger.run_cost(&run).expect("run cost"),
            MicroUsd::from_micros(5_000_000)
        );
    }
}
