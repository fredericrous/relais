//! The hook probe and the live hook handler for Claude Code hook
//! payloads.
//!
//! Three pieces, deliberately unequal in what they know:
//!
//! - [`record`] is `relais hook --probe --record <dir>`: it reads one
//!   payload on stdin and writes it to disk byte for byte. It parses
//!   nothing beyond a best-effort event name for the file name, decides
//!   nothing, and cannot fail a session — a hook that exits non-zero or
//!   writes to stdout can block or alter the tool call that triggered
//!   it, so every error here is swallowed after being tried rather than
//!   surfaced.
//! - [`probe`] is `relais doctor --probe-hooks`: it wires [`record`] into
//!   a throwaway settings file, runs one real `claude -p` session through
//!   it, and reads the recordings back to say which of the seven targets
//!   fired and which top-level fields their payloads carried. That
//!   reading is metadata about the recording (field names, not values;
//!   never a relais type), not the interpretation the packages this probe
//!   exists to give fixtures to will eventually do.
//! - [`respond::handle`] is `relais hook` with no flags: the live path,
//!   which parses a payload with [`event::parse`], asks the coordinator
//!   about a spawn, applies [`decide::decide`] and prints the answer.
//!   This is the one that can refuse a tool call; the two above never do.
//!
//! The fixtures every later package tests against are transcribed from a
//! real session this way, rather than assumed.

pub mod decide;
pub mod event;
pub mod pairing;
pub mod respond;

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::procs::{run_with_timeout, Ended};

/// A payload past this size still gets recorded, up to the cap: the cap
/// exists so a hook that was handed something absurd cannot make the
/// probe (or the session it is watching) hang or exhaust memory, not so
/// an oversized payload is refused.
pub const MAX_PAYLOAD_BYTES: u64 = 64 * 1024 * 1024;

/// The seven hook targets `relais doctor --probe-hooks` wires: the three
/// tool-scoped events, matched to the Agent tool, plus the four
/// lifecycle events. Named once so the settings file, the compatibility
/// record and its staleness check cannot disagree about the list.
pub const TARGETS: [&str; 7] = [
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "SubagentStart",
    "SubagentStop",
    "SessionStart",
    "SessionEnd",
];

/// One recorded hook invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedHook {
    /// Nanoseconds since the epoch when this payload arrived.
    pub order: u128,
    pub event: String,
    pub payload_path: PathBuf,
}

/// Read one hook payload from `stdin` and write it verbatim into `dir`,
/// named with the arrival order and the event name the payload claimed.
/// Never panics, never returns an error outward, never writes to
/// stdout: every I/O failure here is tried and dropped, because the
/// handler this drives (SPEC: `relais hook --probe --record`) cannot be
/// the thing that blocks or alters the session it is watching.
pub fn record(dir: &Path, order: u128, mut stdin: impl Read) -> RecordedHook {
    let mut bytes = Vec::new();
    // A broken pipe or a truncated write leaves `bytes` with whatever was
    // read so far; still recorded, never surfaced (the handler cannot fail).
    let _ = stdin
        .by_ref()
        .take(MAX_PAYLOAD_BYTES)
        .read_to_end(&mut bytes);

    let event = event_name(&bytes);
    // Best effort: a directory or write failure here would otherwise be
    // the handler's own fault to report, which it is forbidden to do.
    let _ = fs::create_dir_all(dir);

    // Two hooks CAN read the clock in the same nanosecond, and two
    // recordings of one event in one instant would otherwise overwrite
    // each other — losing a payload, the one thing a recorder may not
    // do. Claim a free name rather than assume one. The sequence is
    // always present and zero-padded, so a collision cannot reorder
    // anything: `…-000-` sorts before `…-001-`, where a bare name and a
    // `-1-` suffixed one sort the wrong way round ('1' < 'S') and
    // lexical order would stop being arrival order exactly when two
    // hooks raced.
    let mut payload_path = dir.join(format!("{order:021}-000-{event}.json"));
    for attempt in 1..=u32::MAX {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&payload_path)
        {
            Ok(mut file) => {
                use std::io::Write;
                let _ = file.write_all(&bytes);
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                payload_path = dir.join(format!("{order:021}-{attempt:03}-{event}.json"));
            }
            // Any other failure is the handler's own, and it is
            // forbidden to report one: the recording is lost, the
            // session is not disturbed.
            Err(_) => break,
        }
    }

    RecordedHook {
        order,
        event,
        payload_path,
    }
}

