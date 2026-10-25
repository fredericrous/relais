// adr-lint's candidates in the reviewer's prompt.
//
// docs/plans/2026-10-24-code-rules-reach-the-reviewer.md, behaviour 3: the
// reviewer rules on each candidate line, the receipt counts the rulings, and
// each ruling is an `adr-lint.ruling/1` line in `rulings.jsonl`.

use std::sync::Mutex;

use crate::context::candidates::{Candidate, CandidateList};
use crate::context::AvalCli;
use crate::verify::CandidatesRecord;

use super::review_parts::{answer_review_parts, parts_backend, run_stopped_over_budget};
use super::*;

fn candidate_on(path: &str, seed: char) -> Candidate {
    Candidate {
        id: seed.to_string().repeat(64),
        rule: "names.reveal-intent".into(),
        pattern_id: "names.reveal-intent/rust-weak-name".into(),
        pattern: "a weak name".into(),
        path: path.into(),
        line: 2,
        col: 9,
        text: "let data = 1;".into(),
    }
}

/// A resolver whose adr-lint answer is given, counting the calls and keeping
/// the base and head it was asked about.
struct Listing {
    answer: Result<CandidateList, GuidanceGap>,
    asked: Mutex<Vec<(String, String)>>,
}

impl Listing {
    fn of(candidates: Vec<Candidate>) -> Self {
        Self {
            answer: Ok(CandidateList {
                candidates,
                overflow: std::collections::BTreeMap::new(),
            }),
            asked: Mutex::new(Vec::new()),
        }
    }
}

impl crate::context::DecisionResolver for Listing {
    fn resolve(&self, _key: &str, _scope: Option<&str>) -> AvalVerdict {
        AvalVerdict::Unknown
    }

    fn candidates_for(&self, base: &str, head: &str) -> Result<CandidateList, GuidanceGap> {
        self.asked
            .lock()
            .expect("asked")
            .push((base.to_string(), head.to_string()));
        self.answer.clone()
    }
}

/// The ids a reviewer prompt lists between the candidate fences.
fn listed_ids(prompt: &str) -> Vec<String> {
    let Some(start) = prompt.find("<<<CANDIDATES ") else {
        return Vec::new();
    };
    let body = &prompt[start..];
    let body = &body[body.find('\n').expect("a fence line") + 1..];
    body[..body.find("<<<END CANDIDATES>>>").expect("a closing fence")]
        .lines()
        .filter_map(|line| line.split(' ').next())
        .map(str::to_string)
        .collect()
}

/// A backend whose reviewer keeps its prompts and answers with the
/// `Candidates:` block `rule` writes for the ids it was shown, and whose
/// worker is `inner`.
fn ruling_backend(
    inner: MockBackend,
    rule: impl Fn(&[String]) -> String + Send + Sync + 'static,
) -> (MockBackend, Arc<Mutex<Vec<String>>>) {
    let prompts = Arc::new(Mutex::new(Vec::<String>::new()));
    let kept = Arc::clone(&prompts);
    let backend = MockBackend::new(move |spec| {
        if spec.prompt.contains("semantic reviewer") {
            kept.lock().expect("prompts").push(spec.prompt.clone());
            let block = rule(&listed_ids(&spec.prompt));
            return MockOutcome {
                result_text: Some(format!("review done\n{block}\nFINDINGS: none")),
                exit_code: Some(0),
                usage: Some(usage(10)),
                ..Default::default()
            };
        }
        (inner.behavior)(spec)
    });
    (backend, prompts)
}

fn accepted(outcome: RunOutcome) -> Box<crate::verify::Receipt> {
    let Terminal::Accepted(receipt) = outcome.terminal else {
        panic!("expected acceptance after the review, got {outcome:?}");
    };
    receipt
}

