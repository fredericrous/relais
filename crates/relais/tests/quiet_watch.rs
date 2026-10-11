//! `relais quiet watch --protocol` against a temp active-run directory.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

struct Watcher {
    child: Child,
    lines: Receiver<Value>,
    /// The first line, read while waiting for the watcher to be ready.
    first: std::cell::RefCell<Option<Value>>,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("relais-quiet-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("active")).unwrap();
    dir
}

fn watch(active: &Path) -> Watcher {
    let mut child = Command::new(env!("CARGO_BIN_EXE_relais"))
        .args(["quiet", "watch", "--protocol"])
        .env("QUIET_ACTIVE_DIR", active)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, lines) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(serde_json::from_str(&line).unwrap()).is_err() {
                break;
            }
        }
    });
    // Ready once it has said anything (it beats at startup). macOS can hold
    // the first exec of a freshly linked binary for many seconds on a loaded
    // machine, and `cargo test` relinks it with other features than `cargo
    // build`: the timed assertions start after this, not at spawn.
    let first = lines
        .recv_timeout(Duration::from_secs(60))
        .expect("the watcher said nothing within 60 s");
    Watcher {
        child,
        lines,
        first: std::cell::RefCell::new(Some(first)),
    }
}

impl Watcher {
    /// The next non-heartbeat event, or None after `wait`.
    fn next(&self, wait: Duration) -> Option<Value> {
        let end = Instant::now() + wait;
        loop {
            if let Some(v) = self.first.borrow_mut().take() {
                if v["event"] == "heartbeat" {
                    continue;
                }
                return Some(v);
            }
            let left = end.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(v) if v["event"] == "heartbeat" => continue,
                Ok(v) => return Some(v),
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return None,
            }
        }
    }

    fn expect(&self, event: &str) -> Value {
        let v = self.next(Duration::from_secs(5)).unwrap_or(Value::Null);
        assert_eq!(v["event"], event, "got {v}");
        assert_eq!(v["relais"], "quiet");
        v
    }
}

fn dead_pid() -> u32 {
    let mut child = Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    pid
}

fn write_entry(active: &Path, id: &str, log: &Path, pid: u32, session: Value, extra: Value) {
    let mut entry = json!({"v": 1, "tool": "amont", "session": session, "check": "clippy",
        "repo": "/r", "log": log, "pid": pid, "started": 1000});
    for (k, v) in extra.as_object().unwrap() {
        entry[k] = v.clone();
    }
    let tmp = active.join(format!("{id}.json.tmp"));
    fs::write(&tmp, entry.to_string()).unwrap();
    fs::rename(&tmp, active.join(format!("{id}.json"))).unwrap();
}

fn append(log: &Path, bytes: &[u8]) {
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .unwrap();
    f.write_all(bytes).unwrap();
}