/// When a payload arrived, in nanoseconds since the epoch — the order
/// its recording is named for.
///
/// NOT a count of what is already in the directory. Hooks fire
/// concurrently and each is its own process, so two counting the same
/// directory in the same instant both see N and both claim `000N`.
/// Measured in a session that spawned five agents: four collisions, and
/// the ordinals 0003, 0006, 0013 and 0018 never existed. Two payloads
/// of one event in one instant went further and overwrote each other.
///
/// Arrival order is the whole point of a recording — it is the only
/// thing that says which `SubagentStart` followed which spawn, since no
/// payload carries both a `tool_use_id` and an `agent_id` — so the
/// recorder may not be what loses it. A clock read needs no
/// coordination between processes, and a tie is broken in [`record`]
/// rather than resolved by whoever happened to write last.
pub fn arrival_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        // A clock before the epoch is a machine problem, not a reason to
        // lose the recording: order from zero and keep going.
        .map(|since| since.as_nanos())
        .unwrap_or(0)
}

/// The event name a payload claims, read only far enough to name the
/// file — `hook_event_name` when the payload parses that far as JSON,
/// `"unknown"` otherwise (not valid JSON, not an object, or missing the
/// field). This is the one field the handler looks at, and only to
/// choose a file name; the bytes written are the whole payload, unread.
fn event_name(bytes: &[u8]) -> String {
    let name = serde_json::from_slice::<Value>(bytes)
        .ok()
        .and_then(|value| {
            value
                .get("hook_event_name")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "unknown".to_string());
    sanitize(&name)
}

/// A file-name-safe rendering of an event name that came from the
/// payload rather than from this crate's own vocabulary — a hook the
/// handler was not wired for can claim anything.
fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "unknown".to_string()
    } else {
        cleaned
    }
}

/// What one target's recordings said, reported as what was SEEN.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetObservation {
    pub target: String,
    pub fired: bool,
    /// The union of top-level JSON keys across every firing recorded for
    /// this target, sorted. Field names only — never a value, and never
    /// carried into a relais type.
    pub fields: Vec<String>,
}

/// The compatibility record `relais doctor --probe-hooks` writes: which
/// Claude Code version was observed, and what each target's payloads
/// looked like on it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompatRecord {
    pub claude_code_version: String,
    pub observed_at: String,
    pub targets: Vec<TargetObservation>,
}

/// Where the compatibility record lives: state-directory-relative, so
/// `doctor`'s normal run and `--probe-hooks` agree on it without either
/// naming a path to the other.
pub fn compat_record_path() -> Result<PathBuf, crate::paths::HomeUnset> {
    Ok(crate::paths::state_dir()?.join("hook_compat.json"))
}

/// Read the compatibility record, when one exists. A missing or
/// unreadable file is `None`, not an error: `doctor`'s normal run treats
/// "no record yet" as a finding to report, not a fault to surface.
pub fn read_compat_record(path: &Path) -> Option<CompatRecord> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// How long the one real session `--probe-hooks` runs may take. Generous:
/// it is asked to make a nested agent call, which is real model work,
/// not a version probe.
pub const PROBE_SESSION_TIMEOUT: Duration = Duration::from_secs(600);

/// The prompt the probe session is given. It has to exercise three
/// things the payloads cannot be read off without: an agent call, so
/// `SubagentStart`/`SubagentStop` fire; a tool call made BY the
/// subagent, so a payload whose caller is an agent is recorded and
/// `agent_id`'s presence there is a measurement rather than an
/// assumption; and a tool call that FAILS, so `PostToolUseFailure` has
/// something to fire on. An earlier prompt did only the first, and the
/// other two events read as "did not fire" — which says nothing about
/// the harness and everything about the prompt.
///
/// The probe does not care what any of it finds, only that the harness
/// routed through the events being probed.
pub const PROBE_PROMPT: &str = "Do exactly these two things and nothing else. \
First, use the Read tool on the path /nonexistent-relais-probe-target so that it fails; \
report only that it failed. Second, use the Task tool to launch one general-purpose \
subagent, and tell that subagent to itself use the Read tool on this repository's \
Cargo.toml and report back its first line. Print that line and stop.";