fn rulings_lines(fixture: &Fixture, receipt: &crate::verify::Receipt) -> Vec<serde_json::Value> {
    let path = fixture
        .artifacts
        .join(&receipt.run_id)
        .join("rulings.jsonl");
    std::fs::read_to_string(path)
        .expect("rulings.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).expect("a JSON line"))
        .collect()
}

fn is_hex64(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[test]
fn the_reviewer_lists_the_candidates_and_the_receipt_counts_the_rulings() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let listing = Listing::of(vec![
        candidate_on("src/main.rs", 'a'),
        candidate_on("src/main.rs", 'b'),
        candidate_on("src/main.rs", 'c'),
    ]);
    let (backend, prompts) = ruling_backend(conditional_worker("relais task"), |ids| {
        format!(
            "Candidates:\n{} violation: a weak name\n- `{}` fine: \"never silently suppressed\"\n",
            ids[0],
            &ids[1][..12]
        )
    });
    let receipt = accepted(fixture.execute_as_resolving(
        &listing,
        None,
        &fixture.contract(Review::Required),
        &repo,
        &backend,
    ));

    assert_eq!(
        receipt.candidates,
        CandidatesRecord::Counted {
            listed: 3,
            violation: 1,
            fine: 1,
            undisposed: 1,
            overflow: 0
        }
    );
    let asked = listing.asked.lock().expect("asked").clone();
    assert_eq!(
        asked,
        [(receipt.base_sha.clone(), receipt.candidate_sha.clone())]
    );

    let prompts = prompts.lock().expect("prompts");
    let prompt = prompts.first().expect("the reviewer was dispatched");
    let fence = prompt.find("<<<CANDIDATES sha=").expect("the fence");
    assert!(prompt.contains(" n=3>>>\n"), "{prompt}");
    assert!(prompt.contains("not a finding"), "{prompt}");
    assert!(
        fence
            < prompt
                .find("the candidate patch is the file")
                .expect("patch"),
        "the candidates come before the patch section"
    );
    assert!(
        prompt.contains("let data = 1;") && prompt.contains("a weak name"),
        "{prompt}"
    );
    assert!(
        prompt
            .trim_end()
            .ends_with("followed by the list of findings."),
        "the FINDINGS instruction stays last: {prompt}"
    );

    let lines = rulings_lines(&fixture, &receipt);
    assert_eq!(lines.len(), 2);
    let patch = std::fs::read(
        fixture
            .artifacts
            .join(&receipt.run_id)
            .join("candidate-latest.patch"),
    )
    .expect("the patch");
    for line in &lines {
        let fields: Vec<&str> = line
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            fields,
            [
                "at",
                "candidate",
                "clause",
                "format",
                "pattern",
                "pattern_id",
                "repo",
                "review",
                "rule",
                "ruling",
                "tool",
                "tree"
            ]
        );
        assert_eq!(line["format"], "adr-lint.ruling/1");
        assert_eq!(line["tool"], "relais");
        assert!(is_hex64(line["candidate"].as_str().expect("candidate")));
        assert_eq!(line["tree"], crate::ids::sha256_hex(&patch));
        assert!(is_hex64(line["tree"].as_str().expect("tree")));
        assert_eq!(line["rule"], "names.reveal-intent");
        assert_eq!(
            line["repo"],
            fixture
                .repo
                .file_name()
                .expect("a name")
                .to_str()
                .expect("utf8")
        );
        let at = line["at"].as_str().expect("at");
        assert!(
            chrono::NaiveDateTime::parse_from_str(at, "%Y-%m-%dT%H:%M:%SZ").is_ok(),
            "{at}"
        );
        assert!(!line["review"].as_str().expect("review").is_empty());
    }
    assert_eq!(lines[0]["ruling"], "violation");
    assert_eq!(lines[0]["clause"], "a weak name");
    assert_eq!(lines[1]["ruling"], "fine");
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn no_candidates_adds_nothing_to_the_prompt() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let listing = Listing::of(Vec::new());
    let (backend, prompts) = ruling_backend(conditional_worker("relais task"), |_| String::new());
    let receipt = accepted(fixture.execute_as_resolving(
        &listing,
        None,
        &fixture.contract(Review::Required),
        &repo,
        &backend,
    ));
    let prompts = prompts.lock().expect("prompts");
    let prompt = prompts.first().expect("the reviewer was dispatched");
    assert!(
        !prompt.contains("CANDIDATES") && !prompt.contains("adr-lint"),
        "{prompt}"
    );
    assert_eq!(
        receipt.candidates,
        CandidatesRecord::Counted {
            listed: 0,
            violation: 0,
            fine: 0,
            undisposed: 0,
            overflow: 0
        }
    );
    assert!(!fixture
        .artifacts
        .join(&receipt.run_id)
        .join("rulings.jsonl")
        .exists());
    std::fs::remove_dir_all(&fixture.dir).ok();
}

/// A resolver that asks the real adapter for adr-lint's candidates, with
/// `program` standing in for adr-lint.
struct SpawningAdrLint(AvalCli);

