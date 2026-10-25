//! The session router's relais commands (SPEC §30), through the real
//! binary: `native router-state`, `native router-observe`, `native
//! router-envelope` and the `relais report` section. Each
//! scenario runs in a world of its own (state, config and home), with no
//! git, no shell and no harness, so it runs on every platform CI builds.

use crate::common;

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
    /// Released only after the scratch directory is gone, so the cap bounds
    /// live worlds, not finished ones.
    _permit: std::sync::Arc<common::WorldPermit>,
}

impl World {
    fn new(tag: &str) -> Self {
        Self::with_permit(tag, std::sync::Arc::new(common::WorldPermit::acquire(1)))
    }

    /// Builds a world under a permit the caller already holds, so a test
    /// with several worlds takes one `acquire(n)` for all of them.
    fn with_permit(tag: &str, permit: std::sync::Arc<common::WorldPermit>) -> Self {
        let scratch = short_temp_dir(&format!("sr-{tag}"));
        let root = scratch.to_path_buf();
        let repo = root.join("repo");
        let state = root.join("state");
        let config = root.join("cfg");
        // A repository root is where `.git` is; nothing here runs git.
        std::fs::create_dir_all(repo.join(".git")).expect("repo");
        std::fs::create_dir_all(&config).expect("config");
        Self {
            _permit: permit,
            _scratch: scratch,
            root,
            repo,
            state,
            config,
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = common::relais(BIN);
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
    // The current models are priced from relais's built-in list.
    assert_eq!(state["rates"]["claude-haiku-5-5"]["input"], 500_000);
    assert_eq!(state["rates"]["claude-sonnet-5-5"]["input"], 2_000_000);
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
fn the_envelope_alone_turns_routing_on_and_the_env_only_narrows_it() {
    let world = World::new("mode");
    let state = |world: &World| {
        world.state_json(&world.relais(&["native", "router-state", "--session", "s"]))
    };
    assert_eq!(state(&world)["mode"], "shadow");
    let granted = world.relais(&[
        "native",
        "router-envelope",
        "--by",
        "me",
        "--epsilon-max",
        "0.1",
    ]);
    assert_eq!(code(&granted), 0, "{}", stderr(&granted));
    let on = state(&world);
    assert_eq!(on["mode"], "on");
    assert_eq!(on["mode_reason"], "envelope recorded");
    assert_eq!(on["epsilon"], 0.1);
    assert!(on.get("r3").is_none(), "{on}");
    for (value, expected) in [
        ("shadow", "shadow"),
        ("off", "off"),
        ("on", "on"),
        ("nope", "shadow"),
    ] {
        let output = world.relais_env(
            &["native", "router-state", "--session", "s"],
            "RELAIS_SESSION_ROUTING",
            value,
        );
        assert_eq!(world.state_json(&output)["mode"], expected, "env {value}");
    }
}

#[test]
fn router_envelope_off_removes_the_envelope_keeps_the_file_and_records_why() {
    let world = World::new("off");
    let before =
        "# my machine\nschema_version = 1\n\n[session_routing]\n# tuned\nholdout_rate = 0.2\n";
    std::fs::write(world.machine(), before).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(world.machine(), std::fs::Permissions::from_mode(0o640)).unwrap();
    }
    let granted = world.relais(&[
        "native",
        "router-envelope",
        "--by",
        "me",
        "--epsilon-max",
        "0.1",
        "--source",
        "plugin-ask",
    ]);
    assert_eq!(code(&granted), 0, "{}", stderr(&granted));
    let off = world.relais(&[
        "native",
        "router-envelope",
        "--off",
        "--by",
        "me",
        "--source",
        "plugin-ask",
    ]);
    assert_eq!(code(&off), 0, "{}", stderr(&off));
    let printed: serde_json::Value = serde_json::from_slice(&off.stdout).unwrap();
    assert_eq!(printed["removed"], true);
    let after = std::fs::read_to_string(world.machine()).unwrap();
    assert!(after.starts_with(before), "{after}");
    assert!(!after.contains("envelope"), "{after}");
    assert!(after.contains("off = {"), "{after}");
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
    let state = world.state_json(&world.relais(&["native", "router-state", "--session", "s"]));
    assert_eq!(state["mode"], "off");
    let rows = world.ledger().router_provenance().unwrap();
    assert!(
        matches!(&rows[..], [RouterProvenance::Envelope { .. }, RouterProvenance::EnvelopeRemoved { source, .. }] if source == "plugin-ask"),
        "{rows:?}"
    );
    // `--off` takes no rate, and a grant needs one.
    assert_eq!(
        code(&world.relais(&[
            "native",
            "router-envelope",
            "--off",
            "--by",
            "me",
            "--epsilon-max",
            "0.1"
        ])),
        2
    );
    assert_eq!(
        code(&world.relais(&["native", "router-envelope", "--by", "me"])),
        2
    );
    // Recording an envelope again lifts the tombstone.
    let again = world.relais(&[
        "native",
        "router-envelope",
        "--by",
        "me",
        "--epsilon-max",
        "0.1",
    ]);
    assert_eq!(code(&again), 0, "{}", stderr(&again));
    let after = std::fs::read_to_string(world.machine()).unwrap();
    assert!(
        after.contains("envelope = {") && !after.contains("off = {"),
        "{after}"
    );
}

#[test]
fn a_task_record_keeps_its_file_hashes_and_its_end_outcome() {
    let world = World::new("files");
    let mut first: serde_json::Value =
        serde_json::from_str(&batch("s", "t1", "completed_verified", "claude-haiku-5-5")).unwrap();
    first["records"][4]["files"] = serde_json::json!(["0123456789abcdef"]);
    first["records"][4]["completed_at_end"] = serde_json::json!("completed_verified");
    first["records"][3]["event"] = serde_json::json!("revert");
    first["records"][3]["escalating"] = serde_json::json!(false);
    first["records"][3]["files"] = serde_json::json!(["0123456789abcdef"]);
    let output = world.relais_stdin(&["native", "router-observe"], &first.to_string());
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let mut later: serde_json::Value =
        serde_json::from_str(&batch("s", "t1", "corrected", "claude-haiku-5-5")).unwrap();
    later["records"][4]["completed_at_end"] = serde_json::json!("corrected");
    let output = world.relais_stdin(&["native", "router-observe"], &later.to_string());
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let task = world
        .ledger()
        .router_task("s", "t1", None)
        .unwrap()
        .unwrap();
    assert_eq!(task.outcome, "corrected");
    assert_eq!(task.completed_at_end.as_deref(), Some("completed_verified"));
    assert_eq!(task.files, vec!["0123456789abcdef"]);
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
        ("unpriced", "claude-mystery-9", "completed_verified"),
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
        text.contains("session routing (API-equivalent estimate, [pricing] 2026-10 + relais built-in 2026-10-07)"),
        "{text}"
    );
    assert!(
        text.contains("routed: 1 session(s), 1 task(s), 1 completed; cost per completed task $1.00103 (95% interval"),
        "{text}"
    );
    assert!(
        text.contains("held out: 1 session(s), 1 task(s), 1 completed; cost per completed task unknown (no [pricing.models] entry for claude-mystery-9)"),
        "{text}"
    );
    let json = world.relais(&["report", "--since", "2026-10-01", "--no-import", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(value["session_routing"]["arms"][0]["arm"], "routed");
    assert!(value["session_routing"]["arms"][1]["cost"].is_null());
}

/// One session's batch of `n` main-session tasks, each with the
/// exploration fields the draw recorded and an outcome.
fn exploration_batch(session: &str, tasks: &[(String, bool, &str)]) -> String {
    let mut records = Vec::new();
    for (task, drawn, outcome) in tasks {
        let class = serde_json::json!({
            "kind": "edit", "difficulty": 3, "scope": "local",
            "uncertainty": "low", "verifiable": true, "confidence": 0.9
        });
        records.push(serde_json::json!({
            "kind": "decision", "task_id": task, "turn_id": format!("{task}-u1"), "agent_id": null,
            "relation": "new_task", "class": class,
            "tier": if *drawn { "research" } else { "implementation" },
            "model": "claude-sonnet-5-5", "effort": null, "reason": "table",
            "mode_effective": "on", "holdout": false, "applied": true,
            "explored": drawn, "propensity": if *drawn { 0.1 } else { 0.9 }, "draw": 0.05,
            "would_pass_gate": true, "at": "2026-10-07T10:00:00Z"
        }));
        records.push(serde_json::json!({
            "kind": "task", "task_id": task, "agent_id": null,
            "started_at": "2026-10-07T10:00:00Z", "ended_at": "2026-10-07T10:05:00Z",
            "class": class, "outcome": outcome, "inferred": [],
            "escalations": 0, "exhausted": false, "turns": 1, "explicit_quote": null
        }));
    }
    serde_json::json!({ "schema": 1, "session": session, "records": records }).to_string()
}

/// 20 drawn tasks, `drawn_corrected` of them corrected, and 40 clean
/// undrawn ones, in a world with an envelope.
fn world_with_exploration(tag: &str, drawn_corrected: usize) -> World {
    let world = World::new(tag);
    std::fs::write(
        world.machine(),
        "schema_version = 1\n[session_routing]\nepsilon = 0.1\n\
         envelope = { granted_at = \"2026-10-07T00:00:00Z\", by = \"me\", epsilon_max = 0.1, source = \"cli\" }\n",
    )
    .unwrap();
    let mut tasks = Vec::new();
    for i in 0..20 {
        let outcome = if i < drawn_corrected {
            "corrected"
        } else {
            "completed_verified"
        };
        tasks.push((format!("d{i}"), true, outcome));
    }
    for i in 0..40 {
        tasks.push((format!("u{i}"), false, "completed_verified"));
    }
    let output = world.relais_stdin(
        &["native", "router-observe"],
        &exploration_batch("s-explore", &tasks),
    );
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    world
}

#[test]
fn drawn_tasks_doing_worse_than_undrawn_stop_exploration_and_the_stop_is_recorded() {
    let world = world_with_exploration("stop", 4);
    let state = world.state_json(&world.relais(&["native", "router-state", "--session", "s-9"]));
    assert_eq!(state["epsilon"], 0.0);
    let stops = world.ledger().router_exploration_stops().unwrap();
    assert_eq!(stops.len(), 1);
    assert_eq!(
        (
            stops[0].drawn,
            stops[0].drawn_bad,
            stops[0].undrawn,
            stops[0].undrawn_bad
        ),
        (20, 4, 40, 0)
    );
    // Read again with nothing new: still stopped, no second row.
    let again = world.state_json(&world.relais(&["native", "router-state", "--session", "s-10"]));
    assert_eq!(again["epsilon"], 0.0);
    assert_eq!(world.ledger().router_exploration_stops().unwrap().len(), 1);
    // The report says so.
    let text = stdout(&world.relais(&["report", "--since", "2026-10-01", "--no-import"]));
    assert!(
        text.contains("main edit d3 local: drawn 20 (20%), undrawn 40 (0%)"),
        "{text}"
    );
    assert!(text.contains("stopped "), "{text}");
    let section = text
        .split("  exploration (")
        .nth(1)
        .and_then(|rest| rest.split("\nlearning:").next())
        .expect("an exploration section");
    assert!(
        section.lines().all(|line| line.chars().count() <= 80),
        "{section}"
    );
}

/// One session's batch of subagent spawns: a decision with relation
/// `subagent` (its `agent_id` empty when the spawn was not keyed) and the
/// task the spawn left, keyed or not.
fn spawn_batch(session: &str, tasks: &[(String, bool, &str, Option<&str>)]) -> String {
    let mut records = Vec::new();
    for (task, drawn, outcome, agent) in tasks {
        let class = serde_json::json!({
            "kind": "edit", "difficulty": 3, "scope": "local",
            "uncertainty": "low", "verifiable": true, "confidence": 0.9
        });
        records.push(serde_json::json!({
            "kind": "decision", "task_id": task, "turn_id": null, "agent_id": null,
            "relation": "subagent", "class": class,
            "tier": if *drawn { "research" } else { "implementation" },
            "model": "claude-sonnet-5-5", "effort": null, "reason": "table",
            "mode_effective": "on", "holdout": false, "applied": true,
            "explored": drawn, "propensity": if *drawn { 0.1 } else { 0.9 }, "draw": 0.05,
            "would_pass_gate": true, "at": "2026-10-07T10:00:00Z"
        }));
        records.push(serde_json::json!({
            "kind": "task", "task_id": task, "agent_id": agent,
            "started_at": "2026-10-07T10:00:00Z", "ended_at": "2026-10-07T10:05:00Z",
            "class": class, "outcome": outcome, "inferred": [],
            "escalations": 0, "exhausted": false, "turns": 1, "explicit_quote": null
        }));
    }
    serde_json::json!({ "schema": 1, "session": session, "records": records }).to_string()
}

#[test]
fn subagent_draws_reach_the_strata_and_the_stop_rule() {
    // 20 clean main draws and 40 clean undrawn: no stop on their own.
    let world = world_with_exploration("spawns", 0);
    // 20 drawn spawns, 4 corrected, keyed and unkeyed alike, and 40 clean
    // undrawn spawns: a `sub` stratum of its own, and the one stop rule
    // now sees 4 of 40 drawn corrected against 0 of 80 undrawn (10 points).
    let mut spawns = Vec::new();
    for i in 0..20 {
        let outcome = if i < 4 {
            "corrected"
        } else {
            "completed_verified"
        };
        let agent = if i % 2 == 0 {
            Some("agent-keyed")
        } else {
            None
        };
        spawns.push((format!("sd{i}"), true, outcome, agent));
    }
    for i in 0..40 {
        spawns.push((
            format!("su{i}"),
            false,
            "completed_verified",
            Some("agent-keyed"),
        ));
    }
    let output = world.relais_stdin(
        &["native", "router-observe"],
        &spawn_batch("s-explore", &spawns),
    );
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let text = stdout(&world.relais(&["report", "--since", "2026-10-01", "--no-import"]));
    assert!(
        text.contains("main edit d3 local: drawn 20 (0%), undrawn 40 (0%)"),
        "{text}"
    );
    assert!(
        text.contains("sub edit d3 local: drawn 20 (20%), undrawn 40 (0%)"),
        "{text}"
    );
    assert!(text.contains("sub = subagent spawn"), "{text}");
    let state = world.state_json(&world.relais(&["native", "router-state", "--session", "s-9"]));
    assert_eq!(state["epsilon"], 0.1, "10 points over: no stop");
    // Nine more corrected spawn draws: 13 of 40 is 32 points over.
    let mut worse = Vec::new();
    for i in 0..9 {
        worse.push((format!("sw{i}"), true, "corrected", None));
    }
    let output = world.relais_stdin(
        &["native", "router-observe"],
        &spawn_batch("s-explore", &worse),
    );
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let state = world.state_json(&world.relais(&["native", "router-state", "--session", "s-9"]));
    assert_eq!(state["epsilon"], 0.0, "spawn draws stop every draw");
    let stops = world.ledger().router_exploration_stops().unwrap();
    assert_eq!(stops.len(), 1);
    assert_eq!((stops[0].drawn, stops[0].drawn_bad), (49, 13));
    let json = world.relais(&["report", "--since", "2026-10-01", "--no-import", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    let strata = value["session_routing"]["exploration"]["strata"]
        .as_array()
        .unwrap();
    assert!(
        strata.iter().any(|s| s["relation"] == "subagent"),
        "{strata:?}"
    );
}

#[test]
fn at_fifteen_points_exploration_goes_on_and_no_stop_is_recorded() {
    let world = world_with_exploration("go", 3);
    let state = world.state_json(&world.relais(&["native", "router-state", "--session", "s-9"]));
    assert_eq!(state["epsilon"], 0.1);
    assert!(world
        .ledger()
        .router_exploration_stops()
        .unwrap()
        .is_empty());
    let json = world.relais(&["report", "--since", "2026-10-01", "--no-import", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    let exploration = &value["session_routing"]["exploration"];
    assert_eq!(exploration["strata"][0]["drawn"], 20);
    assert_eq!(exploration["strata"][0]["projection"]["state"], "reached");
    assert!(exploration["stop"].is_null());
}

// --- classifier calls kept for training (learning-loop Phase 5, ADR-0002) ---

const PROMPT_VERSION: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn classification(call: &str, at: &str, input: &str, parsed: bool) -> serde_json::Value {
    serde_json::json!({
        "kind": "classification", "call_id": call, "purpose": "prompt",
        "model": "claude-haiku-5-5", "prompt_version": PROMPT_VERSION, "input": input,
        "answer": if parsed { serde_json::json!({
            "relation": "new_task", "kind": "edit", "difficulty": 2, "scope": "local",
            "uncertainty": "low", "verifiable": true,
            "explicit": { "value": "none", "quote": null },
            "user_model": null, "user_model_for": null, "confidence": 0.9 }) }
            else { serde_json::Value::Null },
        "parse_ok": parsed, "latency_ms": 350, "at": at
    })
}

fn observe_classifications(world: &World, records: Vec<serde_json::Value>) -> Output {
    world.relais_stdin(
        &["native", "router-observe"],
        &serde_json::json!({ "schema": 1, "session": "s1", "records": records }).to_string(),
    )
}

fn classifier_dir(world: &World) -> PathBuf {
    world.state.join("classifier")
}

/// The bytes of every ledger file (database and journal), for "the text is
/// nowhere in the ledger".
fn ledger_bytes(world: &World) -> Vec<u8> {
    let mut bytes = Vec::new();
    for entry in std::fs::read_dir(&world.state).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            bytes.extend(std::fs::read(path).unwrap());
        }
    }
    bytes
}

/// A Unix permission mode is checked where it exists; Windows has none.
fn assert_mode(path: &std::path::Path, expected: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, expected, "{}", path.display());
    }
    #[cfg(not(unix))]
    {
        let _ = (path, expected);
    }
}

#[test]
fn a_classification_writes_a_private_redacted_day_file_and_a_ledger_row_without_the_text() {
    let world = World::new("cls");
    // Assembled at run time so the source holds no secret-shaped literal.
    let secret = format!("{}{}", "sk-ant-", "api03-SECRETSECRETSECRET");
    let text = format!("ACTIVE TASK: none\n\nREQUEST:\nrename zebrafish to bar, key {secret}");
    let output = observe_classifications(
        &world,
        vec![classification("c1", "2026-10-07T10:00:00Z", &text, true)],
    );
    assert_eq!(code(&output), 0, "{}", stderr(&output));

    let dir = classifier_dir(&world);
    assert_mode(&dir, 0o700);
    let files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    assert_mode(&files[0], 0o600);
    let line = std::fs::read_to_string(&files[0]).unwrap();
    assert!(line.contains("rename zebrafish to bar"), "{line}");
    assert!(!line.contains("SECRETSECRET"), "{line}");
    assert!(
        line.contains("[REDACTED:api-key]") && line.contains("\"redactions\":1"),
        "{line}"
    );

    let ledger = world.ledger();
    assert_eq!(ledger.router_classification_counts().unwrap(), (1, 1));
    let rows = ledger.router_classifications_parsed(None).unwrap();
    assert_eq!(
        rows[0].1.input_sha256,
        relais::ids::sha256_hex(text.as_bytes())
    );
    drop(ledger);
    let bytes = ledger_bytes(&world);
    assert!(
        !bytes.windows(9).any(|w| w == b"zebrafish"),
        "the text is in the ledger"
    );
}

#[test]
fn the_off_switches_capture_nothing() {
    let permit = std::sync::Arc::new(common::WorldPermit::acquire(2));
    // keep_classifier_inputs = false
    let world = World::with_permit("cls-off", permit.clone());
    std::fs::write(
        world.machine(),
        "schema_version = 1\n[session_routing]\nkeep_classifier_inputs = false\n",
    )
    .unwrap();
    let records = vec![
        classification("c1", "2026-10-07T10:00:00Z", "rename foo", true),
        serde_json::from_str(&batch("s", "t1", "unknown", "claude-haiku-5-5"))
            .map(|v: serde_json::Value| v["records"][4].clone())
            .unwrap(),
    ];
    let output = observe_classifications(&world, records);
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert!(!classifier_dir(&world).exists());
    assert_eq!(
        world.ledger().router_classification_counts().unwrap(),
        (0, 0)
    );
    // The rest of the batch is still recorded.
    assert!(world
        .ledger()
        .router_task("s1", "t1", None)
        .unwrap()
        .is_some());

    // RELAIS_SESSION_ROUTING=off
    let world = World::with_permit("cls-env", permit.clone());
    let mut child = world
        .command(&["native", "router-observe"])
        .env("RELAIS_SESSION_ROUTING", "off")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            serde_json::json!({ "schema": 1, "session": "s1", "records": [
                classification("c1", "2026-10-07T10:00:00Z", "rename foo", true)] })
            .to_string()
            .as_bytes(),
        )
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert!(!classifier_dir(&world).exists());
    assert_eq!(
        world.ledger().router_classification_counts().unwrap(),
        (0, 0)
    );
}

#[test]
fn a_write_deletes_day_files_older_than_ninety_days() {
    let world = World::new("cls-ttl");
    let dir = classifier_dir(&world);
    std::fs::create_dir_all(&dir).unwrap();
    let today = chrono::Utc::now().date_naive();
    let name = |ago: i64| {
        format!(
            "{}.jsonl",
            (today - chrono::Duration::days(ago)).format("%Y-%m-%d")
        )
    };
    for ago in [90, 91] {
        std::fs::write(dir.join(name(ago)), "{}\n").unwrap();
    }
    let output = observe_classifications(
        &world,
        vec![classification(
            "c1",
            "2026-10-07T10:00:00Z",
            "rename foo",
            true,
        )],
    );
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert!(dir.join(name(90)).exists());
    assert!(!dir.join(name(91)).exists());
    assert!(dir.join(name(0)).exists());
}

#[test]
fn the_export_splits_by_time_behind_a_header_and_is_created_0600() {
    let world = World::new("cls-export");
    let mut records: Vec<serde_json::Value> = (0..10)
        .map(|i| {
            classification(
                &format!("c{i}"),
                &format!("2026-10-{:02}T10:00:00Z", i + 1),
                &format!("request number {i}"),
                true,
            )
        })
        .collect();
    records.push(classification(
        "bad",
        "2026-10-05T10:00:00Z",
        "no parse",
        false,
    ));
    let output = observe_classifications(&world, records);
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert_eq!(
        world.ledger().router_classification_counts().unwrap(),
        (11, 10)
    );

    // To a file: 0600, and no temporary file left beside it.
    let out = world.root.join("export.jsonl");
    let output = world.relais(&[
        "router",
        "export-classifier",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert_mode(&out, 0o600);
    let leftovers: Vec<_> = std::fs::read_dir(&world.root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".export"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    let text = std::fs::read_to_string(&out).unwrap();
    let lines: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 11);
    assert_eq!(lines[0]["capability_table_version"], 1);
    assert_eq!(
        lines[0]["capability_table"]["rules"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    let splits: Vec<&str> = lines[1..]
        .iter()
        .map(|l| l["split"].as_str().unwrap())
        .collect();
    assert_eq!(splits.iter().filter(|s| **s == "train").count(), 7);
    assert_eq!(&splits[7..], ["calibration", "test", "test"]);
    assert_eq!(lines[1]["call_id"], "c0");
    assert_eq!(lines[1]["input"], "request number 0");
    assert_eq!(lines[1]["prompt_version"], PROMPT_VERSION);
    assert_eq!(lines[1]["answer"]["relation"], "new_task");
    let left_out = stderr(&output);
    assert!(
        left_out.contains("10 call(s) written") && left_out.contains("1 unparsed"),
        "{left_out}"
    );

    // To stdout, from a date.
    let output = world.relais(&[
        "router",
        "export-classifier",
        "--out",
        "-",
        "--since",
        "2026-10-06",
    ]);
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let lines = stdout(&output);
    assert_eq!(lines.lines().count(), 6, "{lines}");
    assert!(
        stderr(&output).contains("5 before --since"),
        "{}",
        stderr(&output)
    );

    let output = world.relais(&[
        "router",
        "export-classifier",
        "--out",
        "-",
        "--since",
        "yesterday",
    ]);
    assert_ne!(code(&output), 0);
}

#[test]
fn the_report_counts_the_parsed_calls_kept() {
    let world = World::new("cls-report");
    let mut value: serde_json::Value =
        serde_json::from_str(&batch("s1", "t1", "completed_verified", "claude-haiku-5-5")).unwrap();
    let records = value["records"].as_array_mut().unwrap();
    records.push(classification("c1", "2026-10-07T10:00:00Z", "one", true));
    records.push(classification("c2", "2026-10-07T10:00:01Z", "two", true));
    records.push(classification("c3", "2026-10-07T10:00:02Z", "three", false));
    let output = world.relais_stdin(&["native", "router-observe"], &value.to_string());
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let text = stdout(&world.relais(&["report", "--since", "2026-10-01", "--no-import"]));
    assert!(
        text.contains("classifier calls kept for training: 2 parsed"),
        "{text}"
    );
    let json = world.relais(&["report", "--since", "2026-10-01", "--no-import", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(value["session_routing"]["classifier"]["captured"], 2);
}

/// The directory and the file are private whatever the umask: the binary
/// runs under `umask 000`, and the directory already exists, world
/// readable.
#[cfg(unix)]
#[test]
fn day_files_stay_private_under_a_permissive_umask() {
    use std::os::unix::fs::PermissionsExt;
    let world = World::new("cls-umask");
    let dir = classifier_dir(&world);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
    let payload = serde_json::json!({ "schema": 1, "session": "s1", "records": [
        classification("c1", "2026-10-07T10:00:00Z", "rename foo", true)] })
    .to_string();
    let mut child = Command::new("sh")
        .args(["-c", "umask 000; exec \"$0\" native router-observe", BIN])
        .current_dir(&world.repo)
        .env("RELAIS_STATE_DIR", &world.state)
        .env("RELAIS_CONFIG_DIR", &world.config)
        .env("HOME", &world.root)
        .env_remove("RELAIS_SESSION_ROUTING")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert_mode(&dir, 0o700);
    for entry in std::fs::read_dir(&dir).unwrap() {
        assert_mode(&entry.unwrap().path(), 0o600);
    }
    // The export under the same umask is 0600 as well.
    let out = world.root.join("out.jsonl");
    let status = Command::new("sh")
        .args([
            "-c",
            "umask 000; exec \"$0\" router export-classifier --out \"$1\"",
            BIN,
            out.to_str().unwrap(),
        ])
        .env("RELAIS_STATE_DIR", &world.state)
        .env("RELAIS_CONFIG_DIR", &world.config)
        .env("HOME", &world.root)
        .output()
        .unwrap();
    assert_eq!(code(&status), 0, "{}", stderr(&status));
    assert_mode(&out, 0o600);
}
