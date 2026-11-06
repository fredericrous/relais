//! A check run from a process that owns a controlling terminal must never
//! read that terminal (relais#383). Under a terminal, a check that asked a
//! question on `/dev/tty` was stopped by SIGTTIN and sat there until its
//! timeout: amont's install journey did exactly that under relais.
//!
//! The test process may have no terminal of its own, so it re-runs this
//! test binary as a helper in a new session whose controlling terminal is
//! a fresh pseudo-terminal, the way an interactive shell holds one. The
//! helper runs the check through the production launcher
//! ([`run_observed`]). A machine with no pseudo-terminal fails the test; it
//! never skips.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::*;

/// Set in the helper: which check it runs.
const CASE_ENV: &str = "RELAIS_TERMINAL_TEST_CASE";

/// The shell command line the `command` case runs.
const COMMAND_ENV: &str = "RELAIS_TERMINAL_TEST_COMMAND";

/// The check's own timeout. A stopped check reaches it; an isolated one
/// ends in milliseconds.
const CHECK_TIMEOUT_SECONDS: u64 = 5;

/// Runs the helper for `case` under a new session with the pseudo-terminal
/// as its controlling terminal (and stdin), and fails with its output when
/// the helper fails.
fn run_under_a_terminal(case: &str) {
    let (master, slave) = crate::procs::open_test_pty();
    let mut helper = Command::new(std::env::current_exe().expect("test binary"));
    helper
        .args([
            "--exact",
            "verify::terminal_tests::helper_runs_a_check",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CASE_ENV, case)
        .stdin(Stdio::from(slave))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::procs::take_terminal_on_spawn(&mut helper);
    let mut child = helper.spawn().expect("spawn the helper");
    let deadline = Instant::now() + Duration::from_secs(120);
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("the helper for {case} did not end within 120 s");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    drop(master);
    let mut out = String::new();
    let mut err = String::new();
    child
        .stdout
        .take()
        .expect("stdout")
        .read_to_string(&mut out)
        .ok();
    child
        .stderr
        .take()
        .expect("stderr")
        .read_to_string(&mut err)
        .ok();
    assert!(
        status.success(),
        "helper {case} failed ({status}):\n{out}\n{err}"
    );
}

/// A check that opens `/dev/tty` gets an error at once, instead of being
/// stopped until its timeout.
#[test]
fn a_check_cannot_read_the_controlling_terminal() {
    run_under_a_terminal("tty");
}

/// A check's stdin is at end of file, never the runner's terminal.
#[test]
fn a_check_reads_end_of_file_on_stdin() {
    run_under_a_terminal("stdin");
}

/// A real command, run under a terminal through the production launcher,
/// must pass on its own: the after-check for relais#383 on amont's install
/// journey. Ignored: it runs only with `RELAIS_TERMINAL_TEST_COMMAND` set
/// to a shell command line.
#[test]
#[ignore = "needs RELAIS_TERMINAL_TEST_COMMAND"]
fn a_real_command_passes_under_a_terminal() {
    if std::env::var_os(COMMAND_ENV).is_none() {
        return;
    }
    run_under_a_terminal("command");
}

/// The helper's half: runs one check through the production launcher and
/// asserts it ended on its own, in its own session and group. Ignored so a
/// plain test run never starts it; it does nothing without [`CASE_ENV`].
#[test]
#[ignore = "run by the terminal tests, under a pseudo-terminal"]
fn helper_runs_a_check() {
    let Ok(case) = std::env::var(CASE_ENV) else {
        return;
    };
    // Its own group: `pgid == pid`. Its own session: `sid == pid`.
    let identity = "pid=$$; pgid=$(ps -o pgid= -p $$ | tr -d ' '); \
                    sid=$(ps -o sess= -p $$ | tr -d ' '); echo pid=$pid pgid=$pgid sid=$sid; \
                    [ \"$pgid\" = \"$pid\" ] && echo own-group";
    let body = match case.as_str() {
        "tty" => "cat /dev/tty; echo rc=$?",
        "stdin" => "read line; echo rc=$?",
        "command" => return run_a_real_command(identity),
        other => panic!("unknown case {other}"),
    };
    let script = format!("{identity}; {body}");
    let dir = crate::test_support::temp_dir(&format!("verify-terminal-{case}"));
    let events = Events::for_run("run-terminal", &dir.join("art"));
    let spec = CommandSpec {
        name: None,
        argv: vec!["sh".into(), "-c".into(), script],
        timeout_seconds: CHECK_TIMEOUT_SECONDS,
        junit: None,
        weight: None,
        retry: None,
        env: Default::default(),
        reuse: None,
    };
    let started = Instant::now();
    let outcome = run_observed(
        &dir,
        &spec,
        &dir.join("logs"),
        "terminal",
        "terminal-cmd0",
        &events,
        None,
    )
    .expect("the check runs");
    let elapsed = started.elapsed();
    let log = std::fs::read_to_string(&outcome.log_path).expect("the check's log");
    let ended = format!("{:?}", outcome.ended);
    assert!(
        !ended.contains("TimedOut") && elapsed < Duration::from_secs(CHECK_TIMEOUT_SECONDS),
        "the {case} check was stopped until its timeout ({ended}, {elapsed:?}):\n{log}"
    );
    assert!(
        log.contains("own-group"),
        "the check is its own group:\n{log}"
    );
    assert!(
        log.lines().any(|l| l.starts_with("rc=") && l != "rc=0"),
        "the {case} check read nothing and said so:\n{log}"
    );
}

/// The `command` case: the command passes, in its own group, with no
/// terminal to stop it.
fn run_a_real_command(identity: &str) {
    let line = std::env::var(COMMAND_ENV).expect("the command line");
    let dir = crate::test_support::temp_dir("verify-terminal-command");
    let events = Events::for_run("run-terminal", &dir.join("art"));
    let spec = CommandSpec {
        name: None,
        argv: vec!["sh".into(), "-c".into(), format!("{identity}; {line}")],
        timeout_seconds: 240,
        junit: None,
        weight: None,
        retry: None,
        env: Default::default(),
        reuse: None,
    };
    let started = Instant::now();
    let outcome = run_observed(
        &dir,
        &spec,
        &dir.join("logs"),
        "terminal",
        "terminal-cmd0",
        &events,
        None,
    )
    .expect("the command runs");
    let log = std::fs::read_to_string(&outcome.log_path).expect("the command's log");
    println!("{:?} in {:?}", outcome.ended, started.elapsed());
    assert!(
        outcome.ended.succeeded(),
        "the command failed ({:?}):\n{log}",
        outcome.ended
    );
    assert!(
        log.contains("own-group"),
        "the command is its own group:\n{log}"
    );
}
