//! `relais dataset seed` through the real binary: the seeded state reports
//! exactly what the file described, and every refusal leaves the state
//! directory as it was (docs/plans/2026-10-30-public-relais-seed-command.md).

use crate::common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use relais::paths::{LEDGER_FILE, SHM_SUFFIX, WAL_SUFFIX};
use relais::test_support::short_temp_dir;
use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_relais");

struct World {
    _scratch: relais::test_support::TempDir,
    root: PathBuf,
    state: PathBuf,
    _permit: common::WorldPermit,
}

impl World {
    fn new(tag: &str) -> Self {
        let permit = common::WorldPermit::acquire(1);
        let scratch = short_temp_dir(&format!("sd-{tag}"));
        let root = scratch.to_path_buf();
        std::fs::create_dir_all(root.join("home")).expect("home");
        Self {
            state: root.join("state"),
            root,
            _scratch: scratch,
            _permit: permit,
        }
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    fn command(&self, state: Option<&Path>) -> std::process::Command {
        let mut command = common::relais(BIN);
        command
            .current_dir(&self.root)
            .env("RELAIS_CONFIG_DIR", self.root.join("cfg"))
            .env("HOME", self.home())
            .env("USERPROFILE", self.home())
            .env_remove("CLAUDE_CONFIG_DIR");
        if let Some(state) = state {
            command.env("RELAIS_STATE_DIR", state);
        }
        command
    }

    fn seed_file(&self, text: &str) -> PathBuf {
        let path = self.root.join("seed.jsonl");
        std::fs::write(&path, text).expect("seed file");
        path
    }

    fn seed(&self, state: Option<&Path>, args: &[&str]) -> Output {
        self.command(state)
            .args(["dataset", "seed"])
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("relais runs")
    }

    fn seed_stdin(&self, state: &Path, text: &str) -> Output {
        let mut child = self
            .command(Some(state))
            .args(["dataset", "seed", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("relais runs");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(text.as_bytes())
            .expect("write stdin");
        child.wait_with_output().expect("output")
    }

    fn report(&self, state: &Path, extra: &[&str]) -> Value {
        let output = self
            .command(Some(state))
            .args(["report", "--json", "--no-import"])
            .args(extra)
            .output()
            .expect("relais runs");
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
        serde_json::from_slice(&output.stdout).expect("report json")
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

/// Every file in `dir` with its bytes, for before/after comparison.
fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<(String, Vec<u8>)> = entries
        .map(|entry| {
            let entry = entry.expect("entry");
            (
                entry.file_name().to_string_lossy().into_owned(),
                std::fs::read(entry.path()).unwrap_or_default(),
            )
        })
        .collect();
    files.sort();
    files
}

const ONE_RUN: &str =
    r#"{"run":"r1","task":"t1","created_at":"2026-10-01T00:00:00Z","status":"failed"}"#;

/// Whether the run asked for `--json`, in which case stdout must stay empty.
#[derive(Clone, Copy)]
enum Mode {
    Human,
    Json,
}

fn expect_refused(output: &Output, mode: Mode, needles: &[&str]) {
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    match mode {
        Mode::Json => assert_eq!(text(&output.stdout), "", "stdout must stay empty"),
        Mode::Human => {}
    }
    for needle in needles {
        assert!(stderr.contains(needle), "{needle} not in {stderr}");
    }
}

#[test]
fn a_seed_reports_exactly_what_the_file_described() {
    let world = World::new("counts");
    let lines = [
        r#"{"seed_schema": 1}"#,
        r#"{"run":"r-relais","task":"t1","created_at":"2026-10-01T00:00:00Z","status":"accepted","accepted_by":"relais","cost_micros":120000,"holdouts":[{"name":"report-json","outcome":"failed","profile_green":true,"source":"named"}],"exposures":[{"marker":"store"}]}"#,
        r#"{"run":"r-person","task":"t2","created_at":"2026-10-02T00:00:00Z","status":"accepted","accepted_by":"person","holdouts":[{"name":"att","outcome":"passed","profile_green":true,"source":"attached"}],"exposures":[{"marker":"machine_toml"}]}"#,
        r#"{"run":"r-failed","task":"t3","created_at":"2026-10-03T00:00:00Z","status":"failed"}"#,
        r#"{"run":"r-blocked","task":"t4","created_at":"2026-10-03T00:00:00Z","status":"blocked"}"#,
        r#"{"run":"r-decision","task":"t5","created_at":"2026-10-03T00:00:00Z","status":"needs_decision","cost_micros":30000}"#,
        r#"{"run":"r-review","task":"t6","created_at":"2026-10-03T00:00:00Z","status":"needs_review"}"#,
        r#"{"run":"r-interrupted","task":"t7","created_at":"2026-10-03T00:00:00Z","status":"interrupted"}"#,
        r#"{"run":"r-budget","task":"t8","created_at":"2026-10-03T00:00:00Z","status":"budget_exhausted"}"#,
        r#"{"run":"r-cancelled","task":"t9","created_at":"2026-10-03T00:00:00Z","status":"cancelled","holdouts":[{"name":"skipped-one","outcome":"skipped","profile_green":false,"source":"attached"}]}"#,
    ];
    let file = world.seed_file(&lines.join("\n"));
    let output = world.seed(Some(&world.state), &[&file.to_string_lossy(), "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let printed: Value = serde_json::from_slice(&output.stdout).expect("seed json");
    assert_eq!(printed["seeded"], 9);
    assert_eq!(
        printed["ledger"],
        world.state.join(LEDGER_FILE).to_string_lossy().as_ref()
    );

    let report = world.report(&world.state, &["--since", "2000-01-01"]);
    let mut statuses: Vec<String> = report["runs"]
        .as_array()
        .expect("runs")
        .iter()
        .map(|run| run["status"].as_str().expect("status").to_string())
        .collect();
    statuses.sort();
    assert_eq!(
        statuses,
        [
            "accepted",
            "accepted",
            "blocked",
            "budget_exhausted",
            "cancelled",
            "failed",
            "interrupted",
            "needs_decision",
            "needs_review"
        ]
    );
    assert_eq!(report["accepted_tasks"], 2, "{report}");
    assert_eq!(report["accepted_tasks_by_relais"], 1, "{report}");
    assert_eq!(report["accepted_tasks_by_person"], 1, "{report}");
    assert_eq!(report["pending_decisions"], 3, "{report}");
    assert_eq!(report["total_cost"], 150_000, "{report}");

    let holdouts = &report["holdouts"];
    assert_eq!(holdouts["holdout_runs"], 2, "{report}");
    assert_eq!(holdouts["holdout_catches"], 1, "{report}");
    assert_eq!(holdouts["holdout_exposures"]["runs"], 2, "{report}");
    assert_eq!(holdouts["holdout_exposures"]["by_marker"]["store"], 1);
    assert_eq!(
        holdouts["holdout_exposures"]["by_marker"]["machine_toml"],
        1
    );
    assert_eq!(holdouts["by_source"]["named"]["runs"], 1, "{report}");
    assert_eq!(holdouts["by_source"]["named"]["catches"], 1, "{report}");
    assert_eq!(holdouts["by_source"]["attached"]["runs"], 1, "{report}");
    assert_eq!(holdouts["by_source"]["attached"]["catches"], 0, "{report}");
    assert_eq!(holdouts["by_source"]["attached"]["skipped"], 1, "{report}");

    assert!(
        !world
            .state
            .join(format!("{LEDGER_FILE}{WAL_SUFFIX}"))
            .exists(),
        "no -wal beside the seeded ledger"
    );
}

#[test]
fn the_default_since_counts_only_the_current_month() {
    let world = World::new("since");
    let now = chrono::Utc::now();
    let this_month = now.format("%Y-%m-01T12:00:00Z").to_string();
    let old = (now - chrono::Duration::days(70))
        .format("%Y-%m-%dT12:00:00Z")
        .to_string();
    let file = world.seed_file(&format!(
        "{{\"run\":\"new\",\"task\":\"t1\",\"created_at\":\"{this_month}\",\"status\":\"failed\"}}\n\
         {{\"run\":\"old\",\"task\":\"t2\",\"created_at\":\"{old}\",\"status\":\"failed\"}}\n"
    ));
    let output = world.seed(Some(&world.state), &[&file.to_string_lossy()]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert!(
        text(&output.stderr).contains("relais dataset seed: seeded 2 runs into "),
        "{}",
        text(&output.stderr)
    );
    let report = world.report(&world.state, &[]);
    let ids: Vec<&str> = report["runs"]
        .as_array()
        .expect("runs")
        .iter()
        .map(|run| run["run_id"].as_str().expect("id"))
        .collect();
    assert_eq!(ids, ["new"]);
}

#[test]
fn an_unset_state_dir_is_refused() {
    let world = World::new("unset");
    let file = world.seed_file(ONE_RUN);
    let output = world.seed(None, &[&file.to_string_lossy(), "--json"]);
    expect_refused(
        &output,
        Mode::Json,
        &["RELAIS_STATE_DIR is not set", "mktemp -d"],
    );
    assert!(!world.home().join(".local").exists(), "nothing was created");
}

#[test]
fn the_default_state_dir_is_refused_however_it_is_spelled() {
    let world = World::new("default");
    let file = world.seed_file(ONE_RUN);
    let default = world.home().join(".local/state/relais");
    let with_slash = PathBuf::from(format!("{}/", default.display()));
    let with_dots = world.home().join(".local/state/../state/relais");
    for spelled in [default.clone(), with_slash, with_dots] {
        let output = world.seed(Some(&spelled), &[&file.to_string_lossy(), "--json"]);
        expect_refused(
            &output,
            Mode::Json,
            &["default state directory", "mktemp -d"],
        );
    }
    assert!(!default.join(LEDGER_FILE).exists());

    #[cfg(unix)]
    {
        std::fs::create_dir_all(&default).expect("default dir");
        let link = world.root.join("link");
        std::os::unix::fs::symlink(&default, &link).expect("symlink");
        let output = world.seed(Some(&link), &[&file.to_string_lossy(), "--json"]);
        expect_refused(&output, Mode::Json, &["default state directory"]);
        assert!(snapshot(&default).is_empty());
    }
}

#[test]
fn an_existing_ledger_file_is_refused_and_left_alone() {
    let cases: [(&str, String, &[u8]); 4] = [
        ("empty", LEDGER_FILE.to_string(), b""),
        (
            "full",
            LEDGER_FILE.to_string(),
            b"SQLite format 3\0not really",
        ),
        ("wal", format!("{LEDGER_FILE}{WAL_SUFFIX}"), b"wal"),
        ("shm", format!("{LEDGER_FILE}{SHM_SUFFIX}"), b"shm"),
    ];
    for (tag, name, bytes) in cases {
        let world = World::new(&format!("exists-{tag}"));
        std::fs::create_dir_all(&world.state).expect("state");
        std::fs::write(world.state.join(&name), bytes).expect("existing");
        let before = snapshot(&world.state);
        let file = world.seed_file(ONE_RUN);
        let output = world.seed(Some(&world.state), &[&file.to_string_lossy(), "--json"]);
        expect_refused(
            &output,
            Mode::Json,
            &[&name, "fresh directory", "mktemp -d"],
        );
        assert_eq!(snapshot(&world.state), before, "{tag}");
    }
}

#[test]
fn every_bad_line_is_reported_then_the_summary() {
    let world = World::new("badlines");
    let body = format!(
        "{ONE_RUN}\nnot json\n{{\"run\":\"r2\",\"task\":\"t2\",\"status\":\"sleeping\"}}\n"
    );
    let file = world.seed_file(&body);
    let output = world.seed(Some(&world.state), &[&file.to_string_lossy(), "--json"]);
    let label = file.display().to_string();
    expect_refused(
        &output,
        Mode::Json,
        &[
            &format!("{label}:2: "),
            &format!("{label}:3: unknown status `sleeping`"),
            "relais dataset seed: 2 bad line(s); nothing was written",
        ],
    );
    assert_eq!(
        text(&output.stderr).lines().last(),
        Some("relais dataset seed: 2 bad line(s); nothing was written")
    );
    assert!(!world.state.exists() || snapshot(&world.state).is_empty());

    let output = world.seed_stdin(&world.state, &body);
    expect_refused(
        &output,
        Mode::Human,
        &["<stdin>:2: ", "<stdin>:3: ", "2 bad line(s)"],
    );
    assert_eq!(
        text(&output.stderr).lines().last(),
        Some("relais dataset seed: 2 bad line(s); nothing was written")
    );
    assert!(!world.state.exists() || snapshot(&world.state).is_empty());
}

#[test]
fn whole_file_validation_refuses_each_kind_of_bad_content() {
    let cases: [(&str, String, &str); 7] = [
        (
            "duplicate-after-refused",
            format!(
                "{}\n{ONE_RUN}",
                r#"{"run":"r1","task":"t","status":"sleeping"}"#
            ),
            "2: duplicate run id `r1`",
        ),
        (
            "duplicate-run",
            format!("{ONE_RUN}\n{ONE_RUN}"),
            "2: duplicate run id `r1`",
        ),
        (
            "duplicate-holdout",
            r#"{"run":"r1","task":"t","status":"failed","holdouts":[{"name":"h","outcome":"passed","source":"named"},{"name":"h","outcome":"failed","source":"attached"}]}"#.to_string(),
            "1: duplicate holdout name `h`",
        ),
        (
            "bad-status",
            r#"{"run":"r1","task":"t","status":"done"}"#.to_string(),
            "1: unknown status `done`",
        ),
        (
            "bad-outcome",
            r#"{"run":"r1","task":"t","status":"failed","holdouts":[{"name":"h","outcome":"meh"}]}"#.to_string(),
            "1: unknown holdout outcome `meh`",
        ),
        (
            "schema-2",
            format!("{{\"seed_schema\": 2}}\n{ONE_RUN}"),
            "1: unsupported seed_schema 2",
        ),
        (
            "accepted-by-on-failed",
            r#"{"run":"r1","task":"t","status":"failed","accepted_by":"person"}"#.to_string(),
            "1: accepted_by applies to status accepted only",
        ),
    ];
    for (tag, body, needle) in cases {
        let world = World::new(tag);
        let file = world.seed_file(&body);
        let output = world.seed(Some(&world.state), &[&file.to_string_lossy(), "--json"]);
        expect_refused(&output, Mode::Json, &[needle, "nothing was written"]);
        assert!(!world.state.join(LEDGER_FILE).exists(), "{tag}");
    }
}

#[cfg(unix)]
#[test]
fn an_unwritable_state_dir_exits_1_and_leaves_no_ledger() {
    use std::os::unix::fs::PermissionsExt;
    let world = World::new("readonly");
    std::fs::create_dir_all(&world.state).expect("state");
    std::fs::set_permissions(&world.state, std::fs::Permissions::from_mode(0o555))
        .expect("read-only");
    let probe = world.state.join("probe");
    if std::fs::write(&probe, b"").is_ok() {
        // Skipped for root, which writes through 0o555; owner: relais maintainers.
        // CI runs it: GitHub's hosted runners run tests as the non-root `runner`
        // user, so only a local run as root skips it.
        eprintln!("skipped: the effective user ignores directory permissions");
        std::fs::remove_file(&probe).expect("remove probe");
        std::fs::set_permissions(&world.state, std::fs::Permissions::from_mode(0o755))
            .expect("restore");
        return;
    }
    let file = world.seed_file(ONE_RUN);
    let output = world.seed(Some(&world.state), &[&file.to_string_lossy(), "--json"]);
    std::fs::set_permissions(&world.state, std::fs::Permissions::from_mode(0o755))
        .expect("restore");
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr));
    assert_eq!(text(&output.stdout), "");
    assert!(snapshot(&world.state).is_empty());
}

#[test]
fn a_state_dir_that_cannot_be_resolved_exits_1() {
    let world = World::new("unresolvable");
    let file = world.seed_file(ONE_RUN);
    let beneath_a_file = file.join("state");
    let output = world.seed(Some(&beneath_a_file), &[&file.to_string_lossy()]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr));
    assert_eq!(text(&output.stdout), "");
}

#[test]
fn the_seed_command_is_in_the_dataset_help() {
    let output = common::relais(BIN)
        .args(["dataset", "--help"])
        .output()
        .expect("relais runs");
    assert_eq!(output.status.code(), Some(0));
    let help = text(&output.stdout);
    assert!(
        help.lines()
            .any(|line| line.trim_start().starts_with("seed ") && line.contains("NEW ledger")),
        "{help}"
    );
}