impl crate::context::DecisionResolver for SpawningAdrLint {
    fn resolve(&self, _key: &str, _scope: Option<&str>) -> AvalVerdict {
        AvalVerdict::Unknown
    }

    fn candidates_for(&self, base: &str, head: &str) -> Result<CandidateList, GuidanceGap> {
        self.0.candidates_for(base, head)
    }
}

fn fake_adr_lint(fixture: &Fixture, body: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = fixture.dir.join("fake-adr-lint");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write the script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    // The adapter's 5 s guidance timeout must not pay a fresh script's first exec.
    crate::test_support::prime_script(&path);
    path.to_string_lossy().into_owned()
}

#[test]
fn an_adr_lint_that_fails_or_is_missing_is_a_receipt_note_and_the_review_still_runs() {
    for (script, detail) in [
        (Some("exit 3"), "adr-lint exit 3"),
        (None, "adr-lint could not be launched"),
    ] {
        let fixture = Fixture::new();
        let repo = fixture.repo_policy(vec![main_gone_check()], 3);
        let program = match script {
            Some(body) => fake_adr_lint(&fixture, body),
            None => "/nonexistent/relais-adr-lint".to_string(),
        };
        let resolver = SpawningAdrLint(AvalCli::new(fixture.repo.clone()).with_adr_lint(program));
        let (backend, prompts) =
            ruling_backend(conditional_worker("relais task"), |_| String::new());
        let receipt = accepted(fixture.execute_as_resolving(
            &resolver,
            None,
            &fixture.contract(Review::Required),
            &repo,
            &backend,
        ));
        let note = receipt
            .verification
            .notes
            .iter()
            .find(|note| note.starts_with("guidance:"))
            .expect("a guidance line");
        assert!(note.contains("candidates missing ("), "{note}");
        assert!(note.contains(detail), "{note}");
        let prompts = prompts.lock().expect("prompts");
        let prompt = prompts.first().expect("the reviewer was still dispatched");
        assert!(
            prompt.contains("adr-lint's candidates are unavailable (")
                && prompt.contains("): no ruling is needed"),
            "{prompt}"
        );
        assert!(!prompt.contains("report as uncertainty"), "{prompt}");
        assert!(matches!(
            receipt.candidates,
            CandidatesRecord::Unavailable { .. }
        ));
        std::fs::remove_dir_all(&fixture.dir).ok();
    }
}

