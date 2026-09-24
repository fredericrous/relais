//! Every payload `relais doctor --probe-hooks` recorded from a real
//! session must parse into the typed [`relais::hook::event::HookEvent`]
//! it actually IS.
//!
//! The walk names no file, so a payload a later probe run adds is
//! covered the moment it lands. What each payload must become is derived
//! from the recording's own name and contents — the event in the file
//! name, and for a tool event the `tool_name` inside it — rather than
//! from a list here that would have to be edited alongside.
//!
//! Asserting only that parsing does not panic would assert nothing at
//! all: [`parse`] is total, answering `NotOurs` for whatever it cannot
//! classify, so a renamed envelope field would quietly turn every
//! fixture into `NotOurs` and leave a "they all parse" test green. That
//! exact break — `session_id` renamed — was tried against this file, and
//! it fails here.

use std::fs;
use std::path::{Path, PathBuf};

use relais::hook::event::{parse, HookEvent};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hooks")
}

/// Which variant a recording must parse into, read off the recording
/// itself: the event name is the part of the file name after the arrival
/// order, and a tool event is ours only when its `tool_name` is the
/// Agent tool (under either its current or its legacy spelling).
fn expected(path: &Path, bytes: &[u8]) -> &'static str {
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.split_once('-'))
        .map(|(_order, event)| event)
        .unwrap_or("");
    let tool = serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|v| {
            v.get("tool_name")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_default();
    let is_agent_tool = tool == "Agent" || tool == "Task";
    match name {
        "SessionStart" => "SessionStart",
        "SessionEnd" => "SessionEnd",
        "SubagentStart" => "SubagentStart",
        "SubagentStop" => "SubagentStop",
        // One variant covers the pre and post phases of an Agent tool
        // call, carrying the phase inside it — `0002` and `0007` are the
        // two halves of one spawn and both land here.
        "PreToolUse" | "PostToolUse" if is_agent_tool => "AgentToolCall",
        // A tool event for any other tool is a real event relais simply
        // does not act on — `0001` is a `Read`.
        "PreToolUse" | "PostToolUse" | "PostToolUseFailure" => "NotOurs",
        other => panic!("fixture names an event this test does not map: {other}"),
    }
}

fn variant_of(event: &HookEvent) -> &'static str {
    match event {
        HookEvent::SessionStart(_) => "SessionStart",
        HookEvent::SessionEnd(_) => "SessionEnd",
        HookEvent::SubagentStart(_) => "SubagentStart",
        HookEvent::SubagentStop(_) => "SubagentStop",
        HookEvent::AgentToolCall(_) => "AgentToolCall",
        HookEvent::NotOurs => "NotOurs",
    }
}

#[test]
fn every_recorded_payload_parses_into_what_it_is() {
    let dir = fixtures_dir();
    let mut checked = 0usize;
    for entry in fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let bytes = fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let want = expected(&path, &bytes);
        let got = variant_of(&parse(&bytes));
        assert_eq!(
            got,
            want,
            "{} parsed as {got}, not {want}",
            path.file_name().and_then(|n| n.to_str()).unwrap_or("?")
        );
        checked += 1;
    }
    // Not a fixed count: a later probe run may add payloads, and this
    // test exists so those are covered without being listed. Zero,
    // though, means the fixtures are gone and every assertion above
    // silently did nothing.
    assert!(
        checked > 0,
        "no recorded payloads under {} — this test proves nothing",
        dir.display()
    );
}

/// The caller's agent is optional because it is genuinely absent at the
/// top level, and that is a measurement, not a convention: `0002` is the
/// spawn itself, made by the main session, and carries no `agent_id`;
/// `0004` is a call the subagent made, and carries one. A parser that
/// required the field would reject every top-level spawn — which is
/// exactly the event admission has to act on.
#[test]
fn the_caller_is_present_only_when_a_subagent_made_the_call() {
    let spawn = fs::read(fixtures_dir().join("0002-PreToolUse.json")).expect("read fixture");
    let HookEvent::AgentToolCall(spawn) = parse(&spawn) else {
        panic!("0002 is the spawn: an agent tool call");
    };
    assert!(
        spawn.caller_agent_id.is_none(),
        "the main session made this call, so there is no calling agent"
    );

    let nested = fs::read(fixtures_dir().join("0004-PreToolUse.json")).expect("read fixture");
    assert_eq!(
        parse(&nested),
        HookEvent::NotOurs,
        "0004 is the subagent's own Read: a tool event relais does not act on"
    );
    let value: serde_json::Value = serde_json::from_slice(&nested).expect("fixture is json");
    assert!(
        value.get("agent_id").is_some(),
        "…but it DOES carry the caller's agent id, which is the measurement \
         the optionality of `caller` is pinned to"
    );
}
