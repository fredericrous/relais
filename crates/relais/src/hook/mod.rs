//! The hook recorder and the live hook handler for Claude Code hook
//! payloads.
//!
//! Two pieces, deliberately unequal in what they know:
//!
//! - [`record`] is `relais hook --probe --record <dir>`: it reads one
//!   payload on stdin and writes it to disk byte for byte. It parses
//!   nothing beyond a best-effort event name for the file name, decides
//!   nothing, and cannot fail a session — a hook that exits non-zero or
//!   writes to stdout can block or alter the tool call that triggered
//!   it, so every error here is swallowed after being tried rather than
//!   surfaced.
//! - [`respond::handle`] is `relais hook` with no flags: the live path,
//!   which parses a payload with [`event::parse`], asks the coordinator
//!   about a spawn, applies [`decide::decide`] and prints the answer.
//!   This is the one that can refuse a tool call; the recorder never does.
//!
//! The fixtures the hook tests run against are transcribed from a real
//! session recorded this way, rather than assumed.

pub mod decide;
pub mod event;
pub mod native;
pub mod pairing;
pub mod respond;
pub mod worktree;

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// A payload past this size still gets recorded, up to the cap: the cap
/// exists so a hook that was handed something absurd cannot make the
/// probe (or the session it is watching) hang or exhaust memory, not so
/// an oversized payload is refused.
pub const MAX_PAYLOAD_BYTES: u64 = 64 * 1024 * 1024;

/// The eight hook targets relais reads: the three tool-scoped events, plus
/// the five lifecycle events (`WorktreeCreate` among them). Named once so
/// the settings installer and its checks cannot disagree about the list.
pub const TARGETS: [&str; 8] = [
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "SubagentStart",
    "SubagentStop",
    "SessionStart",
    "SessionEnd",
    "WorktreeCreate",
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
}
