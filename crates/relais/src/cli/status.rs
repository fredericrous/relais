//! `relais status` and `relais explain`.

use super::run::successor_process;
use crate::*;

/// The successor intents a status shows: every pending one (a refused one
/// carries its refusal and retry time, and the plugin waits for that time),
/// and every claimed one whose claim was abandoned (plan A5). `status` and
/// `resume` only list them; the plugin is the one launcher.
/// An intent as `decide --json` and `native status` print it.
pub(crate) fn intent_json(
    ledger: &Ledger,
    intent: &relais::ledger::SuccessorIntent,
) -> serde_json::Value {
    // Where the plugin starts the run: the repository the decided run
    // worked in.
    let repo = ledger
        .run_workspace(&intent.predecessor)
        .ok()
        .flatten()
        .map(|workspace| workspace.repo_path.to_string_lossy().into_owned());
    serde_json::json!({
        "repo": repo,
        "intent_id": intent.intent_id,
        "decision_id": intent.decision_id,
        "predecessor": intent.predecessor.as_str(),
        "contract_digest": intent.contract_digest,
        "state": intent.state.as_str(),
        "run_id": intent.run_id.as_ref().map(RunId::as_str),
        "scope_delta": intent.scope_delta,
        "refusal": intent.refusal,
        "refusal_count": intent.refusal_count,
        "retry_after": intent.retry_after(),
    })
}

pub(crate) fn listed_intents(ledger: &Ledger) -> Vec<relais::ledger::SuccessorIntent> {
    ledger
        .listed_intents(&successor_process)
        .unwrap_or_else(|e| {
            eprintln!("relais: successor intents cannot be listed: {e}");
            Vec::new()
        })
}

/// One line per listed intent, for `status` and `resume`.
pub(crate) fn print_pending_intents(ledger: &Ledger) {
    for intent in listed_intents(ledger) {
        println!(
            "pending successor intent {} (decided run {}, {}): launch with `relais run --intent {}`",
            intent.intent_id,
            intent.predecessor,
            intent.state.as_str(),
            intent.intent_id
        );
        if let (Some(refusal), Some(retry)) = (&intent.refusal, intent.retry_after()) {
            println!("  last launch refused: {refusal} (retry after {retry})");
        }
    }
}

pub(crate) fn status_command(run_id: Option<&str>) -> Result<CliOutcome, CliError> {
    let ledger = open_ledger()?;
    let run_id = run_id.map(RunId::from_stored);
    match run_id {
        None => {
            let report = operational(
                report::runs_report(&ledger, "2000-01-01T00:00:00+00:00", None),
                "status",
            )?;
            print!("{}", report.render());
            print_pending_intents(&ledger);
            Ok(CliOutcome::Accepted)
        }
        Some(run_id) => {
            let Some(state) = operational(ledger.run_status(&run_id), "status")? else {
                eprintln!("relais status: unknown run {run_id}");
                return Ok(CliOutcome::UnknownRun);
            };
            let cost = operational(ledger.run_cost(&run_id), "status")?;
            let attempts = operational(ledger.attempt_count(&run_id), "status")?;
            println!("{run_id}: {state} (attempts: {attempts}, cost: {cost})");
            print_pending_intents(&ledger);
            Ok(CliOutcome::Accepted)
        }
    }
}

