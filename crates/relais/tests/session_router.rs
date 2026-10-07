//! The session router's relais commands (SPEC §30), through the real
//! binary: `native router-state`, `native router-observe`, `native
//! router-envelope`, `router r3` and the `relais report` section. Each
//! scenario runs in a world of its own (state, config and home), with no
//! git, no shell and no harness, so it runs on every platform CI builds.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use relais::ledger::{Ledger, RouterProvenance};
use relais::test_support::short_temp_dir;

const BIN: &str = env!("CARGO_BIN_EXE_relais");

struct World {
    _scratch: relais::test_support::TempDir,
    root: PathBuf,
    repo: PathBuf,
    state: PathBuf,
    config: PathBuf,
}

impl World {
    fn new(tag: &str) -> Self {
        let scratch = short_temp_dir(&format!("sr-{tag}"));
        let root = scratch.to_path_buf();
        let repo = root.join("repo");
        let state = root.join("state");
        let config = root.join("cfg");
        // A repository root is where `.git` is; nothing here runs git.
        std::fs::create_dir_all(repo.join(".git")).expect("repo");
        std::fs::create_dir_all(&config).expect("config");
        Self {
            _scratch: scratch,
            root,
            repo,
            state,
            config,
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(BIN);
        command
            .args(args)
            .current_dir(&self.repo)
            .env("RELAIS_STATE_DIR", &self.state)
            .env("RELAIS_CONFIG_DIR", &self.config)
            .env("HOME", &self.root)
            .env("USERPROFILE", &self.root)
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("RELAIS_SESSION_ROUTING");
        command
    }

    fn relais(&self, args: &[&str]) -> Output {
        self.command(args).output().expect("relais runs")
    }

    fn relais_env(&self, args: &[&str], key: &str, value: &str) -> Output {
        self.command(args)
            .env(key, value)
            .output()
            .expect("relais runs")
    }

    fn relais_stdin(&self, args: &[&str], stdin: &str) -> Output {
        let mut child = self
            .command(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("relais spawns");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(stdin.as_bytes())
            .expect("write stdin");
        child.wait_with_output().expect("relais runs")
    }

    fn ledger_path(&self) -> PathBuf {
        self.state.join("ledger.sqlite")
    }

    fn ledger(&self) -> Ledger {
        Ledger::open(&self.ledger_path()).expect("ledger")
    }

    fn machine(&self) -> PathBuf {
        self.config.join("machine.toml")
    }

    fn state_json(&self, output: &Output) -> serde_json::Value {
        assert_eq!(code(output), 0, "{}", stderr(output));
        serde_json::from_slice(&output.stdout).expect("router-state prints JSON")
    }
}

fn code(output: &Output) -> i32 {
    output.status.code().expect("exited")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn batch(session: &str, task: &str, outcome: &str, model: &str) -> String {
    serde_json::json!({
        "schema": 1,
        "session": session,
        "records": [
            { "kind": "decision", "task_id": task, "turn_id": "u1", "agent_id": null,
              "relation": "new_task",
              "class": { "kind": "edit", "difficulty": 2, "scope": "local", "uncertainty": "low", "verifiable": true, "confidence": 0.9 },
              "tier": "research", "model": model, "effort": null, "reason": "table",
              "mode_effective": "on", "holdout": false, "applied": true,
              "explored": false, "propensity": null, "draw": null, "would_pass_gate": true,
              "at": "2026-10-07T10:00:00Z" },
            { "kind": "usage", "task_id": task, "turn_id": format!("{task}-u1"), "step": 0, "agent_id": null,
              "source": "step", "model": model, "input_tokens": 1000000, "output_tokens": 0,
              "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0, "at": "2026-10-07T10:00:01Z" },
            { "kind": "usage", "task_id": task, "turn_id": format!("{task}-u1"), "step": 0, "agent_id": null,
              "source": "classifier", "model": model, "input_tokens": 1000, "output_tokens": 6,
              "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0, "at": "2026-10-07T10:00:00Z" },
            { "kind": "reassess", "task_id": task, "agent_id": null, "event": "failed_verification",
              "tier_from": "research", "tier_to": "implementation", "effort_to": null, "escalating": true,
              "at": "2026-10-07T10:00:02Z" },
            { "kind": "task", "task_id": task, "agent_id": null, "started_at": "2026-10-07T10:00:00Z",
              "ended_at": "2026-10-07T10:05:00Z", "class": null, "outcome": outcome, "inferred": [],
              "escalations": 1, "exhausted": false, "turns": 2, "explicit_quote": null }
        ]
    })
    .to_string()
}

#[test]
fn router_state_answers_in_shadow_and_never_creates_the_ledger() {
    let world = World::new("state");
    let output = world.relais(&["native", "router-state", "--session", "s-1"]);
    let state = world.state_json(&output);
    assert_eq!(state["schema"], 1);
    assert_eq!(state["mode"], "shadow");
    assert_eq!(state["mode_reason"], "no envelope");
    assert_eq!(state["tiers"]["research"]["model"], "claude-haiku-5-5");
    assert_eq!(state["capability_table"]["version"], 1);
    assert_eq!(state["seed"].as_str().unwrap().len(), 16);
    assert!(state["rates"]["claude-haiku-5-5"].is_null());
    assert!(
        !world.ledger_path().exists(),
        "router-state created the ledger"
    );
    assert!(
        !world.state.exists(),
        "router-state created the state directory"
    );
    // The same session draws the same hold-out and seed.
    let again = world.state_json(&world.relais(&["native", "router-state", "--session", "s-1"]));
    assert_eq!(again["seed"], state["seed"]);
    assert_eq!(again["holdout"], state["holdout"]);
}

#[test]
fn router_state_reads_repository_tiers_checks_and_agent_pins() {
    let world = World::new("pins");
    std::fs::write(
        world.repo.join("relais.toml"),
        "schema_version = 1\n[models.implementation]\nid = \"opus\"\neffort = \"high\"\n\
         [[verification.profiles.default.commands]]\nargv = [\"just\", \"check\"]\n",
    )
    .unwrap();
    let user_agents = world.root.join(".claude").join("agents");
    std::fs::create_dir_all(&user_agents).unwrap();
    std::fs::write(
        user_agents.join("deep.md"),
        "---\nname: deep-thinker\nmodel: opus\n---\n",
    )
    .unwrap();
    std::fs::write(user_agents.join("broken.md"), "model: haiku\n").unwrap();
    let project_agents = world.repo.join(".claude").join("agents");
    std::fs::create_dir_all(&project_agents).unwrap();
    std::fs::write(project_agents.join("quick.md"), "---\nmodel: haiku\n---\n").unwrap();
    std::fs::write(
        world.machine(),
        "schema_version = 1\n[session_routing]\npinned_agents = [\"mine\"]\n\
         excluded_models = [\"claude-opus-5-5[1m]\"]\n",
    )
    .unwrap();
    let output = world.relais(&["native", "router-state", "--session", "s"]);
    let state = world.state_json(&output);
    assert_eq!(state["tiers"]["implementation"]["model"], "claude-opus-5-5");
    assert_eq!(state["tiers"]["implementation"]["effort"], "high");
    assert_eq!(state["checks"][0], serde_json::json!(["just", "check"]));
    assert_eq!(state["pins"]["deep-thinker"], "opus");
    assert_eq!(state["pins"]["quick"], "haiku");
    assert_eq!(state["pins"]["mine"], "inherit");
    assert_eq!(state["excluded_models"][0], "claude-opus-5-5[1m]");
    assert!(
        stderr(&output).contains("broken.md skipped"),
        "{}",
        stderr(&output)
    );
}

/// An agents directory that exists but cannot be read is said: skipping it
/// silently would drop the person's pins, and the router would override
/// models they chose.
#[cfg(unix)]
#[test]
fn an_unreadable_agents_directory_is_said_not_skipped_silently() {
    use std::os::unix::fs::PermissionsExt;
    let world = World::new("pins-unreadable");
    let user_agents = world.root.join(".claude").join("agents");
    std::fs::create_dir_all(&user_agents).unwrap();
    std::fs::write(user_agents.join("deep.md"), "---\nmodel: opus\n---\n").unwrap();
    std::fs::set_permissions(&user_agents, std::fs::Permissions::from_mode(0o000)).unwrap();
    let output = world.relais(&["native", "router-state", "--session", "s"]);
    std::fs::set_permissions(&user_agents, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("could not be read")
            && stderr(&output).contains("pins are not applied"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn the_mode_is_on_only_with_an_envelope_and_a_passing_r3_and_the_env_narrows_it() {
    let world = World::new("mode");
    let mode = |world: &World| {
        world.state_json(&world.relais(&["native", "router-state", "--session", "s"]))["mode"]
            .clone()
    };
    let granted = world.relais(&[
        "native",
        "router-envelope",
        "--by",
        "me",
        "--epsilon-max",
        "0.1",
    ]);
    assert_eq!(code(&granted), 0, "{}", stderr(&granted));
    assert_eq!(mode(&world), "shadow");
    let recorded = world.relais(&["router", "r3", "--record", "--id", "r3-abc", "--passed"]);
    assert_eq!(code(&recorded), 0, "{}", stderr(&recorded));
    let state = world.state_json(&world.relais(&["native", "router-state", "--session", "s"]));
    assert_eq!(state["mode"], "on");
    assert_eq!(state["r3"]["id"], "r3-abc");
    assert_eq!(state["r3"]["source"], "cli (unattributed)");
    assert_eq!(state["epsilon"], 0.1);
    for (value, expected) in [("shadow", "shadow"), ("off", "off"), ("on", "on")] {
        let output = world.relais_env(
            &["native", "router-state", "--session", "s"],
            "RELAIS_SESSION_ROUTING",
            value,
        );
        assert_eq!(world.state_json(&output)["mode"], expected, "env {value}");
    }
    // A later failure withdraws the pass.
    world.relais(&["router", "r3", "--record", "--id", "r3-def", "--failed"]);
    let state = world.state_json(&world.relais(&["native", "router-state", "--session", "s"]));
    assert_eq!(state["mode"], "shadow");
    assert_eq!(state["mode_reason"], "no r3 pass");
}

#[test]
fn r3_record_needs_a_verdict_and_eval_prints_one() {
    let world = World::new("r3");
    let refused = world.relais(&["router", "r3", "--record", "--id", "x"]);
    assert_eq!(code(&refused), 2, "{}", stderr(&refused));
    assert_eq!(code(&world.relais(&["router", "r3"])), 2);
    let labels = world.root.join("labels.jsonl");
    std::fs::write(
        &labels,
        r#"{"item":"a","human":{"tier":"research","relation":"new_task","outcome_correct":true},"router":{"tier":"research","relation":"new_task","outcome":"completed_verified"}}
"#,
    )
    .unwrap();
    let path = labels.to_string_lossy().into_owned();
    let output = world.relais(&["router", "r3", "--eval", &path, "--json"]);
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let evaluation: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(evaluation["passed"], false);
    assert!(evaluation["id"].as_str().unwrap().starts_with("r3-"));
    let text = stdout(&world.relais(&["router", "r3", "--eval", &path]));
    assert!(text.contains("verdict: failed (id r3-"), "{text}");
    std::fs::write(&labels, "{not json}\n").unwrap();
    assert_eq!(code(&world.relais(&["router", "r3", "--eval", &path])), 2);
}

#[test]
fn eight_concurrent_observers_lose_nothing_and_a_resend_adds_nothing() {
    let world = World::new("observe");
    let children: Vec<_> = (0..8)
        .map(|i| {
            let payload = batch(
                &format!("session-{i}"),
                "t1",
                "completed_verified",
                "claude-haiku-5-5",
            );
            let mut child = world
                .command(&["native", "router-observe"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn");
            child
                .stdin
                .take()
                .unwrap()
                .write_all(payload.as_bytes())
                .unwrap();
            child
        })
        .collect();
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert_eq!(code(&output), 0, "{}", stderr(&output));
    }
    // decisions, usage (step + classifier), reassess, tasks
    assert_eq!(world.ledger().router_row_counts().unwrap(), [8, 16, 8, 8]);
    let again = world.relais_stdin(
        &["native", "router-observe"],
        &batch("session-3", "t1", "completed_verified", "claude-haiku-5-5"),
    );
    assert_eq!(code(&again), 0);
    assert_eq!(world.ledger().router_row_counts().unwrap(), [8, 16, 8, 8]);
}

#[test]
fn a_bad_payload_exits_2_names_the_record_and_writes_nothing() {
    let world = World::new("bad");
    let mut value: serde_json::Value =
        serde_json::from_str(&batch("s", "t1", "unknown", "claude-haiku-5-5")).unwrap();
    value["records"][4]["outcome"] = serde_json::json!("inadequate");
    let output = world.relais_stdin(&["native", "router-observe"], &value.to_string());
    assert_eq!(code(&output), 2, "{}", stderr(&output));
    assert!(
        stderr(&output).contains("records[4] (task)"),
        "{}",
        stderr(&output)
    );
    assert!(!world.ledger_path().exists());
    // With a ledger: the good records of a bad batch are not written either.
    let good = world.relais_stdin(
        &["native", "router-observe"],
        &batch("other", "t1", "unknown", "claude-haiku-5-5"),
    );
    assert_eq!(code(&good), 0);
    let before = world.ledger().router_row_counts().unwrap();
    let output = world.relais_stdin(&["native", "router-observe"], &value.to_string());
    assert_eq!(code(&output), 2);
    assert_eq!(world.ledger().router_row_counts().unwrap(), before);
    assert_eq!(
        code(&world.relais_stdin(&["native", "router-observe"], "not json")),
        2
    );
}

#[test]
fn a_correction_is_never_downgraded_by_a_later_record() {
    let world = World::new("rank");
    for outcome in ["corrected", "completed_verified", "unknown"] {
        let output = world.relais_stdin(
            &["native", "router-observe"],
            &batch("s", "t1", outcome, "claude-haiku-5-5"),
        );
        assert_eq!(code(&output), 0, "{}", stderr(&output));
    }
    let task = world
        .ledger()
        .router_task("s", "t1", None)
        .unwrap()
        .unwrap();
    assert_eq!(task.outcome, "corrected");
}

#[test]
fn the_envelope_keeps_machine_toml_comments_and_mode_and_records_provenance() {
    let world = World::new("envelope");
    let before =
        "# my machine\nschema_version = 1\n\n# reviewed\n[spending]\nper_run_micros = 3000000\n";
    std::fs::write(world.machine(), before).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(world.machine(), std::fs::Permissions::from_mode(0o640)).unwrap();
    }
    let output = world.relais(&[
        "native",
        "router-envelope",
        "--by",
        "a person",
        "--epsilon-max",
        "0.05",
        "--source",
        "plugin-ask",
    ]);
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let printed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(printed["envelope"]["source"], "plugin-ask");
    let after = std::fs::read_to_string(world.machine()).unwrap();
    assert!(after.starts_with(before), "{after}");
    assert!(after.contains("[session_routing]"), "{after}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(world.machine())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o640);
    }
    let rows = world.ledger().router_provenance().unwrap();
    assert!(
        matches!(&rows[..], [RouterProvenance::Envelope { by, source, .. }] if by == "a person" && source == "plugin-ask"),
        "{rows:?}"
    );
    // Typed in a shell, the same command is unattributed.
    world.relais(&[
        "native",
        "router-envelope",
        "--by",
        "me",
        "--epsilon-max",
        "0.1",
    ]);
    let rows = world.ledger().router_provenance().unwrap();
    assert!(
        matches!(&rows[1], RouterProvenance::Envelope { source, .. } if source == "cli (unattributed)"),
        "{rows:?}"
    );
    assert_eq!(
        code(&world.relais(&[
            "native",
            "router-envelope",
            "--by",
            "me",
            "--epsilon-max",
            "2"
        ])),
        2
    );
    assert_eq!(
        code(&world.relais(&[
            "native",
            "router-envelope",
            "--by",
            " ",
            "--epsilon-max",
            "0.1"
        ])),
        2
    );
}

#[test]
fn the_report_prices_routing_spend_and_says_unknown_for_an_unpriced_model() {
    let world = World::new("report");
    std::fs::write(
        world.machine(),
        "schema_version = 1\n[pricing]\nversion = \"2026-10\"\n[[pricing.models]]\n\
         ids = [\"claude-haiku-5-5\"]\ninput = 1000000\noutput = 5000000\ncache_read = 100000\n\
         cache_write_5m = 1250000\ncache_write_1h = 2000000\n",
    )
    .unwrap();
    for (session, model, outcome) in [
        ("priced", "claude-haiku-5-5", "completed_verified"),
        ("unpriced", "claude-opus-5-5", "completed_verified"),
    ] {
        let mut value: serde_json::Value =
            serde_json::from_str(&batch(session, "t1", outcome, model)).unwrap();
        if session == "unpriced" {
            value["records"][0]["holdout"] = serde_json::json!(true);
            value["records"][0]["applied"] = serde_json::json!(false);
        }
        let output = world.relais_stdin(&["native", "router-observe"], &value.to_string());
        assert_eq!(code(&output), 0, "{}", stderr(&output));
    }
    let output = world.relais(&["report", "--since", "2026-10-01", "--no-import"]);
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let text = stdout(&output);
    assert!(
        text.contains("session routing (API-equivalent estimate, [pricing] 2026-10)"),
        "{text}"
    );
    assert!(
        text.contains("routed: 1 session(s), 1 task(s), 1 completed; cost per completed task $1.00103 (95% interval"),
        "{text}"
    );
    assert!(
        text.contains("held out: 1 session(s), 1 task(s), 1 completed; cost per completed task unknown (no [pricing.models] entry for claude-opus-5-5)"),
        "{text}"
    );
    let json = world.relais(&["report", "--since", "2026-10-01", "--no-import", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(value["session_routing"]["arms"][0]["arm"], "routed");
    assert!(value["session_routing"]["arms"][1]["cost"].is_null());
}
