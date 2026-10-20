//! `relais evidence attach`.

use super::*;

/// Record one piece of evidence produced by another tool, against a run
/// and, optionally, the acceptance criterion it answers. This decides
/// nothing: it never marks a criterion met, changes a run's state, or
/// clears a gap — only `relais decide` and the runner's own verification
/// do that.
pub(crate) fn evidence_attach_command(
    run_id: &str,
    path: &Path,
    tool: Option<&str>,
    external_id: Option<&str>,
    subject: Option<&str>,
    criterion: Option<&str>,
) -> Result<CliOutcome, CliError> {
    let ledger = open_ledger()?;
    let run = RunId::from_stored(run_id);
    if operational(ledger.run_status(&run), "evidence attach")?.is_none() {
        eprintln!("relais evidence attach: unknown run {run_id}");
        return Ok(CliOutcome::UnknownRun);
    }
    if let Some(criterion_id) = criterion {
        let Some((contract, _tier)) =
            operational(ledger.run_contract_and_tier(&run), "evidence attach")?
        else {
            eprintln!("relais evidence attach: run {run_id} carries no recorded contract");
            return Ok(CliOutcome::OperationalFailure);
        };
        let declared: Vec<String> = contract.acceptance.iter().map(|entry| entry.id()).collect();
        if !declared.iter().any(|id| id == criterion_id) {
            eprintln!(
                "relais evidence attach: run {run_id} names no acceptance criterion \
                 `{criterion_id}` — this contract declares: {}",
                declared.join(", ")
            );
            return Ok(CliOutcome::InvalidInput);
        }
    }
    let sha256 = match workspace::sha256_file(path) {
        Ok(sha256) => sha256,
        Err(cause) => {
            eprintln!(
                "relais evidence attach: {path} cannot be read: {cause}",
                path = path.display()
            );
            return Ok(CliOutcome::InvalidInput);
        }
    };
    // Every other writer of an evidence row stores an absolute path —
    // the runner joins its artifacts directory, `decide` joins the runs
    // directory — and `relais explain` prints them all side by side. A
    // relative `--path` recorded verbatim reads back as a path that
    // resolves from whatever directory the person happened to be in,
    // which is no directory at all by the time anyone looks. The sha256
    // pins the content; this pins where it was.
    let path = match path.canonicalize() {
        Ok(absolute) => absolute,
        Err(cause) => {
            eprintln!(
                "relais evidence attach: {path} cannot be resolved to an absolute path: {cause}",
                path = path.display()
            );
            return Ok(CliOutcome::InvalidInput);
        }
    };
    let path = path.as_path();
    operational(
        ledger.attach_evidence(
            &run,
            path,
            Some(&sha256),
            EvidenceOrigin {
                tool,
                external_id,
                subject,
                criterion_id: criterion,
            },
        ),
        "evidence attach",
    )?;
    println!("attached evidence for {run_id}: {}", path.display());
    Ok(CliOutcome::Accepted)
}

/// How a test criterion's authorship reads in a refusal.
pub(crate) fn authorship_label(authorship: relais::acceptance::TestAuthorship) -> &'static str {
    use relais::acceptance::TestAuthorship;
    match authorship {
        TestAuthorship::PreExisting => "pre-existing",
        TestAuthorship::HumanAdded => "human-added",
        TestAuthorship::ModelAdded => "model-added",
    }
}

/// Mark one criterion met by the sign-off `relais decide` just recorded,
/// and re-seal the run's receipt with it (SPEC §10, §12): the runner
/// already stored a receipt for this run when every check, test and
/// review passed and only a human sign-off gap remained
/// ([`relais::runner::RunEngine::store_pending_receipt`]) — this is the
/// one place that receipt is ever edited, never a second acceptance path
/// built alongside it. A run with no stored receipt yet (an older run,
/// or one whose gaps were never signoff-only) has nothing to re-seal;
/// `relais decide`'s own gap check further down still refuses it if
/// other gaps remain.
pub(crate) fn reseal_receipt_with_signoff(
    ledger: &Ledger,
    run: &RunId,
    criterion_id: &str,
) -> Result<(), CliError> {
    let runs_dir = paths::runs_dir().map_err(CliError::Home)?;
    reseal_receipt_in(ledger, &runs_dir, run, criterion_id)
}

