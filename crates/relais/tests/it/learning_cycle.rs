//! The learning cycle through the compiled binary (SPEC §17, relais
//! ADR-0003): the Training lease and its exit codes, idle deferral, the
//! `--if-due` clock, interruption, `learn last`, `rollback`, the `--log`
//! file and the `[learning]` envelope.
//!
//! The coordinator is the one thing a real session would have running; each
//! scenario that needs a particular lease picture starts its own, in this
//! process, with the limits it wants, and plays the other session itself.
//! The pure rules (the Clopper-Pearson rollback bound, anti-flap, log
//! rotation) have their tests beside the code in `learn/cycle.rs`.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use crate::common;

use relais::admission::{DispatchRequest, DispatchSource, ResourceClass, RunRegistration};
use relais::coordinator::{Client, Coordinator, Request};
use relais::ledger::{LearningAction, LearningSource, Ledger};
use relais::policy::ConcurrencyLimits;
use relais::test_support::short_temp_dir;

const BIN: &str = env!("CARGO_BIN_EXE_relais");

struct World {
    _scratch: relais::test_support::TempDir,
    root: PathBuf,
    repo: PathBuf,
    state: PathBuf,
    config: PathBuf,
    claude: PathBuf,
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
        use std::os::unix::fs::PermissionsExt;
        let scratch = short_temp_dir(&format!("lc-{tag}"));
        let root = scratch.to_path_buf();
        let repo = root.join("repo");
        let state = root.join("state");
        let config = root.join("cfg");
        for dir in [&repo, &state, &config] {
            std::fs::create_dir_all(dir).expect("dir");
        }
        // A `claude` that says no plugin is installed and accepts the rest,
        // for the scenarios that install.
        let claude = root.join("claude");
        std::fs::write(
            &claude,
            "#!/bin/sh\nif [ \"$1 $2 $3\" = \"plugin list --json\" ]; then echo '[]'; fi\nexit 0\n",
        )
        .expect("fake claude");
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        Self {
            _permit: permit,
            _scratch: scratch,
            root,
            repo,
            state,
            config,
            claude,
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(BIN);
        command
            .args(args)
            .current_dir(&self.repo)
            .env("RELAIS_STATE_DIR", &self.state)
            .env("RELAIS_CONFIG_DIR", &self.config)
            .env("RELAIS_CLAUDE_BIN", &self.claude)
            .env("RELAIS_SESSION_ID", "tab-cycle")
            .env("HOME", &self.root)
            .env("USERPROFILE", &self.root);
        command
    }

    fn relais(&self, args: &[&str]) -> Output {
        self.command(args).output().expect("relais runs")
    }

    fn socket(&self) -> PathBuf {
        self.state.join("relais.sock")
    }

    fn ledger_path(&self) -> PathBuf {
        self.state.join("ledger.sqlite")
    }

    fn rows(&self) -> Vec<relais::ledger::LearningCycleRow> {
        if !self.ledger_path().is_file() {
            return Vec::new();
        }
        Ledger::open_read_only(&self.ledger_path())
            .expect("ledger")
            .learning_cycles()
            .expect("rows")
    }

