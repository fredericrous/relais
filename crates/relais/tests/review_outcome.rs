//! `relais native review-outcome` and the report's `Reviewers` section,
//! through the real binary: amont-agent's outcomes ledger read behind the
//! trust boundary, each reviewer labelled, the labels kept in the relais
//! ledger. Unix only: the boundary is file modes and owners.
#![cfg(unix)]

mod common;

use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::PathBuf;
use std::process::Output;

use relais::ledger::Ledger;
use relais::test_support::short_temp_dir;
use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_relais");

const BODY1: &str = "# Plan\n\n## Behaviour\n\nOne.\n\n## Phases\n\nTwo.\n";
const BODY2: &str = "# Plan\n\n## Behaviour\n\nOne.\n\n## Phases\n\nTwo, changed.\n";
const SHA1: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const SHA2: &str = "2222222222222222222222222222222222222222222222222222222222222222";

struct World {
    _scratch: relais::test_support::TempDir,
    root: PathBuf,
    state: PathBuf,
    /// amont-agent's `plan-review` directory: the ledger and `bodies/`.
    reviews: PathBuf,
    plans: PathBuf,
}

fn private(path: &PathBuf, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

impl World {
    fn new(tag: &str) -> Self {
        let scratch = short_temp_dir(&format!("ro-{tag}"));
        let root = scratch.to_path_buf();
        let state = root.join("state");
        let reviews = root.join("claude/amont-agent/plan-review");
        let plans = root.join("plans");
        for dir in [
            &state,
            &reviews.join("bodies"),
            &plans.join("relais/docs/plans"),
        ] {
            std::fs::create_dir_all(dir).expect("dir");
        }
        Self {
            _scratch: scratch,
            root,
            state,
            reviews,
            plans,
        }
    }

    fn ledger_file(&self) -> PathBuf {
        self.reviews.join("outcomes.jsonl")
    }

    fn row(agent: &str, role: &str, round: u32, body: &str, findings: Value) -> Value {
        json!({
            "schema": "amont-agent-review-outcome-v1", "recorded_at": 5, "role": role,
            "agent_id": agent, "model": "claude-fable-5-1", "tokens": 120000, "seconds": 3.5,
            "round": round, "body_sha": body, "first_round_sha": SHA1,
            "plan_path": "/p/plan.md",
            "plan": {"areas": ["cli"], "repos": ["relais"], "body_lines": 4},
            "verdict": "approve-with-changes",
            "remit": if role == "backend" { json!(["*"]) } else { json!(["cli"]) },
            "findings": findings,
        })
    }

    fn finding(role: &str, section: &str) -> Value {
        json!({"role": role, "severity": "high", "disposition": "accepted",
               "section": section, "area": "cli"})
    }

    fn write_ledger(&self, lines: &[String]) {
        let path = self.ledger_file();
        std::fs::write(&path, lines.join("\n") + "\n").expect("ledger");
        private(&path, 0o600);
    }

    fn write_body(&self, sha: &str, text: &str) {
        let path = self.reviews.join("bodies").join(format!("{sha}.md"));
        std::fs::write(&path, text).expect("body");
        private(&path, 0o600);
    }

    /// The plan landed: a plan file stamped with the later body's sha.
    fn push_plan(&self) {
        std::fs::write(
            self.plans.join("relais/docs/plans/p.md"),
            format!("body-sha={}\n", &SHA2[..12]),
        )
        .expect("plan file");
    }

    /// A panel: a `rust` reviewer in round 1 and the backend in round 2,
    /// who raises `Behaviour` (unchanged since round 1) as high.
    fn panel(&self) {
        self.write_ledger(&[
            Self::row("rust-1", "rust", 1, SHA1, json!([])).to_string(),
            Self::row(
                "be-2",
                "backend",
                2,
                SHA2,
                json!([Self::finding("backend", "Behaviour")]),
            )
            .to_string(),
        ]);
        self.write_body(SHA1, BODY1);
        self.write_body(SHA2, BODY2);
        self.push_plan();
    }

    fn run(&self, args: &[&str]) -> Output {
        let ledger = self.ledger_file();
        let mut all = vec!["native", "review-outcome"];
        all.extend_from_slice(args);
        all.extend_from_slice(&[
            "--ledger",
            ledger.to_str().unwrap(),
            "--plan-root",
            self.plans.to_str().unwrap(),
        ]);
        self.relais(&all)
    }

    fn relais(&self, args: &[&str]) -> Output {
        common::relais(BIN)
            .args(args)
            .current_dir(&self.root)
            .env("RELAIS_STATE_DIR", &self.state)
            .env("RELAIS_CONFIG_DIR", self.root.join("cfg"))
            .env("HOME", &self.root)
            .env("USERPROFILE", &self.root)
            .env_remove("CLAUDE_CONFIG_DIR")
            .output()
            .expect("relais runs")
    }

    fn ledger(&self) -> Ledger {
        Ledger::open(&self.state.join("ledger.sqlite")).expect("ledger")
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn a_missed_high_finding_labels_the_earlier_reviewer_a_miss() {
    let world = World::new("miss");
    world.panel();
    let o = world.run(&["--dry-run"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let text = out(&o);
    assert!(text.contains("rust-1 rust round 1 miss\n"), "{text}");
    assert!(
        text.contains("be-2 backend round 2 unknown no_later_review\n"),
        "{text}"
    );
    assert!(
        text.contains("labelled 2: clean 0, miss 1, unknown 1 (not_pushed 0, no_later_review 1, missing_body 0)"),
        "{text}"
    );
}

#[test]
fn the_same_section_raised_by_the_role_is_clean() {
    let world = World::new("clean");
    world.write_ledger(&[
        World::row(
            "rust-1",
            "rust",
            1,
            SHA1,
            json!([World::finding("rust", "Behaviour")]),
        )
        .to_string(),
        World::row(
            "be-2",
            "backend",
            2,
            SHA2,
            json!([World::finding("backend", "Behaviour")]),
        )
        .to_string(),
    ]);
    world.write_body(SHA1, BODY1);
    world.write_body(SHA2, BODY2);
    world.push_plan();
    let text = out(&world.run(&["--dry-run"]));
    assert!(text.contains("rust-1 rust round 1 clean\n"), "{text}");
}

#[test]
fn an_unpushed_plan_and_a_missing_body_are_unknown_with_their_reasons() {
    let world = World::new("unknown");
    world.panel();
    std::fs::remove_file(world.plans.join("relais/docs/plans/p.md")).expect("unpush");
    let text = out(&world.run(&["--dry-run"]));
    assert!(
        text.contains("rust-1 rust round 1 unknown not_pushed\n"),
        "{text}"
    );

    world.push_plan();
    std::fs::remove_file(world.reviews.join("bodies").join(format!("{SHA1}.md"))).expect("rm");
    let text = out(&world.run(&["--dry-run"]));
    assert!(
        text.contains("rust-1 rust round 1 unknown missing_body\n"),
        "{text}"
    );
}

#[test]
fn a_body_with_group_access_counts_as_missing_not_as_a_refusal() {
    let world = World::new("body-mode");
    world.panel();
    private(
        &world.reviews.join("bodies").join(format!("{SHA1}.md")),
        0o644,
    );
    let o = world.run(&["--dry-run"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(
        out(&o).contains("rust-1 rust round 1 unknown missing_body"),
        "{}",
        out(&o)
    );
}

#[test]
fn ingesting_twice_leaves_the_same_rows_and_dry_run_writes_nothing() {
    let world = World::new("upsert");
    world.panel();
    let o = world.run(&["--dry-run"]);
    assert_eq!(o.status.code(), Some(0));
    assert!(
        !world.state.join("ledger.sqlite").exists(),
        "a dry run does not even create the ledger"
    );
    for _ in 0..2 {
        let o = world.run(&[]);
        assert_eq!(o.status.code(), Some(0), "{}", err(&o));
        assert!(
            out(&o).starts_with("labelled 2: clean 0, miss 1, unknown 1"),
            "{}",
            out(&o)
        );
    }
    let totals = world.ledger().review_class_totals().expect("totals");
    let counts: Vec<_> = totals
        .iter()
        .map(|t| (t.class.as_str(), t.clean, t.miss, t.unknown))
        .collect();
    assert_eq!(counts, vec![("backend", 0, 0, 1), ("rust", 0, 1, 0)]);
}

#[test]
fn invalid_rows_are_counted_and_skipped() {
    let world = World::new("invalid");
    let mut other = World::row("x", "rust", 1, SHA1, json!([]));
    other["schema"] = json!("amont-agent-review-outcome-v2");
    let mut extra = World::row("y", "rust", 1, SHA1, json!([]));
    extra["surprise"] = json!(true);
    world.write_ledger(&[
        "not json".into(),
        other.to_string(),
        extra.to_string(),
        World::row("z", "rust", 1, SHA1, json!([])).to_string(),
    ]);
    let o = world.run(&["--dry-run"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(
        out(&o).contains("labelled 1: clean 0, miss 0, unknown 1, invalid 3 ("),
        "{}",
        out(&o)
    );
}

#[test]
fn a_missing_ledger_is_exit_0_and_says_so() {
    let world = World::new("absent");
    let o = world.run(&[]);
    assert_eq!(o.status.code(), Some(0));
    assert!(
        out(&o).starts_with("no reviewer outcomes at "),
        "{}",
        out(&o)
    );
}

#[test]
fn a_symlinked_or_group_readable_ledger_is_refused_with_exit_3() {
    let world = World::new("trust");
    world.panel();
    private(&world.ledger_file(), 0o644);
    let o = world.run(&[]);
    assert_eq!(o.status.code(), Some(3), "{}", err(&o));
    assert!(
        err(&o).contains("group and other access must be off"),
        "{}",
        err(&o)
    );

    private(&world.ledger_file(), 0o600);
    let real = world.reviews.join("real.jsonl");
    std::fs::rename(world.ledger_file(), &real).expect("move");
    symlink(&real, world.ledger_file()).expect("symlink");
    let o = world.run(&[]);
    assert_eq!(o.status.code(), Some(3), "{}", err(&o));
    assert!(err(&o).contains("symlink"), "{}", err(&o));
    assert!(
        !world.state.join("ledger.sqlite").exists(),
        "a refused ledger writes nothing"
    );
    // A file owned by another user is refused the same way
    // (`meta.uid() != procs::uid()`); chown needs root, which a test
    // run does not have, so that branch is not exercised here.
}

#[test]
fn the_report_has_an_empty_state_then_rows_within_80_columns() {
    let world = World::new("report");
    let o = world.relais(&["report", "--no-import"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(
        out(&o).contains("no reviewer outcomes yet: relais learn cycle ingests them"),
        "{}",
        out(&o)
    );
    let json = world.relais(&["report", "--no-import", "--json"]);
    let doc: Value = serde_json::from_str(&out(&json)).expect("json");
    assert_eq!(doc["review_outcomes"]["empty"], true, "{doc}");

    world.panel();
    assert_eq!(world.run(&[]).status.code(), Some(0));
    let o = world.relais(&["report", "--no-import"]);
    let text = out(&o);
    let row = text
        .lines()
        .find(|l| l.contains("rust "))
        .expect("a rust row");
    assert!(
        row.contains("fable-5-1 120k") && text.contains("every class still learning"),
        "{row}"
    );
    assert!(text.contains("tokens by model"), "{text}");
    assert!(text.lines().all(|l| l.chars().count() <= 80), "{text}");

    let json = world.relais(&["report", "--no-import", "--json"]);
    let doc: Value = serde_json::from_str(&out(&json)).expect("json");
    let section = &doc["review_outcomes"];
    assert_eq!(section["empty"], false, "{doc}");
    assert_eq!(section["more"], 0);
    let rust = section["classes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["class"] == "rust")
        .expect("rust class");
    assert_eq!(rust["n"], 1);
    assert_eq!(rust["miss"], 1);
    assert_eq!(rust["tokens_by_model"]["claude-fable-5-1"], 120000);
}

#[test]
fn a_row_whose_body_was_never_saved_parses_and_borrows_the_plans_repos() {
    // amont-agent writes `"plan": null` for a body it never saved; the
    // plan's other rows still say where it would have landed.
    let world = World::new("null-plan");
    let mut rust = World::row("rust-1", "rust", 1, SHA1, json!([]));
    rust["plan"] = Value::Null;
    world.write_ledger(&[
        rust.to_string(),
        World::row(
            "be-2",
            "backend",
            2,
            SHA2,
            json!([World::finding("backend", "Behaviour")]),
        )
        .to_string(),
    ]);
    world.write_body(SHA1, BODY1);
    world.write_body(SHA2, BODY2);
    world.push_plan();
    let o = world.run(&["--dry-run"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(
        out(&o).contains("rust-1 rust round 1 miss\n"),
        "{}",
        out(&o)
    );
    assert!(!out(&o).contains("invalid"), "{}", out(&o));
}