/// [`reseal_receipt_with_signoff`] against an explicit `runs_dir`, the one
/// place a run's `receipt.json` lives.
pub(crate) fn reseal_receipt_in(
    ledger: &Ledger,
    runs_dir: &Path,
    run: &RunId,
    criterion_id: &str,
) -> Result<(), CliError> {
    let Some((stored, _hash)) = operational(ledger.receipt(run), "decide")? else {
        return Ok(());
    };
    // A receipt this same binary wrote and now cannot parse back is
    // corrupt, not absent — `operational` refuses loudly rather than
    // this re-seal silently skipping it.
    let mut receipt: Receipt = operational(serde_json::from_value(stored), "decide")?;
    for criterion in &mut receipt.criteria {
        if criterion.id == criterion_id {
            criterion.met = true;
            // The criterion ALREADY declared a human sign-off — `decide`
            // refuses `--criterion` for any other evidence kind — so this
            // records which evidence settled it, never a rewrite of what
            // the contract asked for.
            criterion.evidence = Some(Evidence::HumanSignOff);
            criterion.settled_via = Some(SettledVia::HumanSignOff);
        }
    }
    receipt.mandatory_evidence_independence = independence_summary(&receipt.criteria);
    // The gap this sign-off answers is no longer open, so the receipt
    // stops naming it — and once no gap is left, the receipt says what
    // the run now is. A receipt still reading `needs_decision`, still
    // listing a gap a person has since answered, beside a run the very
    // same command moved to `accepted`, is two records of one fact
    // disagreeing: exactly the drift `resolve_decision`'s single
    // transaction exists to prevent one line further down.
    receipt
        .verification
        .gaps
        .retain(|gap| relais::verify::sign_off_gap_criterion(gap) != Some(criterion_id));
    if receipt.accepted_by_settlement() {
        receipt.outcome = State::Accepted.as_str().to_string();
    }
    let receipt_hash = receipt.hash();
    operational(
        ledger.store_receipt(
            run,
            &serde_json::to_value(&receipt).expect("a receipt serializes"),
            &receipt_hash,
        ),
        "decide",
    )?;
    // The ledger row and the file are one receipt. Updating only the row
    // would leave `receipt.json` — the copy a person actually opens, and
    // the one the evidence row's hash names — reading `needs_decision`
    // for a run this command just accepted.
    let receipt_path = runs_dir.join(run.as_str()).join("receipt.json");
    if receipt_path.exists() {
        operational(
            std::fs::write(
                &receipt_path,
                serde_json::to_string_pretty(&receipt).expect("a receipt serializes"),
            ),
            "decide",
        )?;
        operational(
            ledger.record_evidence(
                run,
                None,
                EvidenceKind::Receipt,
                &receipt_path,
                Some(&receipt_hash),
            ),
            "decide",
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::tests::temp_ledger;

    const SIGN_OFF_GAP: &str =
        "acceptance criterion `read` names a human sign-off, which nothing has recorded";

    /// A pending receipt as the runner stores it for a run waiting only on
    /// a sign-off: one criterion settled by the report review, one
    /// waiting on `read`.
    fn pending_receipt(kind: relais::contract::Kind, extra_gap: bool) -> Receipt {
        let criterion = |id: &str, evidence: Option<Evidence>, met: bool, via| {
            relais::verify::CriterionOutcome {
                id: id.into(),
                statement: id.into(),
                mandatory: true,
                met,
                evidence,
                settled_by: None,
                settled_via: Some(via),
            }
        };
        let mut gaps = vec![SIGN_OFF_GAP.to_string()];
        if extra_gap {
            gaps.push("another gap".into());
        }
        Receipt {
            run_id: "run-seal".into(),
            candidate_sha: "abc".into(),
            base_sha: "def".into(),
            contract_hash: "ch".into(),
            policy_hash: "ph".into(),
            outcome: State::NeedsDecision.as_str().into(),
            verification: relais::verify::VerificationReport {
                candidate_sha: "abc".into(),
                base_sha: "def".into(),
                contract_hash: "ch".into(),
                policy_hash: "ph".into(),
                checks: Vec::new(),
                gaps,
                gaps_not_judged: vec!["amont gap, not judged".into()],
                notes: Vec::new(),
                baseline_failures: vec!["base@1".into()],
                amont_bypasses: Vec::new(),
                amont_downgrades: Vec::new(),
                verification_inputs_changed: Vec::new(),
                integration_gaps: Vec::new(),
                baseline_cached: false,
                baseline_cache_refused: None,
            },
            kind,
            candidate: None,
            predecessor: None,
            models_used: vec!["haiku".into()],
            attempts: 1,
            cost_completeness: relais::money::CostCompleteness::Actual,
            cost: MicroUsd::from_micros(1),
            criteria: vec![
                criterion("report", None, true, SettledVia::ReportReview),
                criterion(
                    "read",
                    Some(Evidence::HumanSignOff),
                    false,
                    SettledVia::HumanSignOff,
                ),
            ],
            mandatory_evidence_independence: Some(
                relais::verify::IndependenceSummary::PartlyIndependent,
            ),
            recipe: relais::policy::RecipeRecord::NotRecorded,
            verification_profile_hash: "vph".into(),
            review: relais::verify::ReviewRecord::NoReview,
            ladder: relais::verify::LadderRecord::NotRecorded,
            efforts_used: Vec::new(),
        }
    }

    /// Store `stored` as the pending receipt of a fresh run, beside its
    /// `receipt.json`, re-seal it with the sign-off on `read`, and read
    /// back the ledger's row and the file's outcome.
    fn resealed(stored: &serde_json::Value) -> serde_json::Value {
        let (ledger, dir) = temp_ledger("reseal");
        let run = RunId::from_stored("run-seal");
        let task = relais::ids::TaskId::from_stored("t");
        ledger
            .insert_run(&run, "/repo", None, &task, "rk")
            .expect("run");
        ledger
            .store_receipt(&run, stored, "pending")
            .expect("stored");
        let runs = dir.join("runs");
        let file_path = runs.join("run-seal").join("receipt.json");
        std::fs::create_dir_all(runs.join("run-seal")).expect("run dir");
        std::fs::write(&file_path, stored.to_string()).expect("receipt.json");
        reseal_receipt_in(&ledger, &runs, &run, "read").expect("reseal");
        let (row, _) = ledger.receipt(&run).expect("read").expect("row");
        let file = std::fs::read_to_string(&file_path).expect("receipt.json");
        let file: serde_json::Value = serde_json::from_str(&file).expect("json");
        assert_eq!(
            row["outcome"], file["outcome"],
            "the row and the file agree"
        );
        std::fs::remove_dir_all(&dir).ok();
        row
    }

    #[test]
    fn a_signed_off_inspection_is_accepted_by_its_recorded_settlements() {
        let receipt = pending_receipt(relais::contract::Kind::Inspect, false);
        let row = resealed(&serde_json::to_value(&receipt).expect("receipt"));
        assert_eq!(row["outcome"], "accepted", "{row}");
        let read = &row["criteria"][1];
        assert_eq!(read["met"], true, "{read}");
        assert_eq!(read["settled_via"], "human_sign_off", "{read}");
        assert_eq!(row["criteria"][0]["settled_via"], "report_review");
        assert_eq!(
            row["mandatory_evidence_independence"], "partly_independent",
            "a re-seal recomputes what the runner computed: {row}"
        );
    }

    #[test]
    fn a_signed_off_change_still_needs_every_gap_closed() {
        let open = pending_receipt(relais::contract::Kind::Change, true);
        let row = resealed(&serde_json::to_value(&open).expect("receipt"));
        assert_eq!(row["outcome"], "needs_decision", "{row}");
        let closed = pending_receipt(relais::contract::Kind::Change, false);
        let row = resealed(&serde_json::to_value(&closed).expect("receipt"));
        assert_eq!(row["outcome"], "accepted", "{row}");
    }

    /// A receipt written before `kind` and `settled_via` existed carries
    /// neither: it parses, is a change, and re-seals as it always did.
    #[test]
    fn a_receipt_without_kind_or_settled_via_reseals_as_a_change() {
        let receipt = pending_receipt(relais::contract::Kind::Change, true);
        let mut stored = serde_json::to_value(&receipt).expect("receipt");
        stored.as_object_mut().expect("object").remove("kind");
        for criterion in stored["criteria"].as_array_mut().expect("criteria") {
            criterion
                .as_object_mut()
                .expect("object")
                .remove("settled_via");
        }
        stored["criteria"][0]["settled_by"] = serde_json::json!({
            "test": "suite::case",
            "command": "cargo-test@1",
            "outcome": "passed",
        });
        stored["mandatory_evidence_independence"] = serde_json::json!("all_independent");
        let parsed: Receipt =
            serde_json::from_value(stored.clone()).expect("an old receipt parses");
        assert_eq!(parsed.kind, relais::contract::Kind::Change);
        assert_eq!(parsed.criteria[1].settled_via, None);
        let row = resealed(&stored);
        assert_eq!(
            row["outcome"], "needs_decision",
            "the other gap is still open: {row}"
        );
        assert_eq!(
            row["mandatory_evidence_independence"], "all_independent",
            "no settled_via: today's evidence rule: {row}"
        );
    }
}