fn lines_of(v: &Value) -> Vec<String> {
    v["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn live_entry_streams_started_output_and_ended() {
    for session in [json!("s-1"), Value::Null] {
        let dir = scratch("live");
        let active = dir.join("active");
        let log = dir.join("a.log.tmp");
        append(&log, b"old line\n");
        write_entry(
            &active,
            "1-1",
            &log,
            std::process::id(),
            session.clone(),
            json!({}),
        );
        let w = watch(&active);
        let started = w.expect("started");
        assert_eq!(started["id"], "1-1");
        assert_eq!(started["session"], session);
        assert_eq!(started["tool"], "amont");
        assert_eq!(started["check"], "clippy");
        assert_eq!(started["repo"], "/r");
        assert_eq!(started["started"], 1000);
        assert_eq!(lines_of(&w.expect("output")), ["old line"]);
        append(&log, b"one\ntwo\nthree\npartial");
        assert_eq!(lines_of(&w.expect("output")), ["one", "two", "three"]);
        append(&log, b" end\n");
        assert_eq!(lines_of(&w.expect("output")), ["partial end"]);
        fs::remove_file(active.join("1-1.json")).unwrap();
        let ended = w.expect("ended");
        assert_eq!(ended["id"], "1-1");
        assert_eq!(ended["exit"], Value::Null);
        let _ = fs::remove_dir_all(&dir);
    }
}

#[test]
fn an_entry_first_seen_ended_yields_started_output_ended_then_is_deleted() {
    let dir = scratch("ended");
    let active = dir.join("active");
    let log = dir.join("a.log.tmp");
    append(&log, b"error: boom\nlast words");
    write_entry(
        &active,
        "2-1",
        &log,
        dead_pid(),
        json!("s"),
        json!({"exit": 1, "ended": 2000}),
    );
    let w = watch(&active);
    w.expect("started");
    assert_eq!(lines_of(&w.expect("output")), ["error: boom", "last words"]);
    let ended = w.expect("ended");
    assert_eq!(
        (ended["exit"].clone(), ended["ended"].clone()),
        (json!(1), json!(2000))
    );
    let end = Instant::now() + Duration::from_secs(3);
    while active.join("2-1.json").exists() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!active.join("2-1.json").exists());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_dead_pid_without_ended_is_never_started() {
    let dir = scratch("dead");
    let active = dir.join("active");
    let log = dir.join("a.log.tmp");
    append(&log, b"x\n");
    write_entry(&active, "3-1", &log, dead_pid(), json!("s"), json!({}));
    let w = watch(&active);
    assert!(w.next(Duration::from_millis(1500)).is_none());
    assert!(active.join("3-1.json").exists(), "younger than 1 h: kept");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_heartbeat_arrives_within_six_seconds() {
    let dir = scratch("beat");
    let w = watch(&dir.join("active"));
    // With no entries, the first line is the startup beat; the next one
    // comes within the 5 s interval.
    let first = w.first.borrow_mut().take().unwrap();
    assert_eq!(first, json!({"relais": "quiet", "event": "heartbeat"}));
    let v = w.lines.recv_timeout(Duration::from_secs(6)).unwrap();
    assert_eq!(v, json!({"relais": "quiet", "event": "heartbeat"}));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_burst_yields_at_most_ten_lines_per_poll() {
    let dir = scratch("burst");
    let active = dir.join("active");
    let log = dir.join("a.log.tmp");
    append(&log, b"");
    write_entry(
        &active,
        "4-1",
        &log,
        std::process::id(),
        json!("s"),
        json!({}),
    );
    let w = watch(&active);
    w.expect("started");
    let burst: String = (0..30_000)
        .map(|i| format!("line {i:04} xxxxxxxxxxxxxxxxxxxxxxxxxxxx\n"))
        .collect();
    assert!(burst.len() > 1_000_000);
    append(&log, burst.as_bytes());
    let out = w.expect("output");
    assert!(lines_of(&out).len() <= 10, "{out}");
    assert!(lines_of(&out).last().unwrap().starts_with("line "));
    // Nothing from the skipped middle arrives later: the next poll has no news.
    while let Some(v) = w.next(Duration::from_millis(800)) {
        assert!(lines_of(&v).len() <= 10, "{v}");
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn lines_are_cut_as_timeline_cut_line_does() {
    let dir = scratch("cut");
    let active = dir.join("active");
    let log = dir.join("a.log.tmp");
    append(&log, b"");
    write_entry(
        &active,
        "5-1",
        &log,
        std::process::id(),
        json!("s"),
        json!({}),
    );
    let w = watch(&active);
    w.expect("started");
    let long = "日".repeat(300);
    let input = format!("\u{1b}[31mred\u{1b}[0m\tx\u{7}y\n{long}\n");
    append(&log, input.as_bytes());
    let out = w.expect("output");
    assert_eq!(
        lines_of(&out),
        ["red  xy".to_string(), format!("{}…", "日".repeat(239))]
    );
    let _ = fs::remove_dir_all(&dir);
}