#[test]
fn the_real_adapter_spawns_adr_lint_with_the_runs_base_and_the_candidate() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let argv = fixture.dir.join("argv.txt");
    let json = r#"{"format":"adr-lint.candidates/1","candidates":[],"overflow":{"names.reveal-intent":2}}"#;
    let program = fake_adr_lint(
        &fixture,
        &format!("echo \"$@\" > '{}'\nprintf '%s' '{json}'", argv.display()),
    );
    let resolver = SpawningAdrLint(AvalCli::new(fixture.repo.clone()).with_adr_lint(program));
    let (backend, _) = ruling_backend(conditional_worker("relais task"), |_| String::new());
    let receipt = accepted(fixture.execute_as_resolving(
        &resolver,
        None,
        &fixture.contract(Review::Required),
        &repo,
        &backend,
    ));
    assert_eq!(
        std::fs::read_to_string(&argv).expect("argv").trim(),
        format!(
            "candidates --base {} --head {} --json --max 40",
            receipt.base_sha, receipt.candidate_sha
        )
    );
    assert_eq!(
        receipt.candidates,
        CandidatesRecord::Counted {
            listed: 0,
            violation: 0,
            fine: 0,
            undisposed: 0,
            overflow: 2
        }
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_review_in_two_parts_asks_adr_lint_once_and_lists_each_candidate_in_one_part() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let run = run_stopped_over_budget(&fixture, 2, 2300);
    let (contract, new_run) = answer_review_parts(&fixture, &run, State::NeedsDecision);
    let (parts, _) = parts_backend(2, 2300, None, None);
    let (backend, prompts) = ruling_backend(parts.inner, |ids| {
        let lines: Vec<String> = ids
            .iter()
            .map(|id| format!("{id} fine: \"a quoted clause\""))
            .collect();
        format!("Candidates:\n{}\n", lines.join("\n"))
    });
    let listing = Listing::of(vec![
        candidate_on("src/big0.txt", 'a'),
        candidate_on("src/big1.txt", 'b'),
    ]);

    let receipt =
        accepted(fixture.execute_as_resolving(&listing, Some(new_run), &contract, &repo, &backend));

    assert_eq!(listing.asked.lock().expect("asked").len(), 1);
    let prompts = prompts.lock().expect("prompts");
    let per_part: Vec<Vec<String>> = prompts.iter().map(|p| listed_ids(p)).collect();
    assert_eq!(
        per_part,
        [vec!["a".repeat(64)], vec!["b".repeat(64)]],
        "each candidate is listed in exactly one part"
    );
    assert_eq!(
        receipt.candidates,
        CandidatesRecord::Counted {
            listed: 2,
            violation: 0,
            fine: 2,
            undisposed: 0,
            overflow: 0
        }
    );
    assert_eq!(rulings_lines(&fixture, &receipt).len(), 2);
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_candidates_block_before_the_findings_line_changes_neither_verdict_nor_severities() {
    let block = "Candidates:\n- aaaaaaaaaaaa violation: 1) odd\n";
    let findings = "FINDINGS:\n1. [high] a.rs:1 — d\n2. [low] b.rs — e\n";
    let none = "FINDINGS: none";
    for (bare, with) in [
        (findings.to_string(), format!("{block}\n{findings}")),
        (none.to_string(), format!("{block}\n{none}")),
    ] {
        assert_eq!(
            super::super::review::finding_severities(&bare),
            super::super::review::finding_severities(&with)
        );
        assert_eq!(
            matches!(
                super::super::review::review_verdict(&bare),
                ReviewOutcome::NoFindings
            ),
            matches!(
                super::super::review::review_verdict(&with),
                ReviewOutcome::NoFindings
            )
        );
        assert_eq!(
            matches!(
                super::super::review::review_verdict(&bare),
                ReviewOutcome::Findings(_)
            ),
            matches!(
                super::super::review::review_verdict(&with),
                ReviewOutcome::Findings(_)
            )
        );
    }
}

#[test]
fn a_receipt_without_the_candidates_field_still_parses() {
    let receipt = crate::verify::Receipt {
        candidates: CandidatesRecord::NotRecorded,
        ..sample_receipt()
    };
    let mut json = serde_json::to_value(&receipt).expect("serializes");
    assert!(json.get("candidates").is_none(), "{json}");
    json.as_object_mut().expect("object").remove("candidates");
    let back: crate::verify::Receipt = serde_json::from_value(json).expect("parses");
    assert_eq!(back.candidates, CandidatesRecord::NotRecorded);
    let counted = CandidatesRecord::Counted {
        listed: 3,
        violation: 1,
        fine: 1,
        undisposed: 1,
        overflow: 0,
    };
    let text = serde_json::to_string(&counted).expect("serializes");
    assert_eq!(
        serde_json::from_str::<CandidatesRecord>(&text).expect("parses"),
        counted
    );
}

fn sample_receipt() -> crate::verify::Receipt {
    use crate::verify::{LadderRecord, ReviewRecord, VerificationReport};
    crate::verify::Receipt {
        run_id: "run-1".into(),
        candidate_sha: "abc".into(),
        base_sha: "def".into(),
        contract_hash: "ch".into(),
        policy_hash: "ph".into(),
        outcome: "accepted".into(),
        verification: VerificationReport {
            candidate_sha: "abc".into(),
            base_sha: "def".into(),
            contract_hash: "ch".into(),
            policy_hash: "ph".into(),
            checks: vec![],
            gaps: vec![],
            baseline_failures: vec![],
            gaps_not_judged: Vec::new(),
            notes: Vec::new(),
            amont_bypasses: vec![],
            amont_downgrades: vec![],
            verification_inputs_changed: Vec::new(),
            integration_gaps: Vec::new(),
            baseline_cached: false,
            baseline_cache_refused: None,
        },
        kind: crate::contract::Kind::Change,
        candidate: None,
        predecessor: None,
        models_used: vec![],
        attempts: 1,
        cost_completeness: crate::money::CostCompleteness::Actual,
        cost: crate::money::MicroUsd::from_micros(1),
        criteria: Vec::new(),
        mandatory_evidence_independence: None,
        recipe: crate::policy::RecipeRecord::NotRecorded,
        verification_profile_hash: "vph".into(),
        review: ReviewRecord::NotRecorded,
        ladder: LadderRecord::NotRecorded,
        efforts_used: Vec::new(),
        candidates: CandidatesRecord::NotRecorded,
    }
}