pub(crate) fn explain_command(run_id: &str) -> Result<CliOutcome, CliError> {
    let ledger = open_ledger()?;
    let run = RunId::from_stored(run_id);
    let transitions = operational(ledger.transitions(&run), "explain")?;
    if transitions.is_empty() {
        eprintln!("relais explain: unknown run {run_id}");
        return Ok(CliOutcome::UnknownRun);
    }
    let run_dir = paths::runs_dir().map_err(CliError::Home)?.join(run_id);
    let route_file = run_dir.join("route.txt");
    match std::fs::read_to_string(&route_file) {
        Ok(route) => print!("{route}"),
        // The route note is written per run and is not part of the
        // ledger: an older run, or one that never got that far, simply
        // has none.
        Err(_) => println!("(no route note was recorded for this run)"),
    }
    println!("run: {run_id}");
    println!("artifacts: {}", run_dir.display());
    for transition in &transitions {
        println!(
            "  {} -> {} ({})",
            transition
                .from_state
                .map(|state| state.as_str().to_string())
                .unwrap_or_else(|| "-".into()),
            transition.to_state,
            transition.reason
        );
        if let Some(detail) = transition
            .detail
            .as_ref()
            .and_then(|detail| detail.get("detail"))
            .and_then(|value| value.as_str())
        {
            println!("      {detail}");
        }
    }
    let children = operational(ledger.child_runs(&run), "explain")?;
    if !children.is_empty() {
        println!("work packages (SPEC §19; each a run of its own, costed into this one):");
        for child in &children {
            println!(
                "  {}: {} {} (attempts: {}, cost: {})",
                child.package,
                child.run,
                child.status,
                operational(ledger.attempt_count(&child.run), "explain")?,
                operational(ledger.run_cost(&child.run), "explain")?
            );
        }
    }
    if let Some(decision) = operational(ledger.decision_of_run(&run), "explain")? {
        let resolved = decision
            .resolution
            .zip(decision.resolved_at.as_deref())
            .zip(decision.actor.as_deref());
        if let Some(((resolution, resolved_at), actor)) = resolved {
            println!(
                "decision: {} ({}) -> {} by {actor} at {resolved_at} (waited {}s){}",
                decision.raised_state,
                decision.raised_reason,
                resolution.as_str(),
                decision.waited_seconds,
                decision
                    .note
                    .as_deref()
                    .map(|note| format!(" — {note}"))
                    .unwrap_or_default()
            );
        } else {
            println!(
                "decision: open since {} ({}: {}), waiting {}s",
                decision.raised_at,
                decision.raised_state,
                decision.raised_reason,
                decision.waited_seconds
            );
        }
    }
    let cost = operational(ledger.run_cost(&run), "explain")?;
    let completeness = operational(ledger.run_cost_completeness(&run), "explain")?;
    println!("cost: {}", report::cost_line(cost, completeness));
    if let Some((receipt, _hash)) = operational(ledger.receipt(&run), "explain")? {
        println!(
            "receipt: candidate {} ({} attempt(s), [{}])",
            receipt["candidate_sha"].as_str().unwrap_or("?"),
            receipt["attempts"].as_i64().unwrap_or(0),
            receipt["models_used"]
                .as_array()
                .map(|models| models
                    .iter()
                    .filter_map(|model| model.as_str())
                    .collect::<Vec<_>>()
                    .join(","))
                .unwrap_or_default()
        );
    }
    // A per-run query (`outcome_of_run`), not the task-scoped
    // `latest_outcome`: a later run of the same task recording its own
    // feedback must not hide an earlier run's outcome when THAT run is
    // the one being explained (SPEC §20, issue #98).
    if let Some(stored) = operational(ledger.outcome_of_run(&run), "explain")? {
        println!(
            "outcome: {}{}",
            stored.outcome.kind.as_str(),
            stored
                .outcome
                .detail
                .note
                .as_deref()
                .map(|note| format!(" — {note}"))
                .unwrap_or_default()
        );
    }
    let evidence = operational(ledger.evidence(&run), "explain")?;
    if !evidence.is_empty() {
        println!("evidence:");
        for row in &evidence {
            // Read through the typed row, not the positional tuple this
            // replaced — a caller here cannot print the kind where it
            // meant to print the path.
            print!("  {}: {}", row.kind, row.path);
            if let Some(tool) = &row.tool {
                print!(" via {tool}");
            }
            if let Some(criterion_id) = &row.criterion_id {
                print!(" for {criterion_id}");
            }
            println!();
        }
    }
    Ok(CliOutcome::Accepted)
}