/// What one run of this probe measured on Claude Code 2.1.281, kept
/// here because the next person to read this module will want it before
/// they read the code: `agent_id` names the CALLER's agent and is absent
/// at the top level, present on every call a subagent makes;
/// `SubagentStart`/`SubagentStop` carry no `tool_use_id` and the spawn's
/// own `PreToolUse`/`PostToolUse` carry no `agent_id`, so nothing in a
/// single payload joins the admitting tool call to the agent that ran;
/// `prompt_id` is identical across a whole turn; and
/// `PostToolUseFailure` did not fire even for a tool call that failed.
/// The transcribed payloads are under `crates/relais/tests/fixtures/hooks`
/// with the full reading.
///
/// Why `--probe-hooks` could not complete. Distinguished the way the
/// capability probe's own failures are: "could not run" and "ran and
/// said nothing useful" are different faults to fix.
#[derive(Debug)]
pub enum ProbeHooksError {
    NoClaudeBinary,
    SettingsWrite(std::io::Error),
    SessionNotRun(String),
    Home(crate::paths::HomeUnset),
}

impl std::fmt::Display for ProbeHooksError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoClaudeBinary => write!(f, "no `claude` binary on PATH to probe"),
            Self::SettingsWrite(e) => write!(f, "could not write the throwaway settings file: {e}"),
            Self::SessionNotRun(detail) => write!(f, "the probe session did not run: {detail}"),
            Self::Home(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ProbeHooksError {}

/// Everything `relais doctor --probe-hooks` produced: the compatibility
/// record it wrote, and where the throwaway settings file and recording
/// directory live, so a person can go look at either.
#[derive(Debug)]
pub struct ProbeHooksReport {
    pub record: CompatRecord,
    pub settings_path: PathBuf,
    pub recording_dir: PathBuf,
}

/// Wire the seven targets into a throwaway settings file, run one real
/// `claude -p` session through it forcing a nested agent call, and read
/// back which targets fired and what their payloads carried. Never edits
/// the user's own settings.json: everything here lives under a
/// timestamped directory of its own.
pub fn probe(
    claude_binary: &Path,
    relais_binary: &Path,
) -> Result<ProbeHooksReport, ProbeHooksError> {
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
    let probe_dir = crate::paths::state_dir()
        .map_err(ProbeHooksError::Home)?
        .join("hook-probe")
        .join(stamp.to_string());
    let recording_dir = probe_dir.join("recordings");
    fs::create_dir_all(&recording_dir).map_err(ProbeHooksError::SettingsWrite)?;

    let settings_path = probe_dir.join("settings.json");
    let settings = settings_document(relais_binary, &recording_dir);
    fs::write(
        &settings_path,
        serde_json::to_string_pretty(&settings).expect("settings document serializes"),
    )
    .map_err(ProbeHooksError::SettingsWrite)?;

    let mut command = std::process::Command::new(claude_binary);
    command.args([
        "-p",
        PROBE_PROMPT,
        "--settings",
        settings_path.to_string_lossy().as_ref(),
    ]);
    let end = run_with_timeout(command, PROBE_SESSION_TIMEOUT, None, None, None)
        .map_err(|e| ProbeHooksError::SessionNotRun(e.to_string()))?;
    if !matches!(end.ended, Ended::Exited(_)) {
        return Err(ProbeHooksError::SessionNotRun(end.ended.describe()));
    }

    let claude_code_version = probe_version(claude_binary).unwrap_or_else(|| "unknown".to_string());
    let record = CompatRecord {
        claude_code_version,
        observed_at: chrono::Utc::now().to_rfc3339(),
        targets: TARGETS
            .iter()
            .map(|t| observe_target(&recording_dir, t))
            .collect(),
    };

    let compat_path = compat_record_path().map_err(ProbeHooksError::Home)?;
    if let Some(parent) = compat_path.parent() {
        // Best effort: the session already ran; a write failure here is
        // reported by `doctor`'s next run finding no record, not thrown here.
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(
        &compat_path,
        serde_json::to_string_pretty(&record).expect("compat record serializes"),
    );

    Ok(ProbeHooksReport {
        record,
        settings_path,
        recording_dir,
    })
}

/// `claude --version`, trimmed, best effort — the probe's own version
/// stamp does not need the full capability machinery.
fn probe_version(claude_binary: &Path) -> Option<String> {
    let mut command = std::process::Command::new(claude_binary);
    command.arg("--version");
    let end = run_with_timeout(command, Duration::from_secs(30), None, None, None).ok()?;
    if !end.ended.succeeded() {
        return None;
    }
    let text = format!("{}{}", end.stdout, end.stderr);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// What was recorded for one target: whether any file's event name
/// matches it, and the union of top-level field names those files
/// carried. Reads each recorded file as JSON to list its keys — nothing
/// beyond that: no value is read, and nothing here becomes a relais
/// type.
fn observe_target(recording_dir: &Path, target: &str) -> TargetObservation {
    let mut fields: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut fired = false;
    if let Ok(entries) = fs::read_dir(recording_dir) {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !file_name.ends_with(&format!("-{target}.json")) {
                continue;
            }
            fired = true;
            if let Ok(text) = fs::read_to_string(&path) {
                if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&text) {
                    fields.extend(map.keys().cloned());
                }
            }
        }
    }
    TargetObservation {
        target: target.to_string(),
        fired,
        fields: fields.into_iter().collect(),
    }
}

/// The throwaway settings document: every target from [`TARGETS`] wired
/// to `<relais_binary> hook --probe --record <recording_dir>`, the three
/// tool-scoped events matched to the Agent tool.
fn settings_document(relais_binary: &Path, recording_dir: &Path) -> Value {
    let command = format!(
        "{} hook --probe --record {}",
        relais_binary.display(),
        recording_dir.display()
    );
    let hook_entry = |matcher: Option<&str>| {
        let mut entry = serde_json::json!({
            "hooks": [{"type": "command", "command": command}]
        });
        if let Some(matcher) = matcher {
            entry["matcher"] = Value::String(matcher.to_string());
        }
        entry
    };
    serde_json::json!({
        "hooks": {
            // Every tool, not just `Agent`. Relais will only ever ACT on
            // the Agent ones, but the probe is gathering evidence, and
            // the question it most needs to answer is what a payload
            // looks like when the caller is itself a subagent — which
            // only a tool call made INSIDE one can show. Matching Agent
            // alone records the top-level spawn and nothing the subagent
            // then does, so `agent_id`'s absence would prove nothing.
            "PreToolUse": [hook_entry(None)],
            "PostToolUse": [hook_entry(None)],
            "PostToolUseFailure": [hook_entry(None)],
            "SubagentStart": [hook_entry(None)],
            "SubagentStop": [hook_entry(None)],
            "SessionStart": [hook_entry(None)],
            "SessionEnd": [hook_entry(None)],
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp_dir;
    use std::io::Cursor;

    #[test]
    fn records_a_valid_payload_verbatim_and_names_it_by_event() {
        let dir = temp_dir("hook-record");
        let payload = br#"{"hook_event_name":"PreToolUse","tool_name":"Agent"}"#;
        let recorded = record(&dir, 0, Cursor::new(payload.to_vec()));
        assert_eq!(recorded.event, "PreToolUse");
        let on_disk = fs::read(&recorded.payload_path).unwrap();
        assert_eq!(on_disk, payload);
    }

    #[test]
    fn records_invalid_json_verbatim_as_unknown() {
        let dir = temp_dir("hook-record");
        let payload = b"not json at all {{{";
        let recorded = record(&dir, 0, Cursor::new(payload.to_vec()));
        assert_eq!(recorded.event, "unknown");
        assert_eq!(fs::read(&recorded.payload_path).unwrap(), payload);
    }

    #[test]
    fn records_a_payload_for_an_unwired_event_verbatim() {
        let dir = temp_dir("hook-record");
        let payload = br#"{"hook_event_name":"SomethingNew","x":1}"#;
        let recorded = record(&dir, 0, Cursor::new(payload.to_vec()));
        assert_eq!(recorded.event, "SomethingNew");
        assert_eq!(fs::read(&recorded.payload_path).unwrap(), &payload[..]);
    }

    #[test]
    fn a_payload_past_the_cap_is_still_recorded_up_to_it() {
        let dir = temp_dir("hook-record");
        let big = vec![b'a'; (MAX_PAYLOAD_BYTES + 10) as usize];
        let recorded = record(&dir, 0, Cursor::new(big));
        let on_disk = fs::read(&recorded.payload_path).unwrap();
        assert_eq!(on_disk.len() as u64, MAX_PAYLOAD_BYTES);
    }

    /// The failure this test exists for. Order used to be the count of
    /// files already in the directory, and hooks fire concurrently as
    /// separate processes: two reading it in the same instant both saw
    /// N and both wrote `000N`. Measured in a session that spawned five
    /// agents — four collisions, and the ordinals 0003, 0006, 0013 and
    /// 0018 never existed. Here both payloads are the SAME event at the
    /// SAME instant, which under the old scheme did not merely misnumber
    /// them: the second overwrote the first, losing a recording.
    #[test]
    fn two_payloads_at_one_instant_are_both_kept_and_ordered() {
        let dir = temp_dir("hook-order");
        let instant = arrival_nanos();
        let body = |n: u8| format!(r#"{{"hook_event_name":"SubagentStart","n":{n}}}"#).into_bytes();

        let first = record(&dir, instant, Cursor::new(body(1)));
        let second = record(&dir, instant, Cursor::new(body(2)));
        assert_ne!(
            first.payload_path, second.payload_path,
            "one instant must not cost a recording"
        );

        let mut names: Vec<String> = fs::read_dir(&dir)
            .expect("recordings")
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names.len(), 2, "both payloads survive: {names:?}");
        // Zero-padded throughout, so lexical order IS arrival order —
        // what a later reader sorts by to see which start followed which
        // spawn.
        for (name, want) in names.iter().zip([r#""n":1"#, r#""n":2"#]) {
            assert!(name.starts_with(&format!("{instant:021}-")), "{names:?}");
            let body = fs::read_to_string(dir.join(name)).expect("payload");
            assert!(body.contains(want), "{name} holds {body}");
        }
    }

    /// Arrival order is a clock, not a count, so it rises without the
    /// directory being consulted at all — the property the old scheme
    /// lacked, and the reason two concurrent hooks no longer collide.
    #[test]
    fn arrival_order_rises_without_consulting_the_directory() {
        let first = arrival_nanos();
        let second = arrival_nanos();
        assert!(second >= first, "{second} < {first}");
        assert!(
            first > 0,
            "a failed clock read would order every payload at zero"
        );
    }

    #[test]
    fn observe_target_reports_not_fired_when_nothing_matched() {
        let dir = temp_dir("hook-observe");
        let observation = observe_target(&dir, "SessionStart");
        assert!(!observation.fired);
        assert!(observation.fields.is_empty());
    }

    #[test]
    fn observe_target_unions_field_names_across_firings() {
        let dir = temp_dir("hook-observe");
        record(
            &dir,
            0,
            Cursor::new(br#"{"hook_event_name":"SessionStart","a":1}"#.to_vec()),
        );
        record(
            &dir,
            1,
            Cursor::new(br#"{"hook_event_name":"SessionStart","b":2}"#.to_vec()),
        );
        let observation = observe_target(&dir, "SessionStart");
        assert!(observation.fired);
        assert_eq!(
            observation.fields,
            vec![
                "a".to_string(),
                "b".to_string(),
                "hook_event_name".to_string()
            ]
        );
    }

    #[test]
    fn settings_document_wires_all_seven_targets() {
        let doc = settings_document(Path::new("/usr/local/bin/relais"), Path::new("/tmp/rec"));
        let hooks = doc.get("hooks").unwrap().as_object().unwrap();
        for target in TARGETS {
            assert!(hooks.contains_key(target), "missing {target}");
        }
        // No matcher on the tool events, deliberately. Relais will only
        // ever ACT on `Agent` calls, but the probe is gathering evidence,
        // and the payload it most needs is the one from a tool call made
        // INSIDE a subagent — which matching `Agent` alone never records.
        // Matched to `Agent`, a first run of this probe saw `agent_id` on
        // no tool event at all and read as proof the field does not
        // exist; widened, the same session showed it present on every
        // call the subagent made. A narrower instrument here does not
        // collect less, it collects something misleading.
        for tool_event in ["PreToolUse", "PostToolUse", "PostToolUseFailure"] {
            assert!(
                hooks[tool_event][0].get("matcher").is_none(),
                "{tool_event} must record every tool, not only Agent"
            );
        }
        assert!(hooks["SessionStart"][0].get("matcher").is_none());
    }
}
