//! `relais hook`, run with no flags, through the real binary (SPEC §23):
//! the live path that asks a real coordinator about a spawn, applies
//! `hook::decide::decide`, prints the answer, and journals the firing.
//!
//! Everything here drives the compiled binary as a subprocess against an
//! in-process coordinator this suite starts itself — never the pure
//! `decide` function directly, which `hook::decide`'s own unit tests
//! already cover, and never `hook::respond::handle` directly, which
//! `hook::respond`'s own unit tests already cover. This file exists to
//! prove the wiring between them: that the command, not just the
//! function, produces a refusal on stdout for a refusing coordinator and
//! nothing at all for a granting one, and that it exits zero regardless.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use relais::coordinator::{Coordinator, Request};
use relais::policy::ConcurrencyLimits;

const BIN: &str = env!("CARGO_BIN_EXE_relais");

/// One isolated world: a state directory and a config directory, and
/// nothing else — no repository, since a hook never runs `relais` from
/// inside one.
struct World {
    state: PathBuf,
    config: PathBuf,
}

impl World {
    fn new(tag: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let base = if cfg!(unix) {
            PathBuf::from("/tmp")
        } else {
            std::env::temp_dir()
        };
        let root = base.join(format!(
            "rl-hook-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        let state = root.join("state");
        let config = root.join("cfg");
        std::fs::create_dir_all(&state).expect("state");
        std::fs::create_dir_all(&config).expect("config");
        Self { state, config }
    }

    fn socket(&self) -> PathBuf {
        self.state.join("relais.sock")
    }

    /// Run `relais hook` with `payload` on stdin, no coordinator daemon
    /// wired unless the caller started one on `self.socket()` — the
    /// unreachable-coordinator scenarios rely on none existing.
    fn hook(&self, payload: &[u8]) -> (i32, String, String) {
        let mut child = Command::new(BIN)
            .args(["hook"])
            .env("RELAIS_STATE_DIR", &self.state)
            .env("RELAIS_CONFIG_DIR", &self.config)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("relais hook spawns");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(payload)
            .expect("write payload");
        let output = child.wait_with_output().expect("relais hook runs");
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    fn journal_path(&self) -> PathBuf {
        self.state.join("hook_journal.jsonl")
    }
}

/// Elect and serve a coordinator in a background thread. `start` uses
/// limits generous enough that nothing here is refused by a cap;
/// `start_with_limits` is how the cap test sets one low enough to bind,
/// which is the whole point of that test. Returns the running handle and
/// a client already able to register runs against it.
struct RunningCoordinator {
    server: Option<std::thread::JoinHandle<()>>,
    client: relais::coordinator::Client,
}

impl RunningCoordinator {
    fn start(socket: &Path) -> Self {
        Self::start_with_limits(
            socket,
            ConcurrencyLimits {
                max_active_agents: Some(64),
                max_active_agents_per_session: Some(64),
                max_heavy_commands: Some(8),
                max_training_jobs: Some(1),
                max_agent_depth: Some(8),
                max_agents_per_run: Some(256),
                training_when_idle: false,
            },
        )
    }

    fn start_with_limits(socket: &Path, limits: ConcurrencyLimits) -> Self {
        let (coordinator, listener) = Coordinator::start(
            socket,
            limits,
            None,
            relais::admission::DEFAULT_AGENT_LEASE_TTL,
        )
        .expect("coordinator starts");
        let server = std::thread::spawn(move || {
            let _ = coordinator.serve(listener);
        });
        let client = relais::coordinator::Client::new(socket.to_path_buf());
        // Wait for the daemon to actually be answering before the test
        // proceeds — `serve` sets up its accept loop on its own thread.
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

    fn register_run(&self, run_id: &str, session_id: &str) {
        self.client
            .request(&Request::RegisterRun {
                registration: relais::admission::RunRegistration {
                    run_id: run_id.to_string(),
                    session_id: session_id.to_string(),
                    budget_micros: None,
                    max_agents: None,
                    max_depth: None,
                },
            })
            .expect("register run");
    }

    fn stop(mut self) {
        let _ = self.client.request(&Request::Shutdown);
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
    }
}

fn spawn_payload(session: &str, tool_use: &str) -> Vec<u8> {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": session,
        "tool_name": "Agent",
        "tool_use_id": tool_use,
    })
    .to_string()
    .into_bytes()
}

/// The end of a spawn's tool call, carrying the same `tool_use_id` the
/// spawn did — which is what lets the end find the seat the start took.
fn finish_payload(session: &str, tool_use: &str) -> Vec<u8> {
    serde_json::json!({
        "hook_event_name": "PostToolUse",
        "session_id": session,
        "tool_name": "Agent",
        "tool_use_id": tool_use,
    })
    .to_string()
    .into_bytes()
}

/// A spawn in a session the coordinator has never heard of is answered
/// on its merits rather than refused for having no record: the command
/// derives and registers that session's own run and retries the
/// admission once, and with room to spare the retry is silent — through
/// the compiled binary against a coordinator that was never told about
/// this session beforehand.
#[test]
fn an_unregistered_session_is_admitted_silently_through_the_command() {
    let world = World::new("unregistered");
    let coordinator = RunningCoordinator::start(&world.socket());

    let payload = spawn_payload("session-never-registered", "tool-first");
    let (code, stdout, stderr) = world.hook(&payload);

    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert_eq!(
        stdout, "",
        "a self-registered first spawn with room is admitted silently: stderr={stderr}"
    );

    coordinator.stop();
}

/// A CAP is observed refusing a spawn, end to end, through the command:
/// with the machine's per-session agent cap set to 2, the first two
/// firings of one session are silent and the third is refused with a
/// message that names the limit — never `UnknownRun`, which would prove
/// only that the mechanism carries an answer, nothing about a limit.
#[test]
fn a_cap_refuses_a_spawn_through_the_command_end_to_end() {
    let world = World::new("cap");
    let limits = ConcurrencyLimits {
        max_active_agents: Some(64),
        max_active_agents_per_session: Some(2),
        max_heavy_commands: Some(8),
        max_training_jobs: Some(1),
        max_agent_depth: Some(8),
        max_agents_per_run: Some(256),
        training_when_idle: false,
    };
    let coordinator = RunningCoordinator::start_with_limits(&world.socket(), limits);

    let session = "session-capped";
    for tool_use in ["tool-1", "tool-2"] {
        let (code, stdout, stderr) = world.hook(&spawn_payload(session, tool_use));
        assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
        assert_eq!(
            stdout, "",
            "spawn {tool_use} is under the cap and should be silent: stderr={stderr}"
        );
    }

    let (code, stdout, stderr) = world.hook(&spawn_payload(session, "tool-3"));
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert!(
        stdout.contains("\"permissionDecision\":\"deny\""),
        "stdout={stdout}"
    );
    assert!(
        !stdout.contains("is not registered"),
        "the cap refusal must not be UnknownRun: stdout={stdout}"
    );
    // WHICH refusal, not merely that one arrived: `RunCancelled`,
    // `AlreadyFinished` or a depth refusal would all satisfy "a deny that
    // is not UnknownRun", and none of them would be the cap. The cap's
    // own rendering names the limit and offers retrying when an agent
    // finishes — which the seat being given back at the end of a call is
    // what makes true.
    assert!(
        stdout.contains("at its limit") || stdout.contains("limit"),
        "the refusal must be the cap's own, naming the limit: stdout={stdout}"
    );

    // And the cap counts RUNNING agents: end tool-1's call, and the seat
    // it held comes back, so the next spawn is admitted.
    let (code, stdout, stderr) = world.hook(&finish_payload(session, "tool-1"));
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert_eq!(stdout, "", "the end of a call is never refused");

    let (code, stdout, stderr) = world.hook(&spawn_payload(session, "tool-4"));
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert_eq!(
        stdout, "",
        "with a seat given back, the next spawn is admitted: stderr={stderr}"
    );

    coordinator.stop();
}

/// A coordinator that grants (the session's derived run is registered,
/// with room) produces empty stdout, through the command.
#[test]
fn a_granting_coordinator_produces_empty_stdout() {
    let world = World::new("grant");
    let coordinator = RunningCoordinator::start(&world.socket());

    let session = relais::ids::SessionId::new("session-grant");
    let run_id = relais::ids::derive_run_id(&session);
    coordinator.register_run(run_id.as_str(), session.as_str());

    let payload = spawn_payload(session.as_str(), "tool-grant");
    let (code, stdout, stderr) = world.hook(&payload);

    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert_eq!(
        stdout, "",
        "a granted spawn prints nothing: stderr={stderr}"
    );

    coordinator.stop();
}

/// No coordinator running at all: the default (`carry_on`) stays
/// silent and still exits zero — a coordinator outage must not turn
/// into a hook that blocks a session.
#[test]
fn an_unreachable_coordinator_carries_on_silently_by_default() {
    let world = World::new("unreachable");
    let payload = spawn_payload("session-unreachable", "tool-unreachable");
    let (code, stdout, stderr) = world.hook(&payload);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert_eq!(stdout, "");
}

/// A payload that is not JSON at all: still exits zero, still prints
/// nothing — the worst input `event::parse` can be handed.
#[test]
fn a_non_json_payload_exits_zero_and_prints_nothing() {
    let world = World::new("badjson");
    let (code, stdout, stderr) = world.hook(b"not json at all {{{");
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert_eq!(stdout, "");
}

/// An empty payload on stdin: still exits zero.
#[test]
fn an_empty_payload_exits_zero() {
    let world = World::new("empty");
    let (code, stdout, stderr) = world.hook(b"");
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert_eq!(stdout, "");
}

/// No machine.toml at all: settings default, and the command still
/// exits zero and answers exactly as the defaults say it should
/// (`carry_on`, so a spawn with no reachable coordinator is silent).
#[test]
fn a_missing_settings_file_exits_zero_with_default_behaviour() {
    let world = World::new("nosettings");
    assert!(!world.config.join("machine.toml").exists());
    let payload = spawn_payload("session-nosettings", "tool-nosettings");
    let (code, stdout, stderr) = world.hook(&payload);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert_eq!(stdout, "");
}

/// An invalid machine.toml: still exits zero rather than surfacing a
/// parse failure through the hook — `relais doctor` is where a bad
/// settings file is reported, not a hook holding a tool call open.
#[test]
fn an_invalid_settings_file_exits_zero() {
    let world = World::new("badsettings");
    std::fs::write(world.config.join("machine.toml"), "not valid toml {{{").expect("write");
    let payload = spawn_payload("session-badsettings", "tool-badsettings");
    let (code, stdout, stderr) = world.hook(&payload);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert_eq!(stdout, "");
}

/// `on_coordinator_unreachable = "refuse"` in machine.toml, with no
/// coordinator running: the command still exits zero, and this time
/// the refusal reaches stdout instead of staying silent.
#[test]
fn refuse_on_unreachable_still_exits_zero_and_refuses() {
    let world = World::new("refuseunreachable");
    std::fs::write(
        world.config.join("machine.toml"),
        "schema_version = 1\n\n[admission]\non_coordinator_unreachable = \"refuse\"\n",
    )
    .expect("write");
    let payload = spawn_payload("session-refuseunreachable", "tool-refuseunreachable");
    let (code, stdout, stderr) = world.hook(&payload);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert!(
        stdout.contains("could not reach its coordinator"),
        "stdout={stdout}"
    );
}

/// Every firing is journalled, one JSON line each, to an owner-only
/// file — through the command, across more than one firing.
#[test]
#[cfg(unix)]
fn every_firing_is_journalled_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let world = World::new("journal");
    let coordinator = RunningCoordinator::start(&world.socket());

    let (code1, _, _) = world.hook(&spawn_payload("session-journal-a", "tool-a"));
    assert_eq!(code1, 0);
    let (code2, _, _) = world.hook(&spawn_payload("session-journal-b", "tool-b"));
    assert_eq!(code2, 0);

    let journal = world.journal_path();
    let mode = std::fs::metadata(&journal)
        .expect("journal exists")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "journal must be owner-only");

    let text = std::fs::read_to_string(&journal).expect("read journal");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "one line per firing: {text}");
    for line in &lines {
        let value: serde_json::Value = serde_json::from_str(line).expect("journal line is JSON");
        // WHICH decision, not merely that one was written. These spawns
        // each name a session the coordinator has never heard of, and
        // each now self-registers its own run and is admitted with
        // room to spare, so both must record "silent" — asserting only
        // `is_string()` would pass a journal that recorded every firing
        // as "refuse" just as readily.
        assert_eq!(value["decision"], "silent", "{line}");
        assert!(value["reason"].is_null(), "{line}");
        assert!(value["payload"]["session_id"].is_string(), "{line}");
    }

    coordinator.stop();
}