    fn machine_text(&self) -> String {
        std::fs::read_to_string(self.config.join("machine.toml")).unwrap_or_default()
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = self.relais(&["coordinator", "stop"]);
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

/// An in-process coordinator with `training_when_idle` as given.
struct Daemon {
    server: Option<std::thread::JoinHandle<()>>,
    client: Client,
}

impl Daemon {
    fn start(socket: &Path, training_when_idle: bool) -> Self {
        let (coordinator, listener) = Coordinator::start(
            socket,
            ConcurrencyLimits {
                max_active_agents: Some(64),
                max_active_agents_per_session: Some(64),
                max_heavy_commands: Some(8),
                max_training_jobs: Some(1),
                max_agent_depth: Some(8),
                max_agents_per_run: Some(256),
                training_when_idle,
            },
            None,
            relais::admission::DEFAULT_AGENT_LEASE_TTL,
        )
        .expect("coordinator starts");
        let server = std::thread::spawn(move || {
            let _ = coordinator.serve(listener);
        });
        let client = Client::new(socket.to_path_buf());
        for _ in 0..100 {
            if client.ping().is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Self {
            server: Some(server),
            client,
        }
    }

    /// An agent lease of another session, bound to `pid`.
    fn agent_of_another_session(&self, pid: u32) {
        self.client
            .request(&Request::RegisterRun {
                registration: RunRegistration {
                    run_id: "run-other".into(),
                    session_id: "tab-other".into(),
                    budget_micros: None,
                    max_agents: None,
                    max_depth: None,
                },
            })
            .expect("register run");
        self.client
            .request(&Request::RequestAdmission {
                request: DispatchRequest {
                    dispatch_id: "d-other".into(),
                    run_id: "run-other".into(),
                    session_id: "tab-other".into(),
                    parent_dispatch: None,
                    depth: 0,
                    resource: ResourceClass::ModelWork,
                    reserve_micros: 0,
                    source: DispatchSource::ManagedRun,
                    caller_agent_id: None,
                    weight: 1,
                    disk: None,
                },
            })
            .expect("admission");
        self.client
            .request(&Request::Bind {
                dispatch_id: "d-other".into(),
                agent_id: Some("agent".into()),
                pid: Some(pid),
            })
            .expect("bind");
    }

    fn stop(mut self) {
        let _ = self.client.request(&Request::Shutdown);
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
    }
}

#[test]
fn a_held_training_lease_refuses_the_cycle_with_exit_20_naming_the_holder() {
    let world = World::new("held");
    let daemon = Daemon::start(&world.socket(), false);
    let holder = daemon
        .client
        .acquire_training("tab-holder", std::process::id(), "2026-10-10T10:00:00Z")
        .expect("lease");
    assert_eq!(
        holder.decision,
        relais::admission::TrainingDecision::Granted
    );

    let out = world.relais(&["learn", "cycle"]);
    assert_eq!(out.status.code(), Some(20), "{}", text(&out.stderr));
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains(&format!("pid {}", std::process::id())),
        "names the holder's pid: {stderr}"
    );
    assert!(
        stderr.contains("2026-10-10T10:00:00Z"),
        "and when: {stderr}"
    );
    assert!(world.rows().is_empty(), "a refused cycle writes no row");

    // Once the holder lets go, the cycle runs.
    daemon.client.end_training(&holder).expect("release");
    let out = world.relais(&["learn", "cycle"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(world.rows().len(), 1);
    daemon.stop();
}

#[test]
fn concurrent_cycles_leave_exactly_one_row_and_the_others_exit_20_or_find_it_not_due() {
    let world = World::new("race");
    let children: Vec<_> = (0..6)
        .map(|_| {
            world
                .command(&["learn", "cycle", "--if-due", "-q"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn")
        })
        .collect();
    let mut codes = Vec::new();
    for child in children {
        let out = child.wait_with_output().expect("wait");
        let code = out.status.code();
        if code == Some(20) {
            assert!(
                text(&out.stderr).contains("holds the Training lease (pid "),
                "{}",
                text(&out.stderr)
            );
        }
        codes.push(code);
    }
    assert!(
        codes.iter().all(|code| matches!(code, Some(0) | Some(20))),
        "{codes:?}"
    );
    assert!(codes.contains(&Some(0)), "one of them ran: {codes:?}");
    assert_eq!(world.rows().len(), 1, "exactly one learning_cycles row");
}

#[test]
fn an_agent_of_another_session_defers_the_cycle_without_a_row_and_logs_one_line() {
    let world = World::new("deferred");
    let daemon = Daemon::start(&world.socket(), true);
    daemon.agent_of_another_session(std::process::id());
    let log = world.root.join("learn-cycle.log");
    let log_arg = log.to_str().expect("utf-8").to_string();

    let out = world.relais(&["learn", "cycle", "--log", &log_arg]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(world.rows().is_empty(), "a deferral writes no row");
    let logged = std::fs::read_to_string(&log).expect("the log");
    assert_eq!(logged.lines().count(), 1, "{logged}");
    assert!(
        logged.contains("deferred: not idle (1 agents active)"),
        "{logged}"
    );
    assert_eq!(
        text(&out.stdout),
        "",
        "with --log the line goes to the file"
    );

    // A second deferral appends; without --log it is printed.
    let again = world.relais(&["learn", "cycle", "--log", &log_arg]);
    assert_eq!(again.status.code(), Some(0));
    assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 2);
    let plain = world.relais(&["learn", "cycle"]);
    assert_eq!(plain.status.code(), Some(0));
    assert_eq!(
        text(&plain.stdout).trim(),
        "deferred: not idle (1 agents active)"
    );
    assert!(world.rows().is_empty());
    daemon.stop();
}

#[test]
fn the_log_file_is_created_0600() {
    use std::os::unix::fs::PermissionsExt;
    let world = World::new("log-mode");
    let log = world.root.join("sub").join("learn-cycle.log");
    let out = world.relais(&["learn", "cycle", "--log", log.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let mode = std::fs::metadata(&log)
        .expect("created")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    assert!(
        std::fs::read_to_string(&log)
            .unwrap()
            .contains("learning cycle 1: none"),
        "the cycle's own lines went to the log"
    );
    assert_eq!(text(&out.stdout), "");
}

#[test]
fn with_no_lease_held_the_cycle_runs() {
    let world = World::new("idle");
    let daemon = Daemon::start(&world.socket(), true);
    let out = world.relais(&["learn", "cycle"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(stdout.contains("learning cycle 1: none"), "{stdout}");
    let rows = world.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].action, LearningAction::None);
    assert!(rows[0].ended_at.is_some());
    daemon.stop();
}

#[test]
fn a_lease_whose_agent_process_is_dead_does_not_defer_the_cycle() {
    let world = World::new("dead-pid");
    let daemon = Daemon::start(&world.socket(), true);
    let mut sleeper = Command::new("sleep").arg("60").spawn().expect("sleep");
    daemon.agent_of_another_session(sleeper.id());

    // While the agent's process lives, the cycle defers.
    let busy = world.relais(&["learn", "cycle"]);
    assert_eq!(
        text(&busy.stdout).trim(),
        "deferred: not idle (1 agents active)"
    );
    assert!(world.rows().is_empty());

    sleeper.kill().expect("kill");
    sleeper.wait().expect("reap");
    let out = world.relais(&["learn", "cycle"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("learning cycle 1: none"),
        "{}",
        text(&out.stdout)
    );
    assert_eq!(world.rows().len(), 1);
    daemon.stop();
}

#[test]
fn an_unfinished_row_is_closed_as_interrupted_by_the_next_cycle() {
    let world = World::new("interrupted");
    let dead = {
        let ledger = Ledger::open(&world.ledger_path()).expect("ledger");
        ledger.begin_learning_cycle().expect("begin")
    };
    let out = world.relais(&["learn", "cycle"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let rows = world.rows();
    assert_eq!(rows.len(), 2);
    let closed = rows.iter().find(|row| row.id == dead).expect("old row");
    assert_eq!(closed.action, LearningAction::Interrupted);
    assert!(closed.ended_at.is_some());
}

#[test]
fn an_unfinished_row_never_makes_if_due_answer_not_due() {
    let world = World::new("unfinished-due");
    let dead = {
        let ledger = Ledger::open(&world.ledger_path()).expect("ledger");
        ledger.begin_learning_cycle().expect("begin")
    };
    let out = world.relais(&["learn", "cycle", "--if-due"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(
        !text(&out.stdout).starts_with("not due"),
        "{}",
        text(&out.stdout)
    );
    let rows = world.rows();
    assert_eq!(rows.len(), 2, "the cycle ran");
    let closed = rows.iter().find(|row| row.id == dead).expect("old row");
    assert_eq!(closed.action, LearningAction::Interrupted);
    assert!(closed.ended_at.is_some());
}

#[test]
fn a_malformed_relais_toml_fails_the_cycle_with_its_message() {
    let world = World::new("bad-toml");
    let git = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&world.repo)
        .output()
        .expect("git init");
    assert!(git.status.success());
    std::fs::write(world.repo.join("relais.toml"), "this is = = not toml\n").expect("write");
    let log = world.root.join("cycle.log");
    let log_arg = log.to_str().expect("utf8 path");
    let out = world.relais(&["learn", "cycle", "--if-due", "--log", log_arg]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stdout));
    let logged = std::fs::read_to_string(&log).expect("log");
    assert!(logged.contains("relais.toml"), "{logged}");
    let rows = world.rows();
    assert_eq!(rows.len(), 1, "one error row");
    assert_eq!(rows[0].action, LearningAction::Error);
    assert!(rows[0].ended_at.is_some());
    assert!(
        rows[0]
            .error
            .as_deref()
            .is_some_and(|e| e.contains("relais.toml")),
        "{:?}",
        rows[0].error
    );
}

#[test]
fn a_finished_row_then_an_interrupted_one_keeps_if_due_by_the_finished_age() {
    let world = World::new("finished-then-interrupted");
    let first = world.relais(&["learn", "cycle"]);
    assert_eq!(first.status.code(), Some(0), "{}", text(&first.stderr));
    {
        let ledger = Ledger::open(&world.ledger_path()).expect("ledger");
        let id = ledger.begin_learning_cycle().expect("begin");
        ledger
            .close_unfinished_learning_cycles(id + 1)
            .expect("close");
        let last = ledger.last_cycle_started().expect("last").expect("row");
        assert_eq!(last.action, LearningAction::Interrupted);
        let report = relais::learn::summary::learning_report(&ledger, None).expect("report");
        let reason = report.nothing_learned.clone().expect("nothing learned");
        assert!(
            reason.contains("interrupted before it finished"),
            "{reason}"
        );
        let cycle = report.last_cycle.expect("a cycle");
        assert_eq!(cycle.action, LearningAction::Interrupted);
    }
    let out = world.relais(&["learn", "cycle", "--if-due"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).starts_with("not due"),
        "{}",
        text(&out.stdout)
    );
}

#[test]
fn if_due_within_twenty_hours_exits_0_with_the_reason_and_the_next_time() {
    let world = World::new("if-due");
    let first = world.relais(&["learn", "cycle", "--if-due"]);
    assert_eq!(first.status.code(), Some(0), "{}", text(&first.stderr));
    assert_eq!(world.rows().len(), 1, "never run before: due");

    let second = world.relais(&["learn", "cycle", "--if-due"]);
    assert_eq!(second.status.code(), Some(0), "{}", text(&second.stderr));
    let stdout = text(&second.stdout);
    assert_eq!(stdout.lines().count(), 1, "{stdout}");
    assert!(
        stdout.starts_with("not due: the last cycle started "),
        "{stdout}"
    );
    assert!(stdout.contains("next due 20"), "{stdout}");
    assert_eq!(world.rows().len(), 1, "no second row");
}

#[test]
fn learn_last_prints_the_latest_row_and_never_creates_or_migrates_the_ledger() {
    let world = World::new("last");
    let none = world.relais(&["learn", "last", "--json"]);
    assert_eq!(none.status.code(), Some(0), "{}", text(&none.stderr));
    let document: serde_json::Value = serde_json::from_slice(&none.stdout).expect("json");
    assert!(document["last"].is_null(), "{document}");
    assert!(
        !world.ledger_path().exists(),
        "reading the last cycle must not create the ledger"
    );

    assert_eq!(world.relais(&["learn", "cycle"]).status.code(), Some(0));
    let before = std::fs::read(world.ledger_path()).expect("ledger bytes");
    let out = world.relais(&["learn", "last", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let document: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(document["last"]["action"], "none", "{document}");
    assert_eq!(document["last"]["source"], "cycle", "{document}");
    assert_eq!(
        std::fs::read(world.ledger_path()).expect("ledger bytes"),
        before,
        "the ledger is byte for byte what it was"
    );
}

#[test]
fn rollback_returns_to_the_previous_artifact_and_records_a_person_row() {
    use relais::learn::fixtures::{artifact, evaluation};
    let world = World::new("rollback");
    let registry =
        relais::learn::registry::Registry::open(&world.state.join("registry")).expect("registry");
    for id in ["art-a", "art-b"] {
        registry
            .store(&artifact(id, Some(evaluation(id, true))))
            .expect("store");
        let checked = registry.evaluated(id).expect("evidence");
        registry.promote(&checked).expect("promote");
    }

    let out = world.relais(&["rollback", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let document: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(document["rolled_back"]["from"], "art-b", "{document}");
    assert_eq!(document["rolled_back"]["to"], "art-a", "{document}");
    assert_eq!(registry.active().unwrap().unwrap().artifact_id, "art-a");
    let rows = world.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].source, LearningSource::Person);
    assert_eq!(rows[0].action, LearningAction::RolledBack);
    assert_eq!(rows[0].artifact_id.as_deref(), Some("art-b"));

    // The report names it.
    let report = world.relais(&["report"]);
    let report = text(&report.stdout);
    assert!(report.contains("last rollback: "), "{report}");
    assert!(report.contains("by a person"), "{report}");

    // Nothing more to go back to: blocked, and no second row.
    let again = world.relais(&["rollback"]);
    assert_eq!(again.status.code(), Some(3), "{}", text(&again.stderr));
    assert_eq!(world.rows().len(), 1);
}

#[test]
fn the_report_has_a_learning_section_within_80_columns_and_in_json() {
    let world = World::new("report");
    let report = world.relais(&["report"]);
    let stdout = text(&report.stdout);
    assert!(stdout.contains("learning:"), "{stdout}");
    assert!(stdout.contains("Nothing learned yet, because"), "{stdout}");
    for line in stdout.lines() {
        assert!(line.chars().count() <= 80, "{line:?}");
    }
    let json = world.relais(&["report", "--json"]);
    let document: serde_json::Value = serde_json::from_slice(&json.stdout).expect("json");
    assert!(document["learning"].is_object(), "{document}");
}

#[test]
fn the_learning_envelope_is_written_and_an_off_tombstone_stops_the_installer() {
    let world = World::new("envelope");
    let grant = world.relais(&["native", "learning-envelope", "--by", "tester"]);
    assert_eq!(grant.status.code(), Some(0), "{}", text(&grant.stderr));
    let machine = world.machine_text();
    assert!(machine.contains("[learning]"), "{machine}");
    assert!(machine.contains("auto_promote"), "{machine}");
    assert!(machine.contains("tester"), "{machine}");

    let off = world.relais(&["native", "learning-envelope", "--by", "tester", "--off"]);
    assert_eq!(off.status.code(), Some(0), "{}", text(&off.stderr));
    let machine = world.machine_text();
    assert!(!machine.contains("auto_promote"), "{machine}");
    assert!(machine.contains("off = "), "{machine}");

    let install = world.relais(&["install", "--claude", "--write"]);
    assert_eq!(install.status.code(), Some(0), "{}", text(&install.stderr));
    let machine = world.machine_text();
    assert!(
        !machine.contains("auto_promote"),
        "an off tombstone keeps the installer from recording it: {machine}"
    );
    assert!(
        !text(&install.stdout).contains("automatic promotion is on"),
        "{}",
        text(&install.stdout)
    );
}

#[test]
fn installing_records_the_envelope_and_says_how_to_turn_it_off() {
    let world = World::new("install-grant");
    let install = world.relais(&["install", "--claude", "--write"]);
    assert_eq!(install.status.code(), Some(0), "{}", text(&install.stderr));
    assert!(
        text(&install.stdout)
            .contains("automatic promotion is on; turn it off with /relais-learning off"),
        "{}",
        text(&install.stdout)
    );
    let machine = world.machine_text();
    assert!(machine.contains("auto_promote"), "{machine}");
    assert!(machine.contains("source = \"install\""), "{machine}");
}
