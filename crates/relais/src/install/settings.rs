//! Wiring the live hook into `settings.json` (SPEC §3, §23).
//!
//! `relais install --claude` alone writes four owned files and never
//! reads or writes `settings.json` at all — that file is a person's own,
//! hand-maintained, and wiring a hook into it is a separate, explicit
//! ask (`--hooks`). This module plans and applies exactly that: one
//! relais handler on each of the seven targets named in
//! [`crate::hook::TARGETS`], appended into whatever is already there
//! rather than replacing it.
//!
//! `settings.json` is not a template with a marked block the way the
//! agent and skill files are — it is arbitrary JSON a person edits by
//! hand, and there is no comment syntax to hide a marker in. So instead
//! of a hash-checked block, every write goes through a byte-for-byte
//! round trip first: parse the file, re-render it with `serde_json`
//! (`preserve_order` is deliberately off — see the crate's `Cargo.toml`
//! — so keys always sort, which is also what keeps `canonical_json_hash`
//! stable), and compare. A file that already looks like that survives an
//! edit losslessly; a hand-formatted one does not, and rewriting it
//! anyway would bury the one line relais actually wants to add inside a
//! reformatting nobody asked for. So an unrenderable file is refused
//! outright: nothing is written, and the fragment relais would have
//! added is printed for a person to paste in by hand.
//!
//! "Byte for byte" means exactly that, with one stated exception: the
//! trailing newline, which `serde_json`'s pretty printer does not emit
//! and nearly every editor adds. A file whose only difference from the
//! canonical rendering is that one newline is accepted, and [`render_like`]
//! then writes the tail it arrived with, so the accepted difference stays
//! a difference relais does not make. A file with trailing blank lines, a
//! `\r\n` tail or anything else in that position is refused like any
//! other unrenderable file, because relais cannot write it back as it
//! found it.
//!
//! Ownership of an existing array entry is never inferred and nothing is
//! ever deleted at that granularity: relais recognises only the leaf
//! hook object whose `command` is its own absolute binary path, and adds
//! or removes exactly that leaf. An entry object relais's own matcher
//! would have created — same event, same matcher string — is joined:
//! relais's command is appended into its `hooks` array rather than being
//! duplicated beside it. An entry on a DIFFERENT matcher is not joined
//! and gets its own entry, because a matcher is a statement about which
//! tool calls a handler wants to see and relais's is not that author's
//! to widen: joining `Bash` would make their block fire relais on every
//! shell command, or make relais's matcher govern their handler. Either
//! way the entry stops saying what it said. And no entry is ever
//! deleted, whoever put it there and however empty it ends up: an entry
//! this program did not create is not its call to remove, and it cannot
//! tell "empty because I un-joined it" from "empty because someone else
//! left it that way" apart.

use serde_json::Value;
use std::path::Path;

/// The seven live targets, paired with the matcher relais installs for
/// each. The three tool-scoped events are matched to the Agent tool and
/// its legacy name `Task` (the harness still sends either); the four
/// lifecycle events get no matcher at all, because they are not tool
/// calls. This is the SAME set of names as [`crate::hook::TARGETS`] —
/// never a second list to keep in step by hand — differing only in the
/// matcher column, which is where it parts company with the probe: the
/// handlers `hook::probe` writes (`hook::settings_document`) install no
/// matcher on any of the seven, deliberately, and the comment there says
/// why. Matched to `Agent` alone, a probe would record the top-level
/// spawn and nothing a subagent then does.
pub const HOOK_TARGETS: [(&str, Option<&str>); 7] = [
    ("PreToolUse", Some("Agent|Task")),
    ("PostToolUse", Some("Agent|Task")),
    ("PostToolUseFailure", Some("Agent|Task")),
    ("SubagentStart", None),
    ("SubagentStop", None),
    ("SessionStart", None),
    ("SessionEnd", None),
];

/// The exact command relais installs: the absolute path of the binary
/// that is running, never a bare name resolved from PATH — a command
/// not found exits 127, which lands in Claude Code's non-blocking
/// bucket, and enforcement would disappear with nothing to notice it.
pub fn hook_command(relais_binary: &Path) -> String {
    format!("{} hook", relais_binary.display())
}

/// What one target's plan says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEventAction {
    /// relais's command is already on this event: nothing to do.
    Current,
    /// An entry already matches relais's matcher; its command joins that
    /// entry's `hooks` array.
    JoinExisting,
    /// No entry matches relais's matcher; a new entry is appended.
    NewEntry,
}

impl HookEventAction {
    pub fn changes_anything(self) -> bool {
        !matches!(self, HookEventAction::Current)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookEventPlan {
    pub event: &'static str,
    pub action: HookEventAction,
}

/// What planning the hook wiring found.
#[derive(Debug, Clone, PartialEq)]
pub enum HooksPlan {
    /// The file (or its absence) is safe to edit: here is what each of
    /// the seven targets needs.
    Ready { events: Vec<HookEventPlan> },
    /// The file cannot be re-rendered byte for byte, so nothing will be
    /// written. `paste_block` is the fragment a person can merge in by
    /// hand.
    Unrenderable { reason: String, paste_block: String },
}

impl HooksPlan {
    pub fn applicable_count(&self) -> usize {
        match self {
            HooksPlan::Ready { events } => events
                .iter()
                .filter(|plan| plan.action.changes_anything())
                .count(),
            HooksPlan::Unrenderable { .. } => 0,
        }
    }
}

/// The canonical rendering of a settings document: `serde_json`'s own
/// pretty printer, with `preserve_order` off so an object's keys always
/// come out sorted. This is deliberately the ONLY spelling relais ever
/// writes, so a file this crate wrote always round-trips on the next run.
pub fn render_canonical(value: &Value) -> String {
    serde_json::to_string_pretty(value).expect("a parsed JSON Value always re-serializes")
}

/// Whether re-rendering `value` reproduces `original` byte for byte,
/// allowing the single trailing newline `serde_json`'s pretty printer
/// does not emit and almost every editor adds — and nothing else in that
/// position. Trailing blank lines, a `\r\n` tail or trailing spaces all
/// fail here, because relais would write a different tail than it found,
/// and "nothing else changes" has to include the last byte of the file.
fn round_trips(original: &str, value: &Value) -> bool {
    let rendered = render_canonical(value);
    original == rendered || original.strip_suffix('\n') == Some(rendered.as_str())
}

/// The bytes to write for an edited document, with the same tail the
/// original arrived with: a file that ended in a newline gets one back,
/// a file that did not, does not, and an absent file is created the way
/// every other text file here is — with one. [`round_trips`] accepts
/// exactly these two tails, so this can always reproduce the one it saw.
pub fn render_like(original: Option<&str>, value: &Value) -> String {
    let rendered = render_canonical(value);
    match original {
        Some(text) if !text.ends_with('\n') => rendered,
        _ => format!("{rendered}\n"),
    }
}

/// The fragment relais would add, for a person to paste in by hand when
/// the file itself is refused. Shows every target relais installs,
/// exactly as [`apply_hooks`] would add it to an empty document.
fn paste_block(relais_binary: &Path) -> String {
    let mut value = serde_json::json!({});
    apply_hooks(&mut value, relais_binary);
    render_canonical(&value)
}

/// Plan the hook wiring against a settings document that may not exist
/// yet (`existing_text: None`) or may be invalid or unrenderable JSON.
pub fn plan_hooks(existing_text: Option<&str>, relais_binary: &Path) -> HooksPlan {
    let value: Value = match existing_text {
        None => serde_json::json!({}),
        Some(text) => match serde_json::from_str(text) {
            Ok(value) => value,
            Err(e) => {
                return HooksPlan::Unrenderable {
                    reason: format!("settings.json is not valid JSON: {e}"),
                    paste_block: paste_block(relais_binary),
                }
            }
        },
    };
    if let Some(text) = existing_text {
        if !round_trips(text, &value) {
            return HooksPlan::Unrenderable {
                reason: "settings.json cannot be re-rendered byte for byte (its key order, \
                         spacing or formatting differs from relais's canonical JSON writer); \
                         editing it here would bury the change in reformatting nobody asked \
                         for"
                .to_string(),
                paste_block: paste_block(relais_binary),
            };
        }
    }
    HooksPlan::Ready {
        events: plan_events(&value, relais_binary),
    }
}

fn plan_events(value: &Value, relais_binary: &Path) -> Vec<HookEventPlan> {
    let command = hook_command(relais_binary);
    HOOK_TARGETS
        .iter()
        .map(|(event, matcher)| HookEventPlan {
            event,
            action: event_action(value, event, *matcher, &command),
        })
        .collect()
}

/// The entries already on one event's array, or none if the key, the
/// `hooks` object, or the event's own array is absent.
fn entries<'a>(value: &'a Value, event: &str) -> &'a [Value] {
    value
        .pointer(&format!("/hooks/{event}"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// The `matcher` an entry carries, `None` for an absent, null or
/// non-string matcher — the same thing Claude Code itself treats as "no
/// matcher, run for every tool".
fn entry_matcher(entry: &Value) -> Option<&str> {
    entry.get("matcher").and_then(Value::as_str)
}

/// Whether an entry's `hooks` array already carries a command hook with
/// exactly this command string.
fn entry_has_command(entry: &Value, command: &str) -> bool {
    entry
        .get("hooks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|hook| {
            hook.get("type").and_then(Value::as_str) == Some("command")
                && hook.get("command").and_then(Value::as_str) == Some(command)
        })
}

/// What one event needs. An entry is relais's to join when its matcher
/// is the same string relais installs — see the module doc on why a
/// different matcher is not widened but gets its own entry.
fn event_action(
    value: &Value,
    event: &str,
    matcher: Option<&str>,
    command: &str,
) -> HookEventAction {
    let entries = entries(value, event);
    if entries
        .iter()
        .any(|entry| entry_has_command(entry, command))
    {
        return HookEventAction::Current;
    }
    if entries.iter().any(|entry| entry_matcher(entry) == matcher) {
        HookEventAction::JoinExisting
    } else {
        HookEventAction::NewEntry
    }
}

/// Apply the wiring in place: join relais's command into a matching
/// entry, or append a fresh entry, for every event that is not already
/// current. Returns the events actually changed, in target order.
///
/// Never called on a document that has not just been confirmed to round
/// trip — the caller re-checks that at write time, the same way the file
/// install re-checks its own markers between plan and apply (C9).
pub fn apply_hooks(value: &mut Value, relais_binary: &Path) -> Vec<&'static str> {
    let command = hook_command(relais_binary);
    let mut changed = Vec::new();
    for (event, matcher) in HOOK_TARGETS {
        let array = value
            .as_object_mut()
            .expect("a JSON document is always an object at its root here")
            .entry("hooks")
            .or_insert_with(|| serde_json::json!({}))
            .as_object_mut()
            .expect("`hooks` is always an object")
            .entry(event)
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .expect("an event's hooks are always an array");
        if array.iter().any(|entry| entry_has_command(entry, &command)) {
            continue;
        }
        let joined = array
            .iter_mut()
            .find(|entry| entry_matcher(entry) == matcher);
        let hook_object = serde_json::json!({"type": "command", "command": command});
        match joined {
            Some(entry) => {
                entry
                    .get_mut("hooks")
                    .and_then(Value::as_array_mut)
                    .expect("a matching entry always carries a `hooks` array")
                    .push(hook_object);
            }
            None => {
                let mut new_entry = serde_json::json!({"hooks": [hook_object]});
                if let Some(matcher) = matcher {
                    new_entry["matcher"] = Value::String(matcher.to_string());
                }
                array.push(new_entry);
            }
        }
        changed.push(event);
    }
    changed
}

/// Remove exactly what [`apply_hooks`] would have added: the leaf hook
/// object whose command is relais's own, wherever it sits. Entry objects
/// and event arrays are never deleted, whatever they are left holding —
/// an entry relais did not create is not its call to remove, and it
/// cannot tell an entry it emptied apart from one that was already empty
/// before it ever touched the file. Returns the events actually changed.
pub fn remove_hooks(value: &mut Value, relais_binary: &Path) -> Vec<&'static str> {
    let command = hook_command(relais_binary);
    let mut changed = Vec::new();
    for (event, _matcher) in HOOK_TARGETS {
        let Some(array) = value
            .pointer_mut(&format!("/hooks/{event}"))
            .and_then(Value::as_array_mut)
        else {
            continue;
        };
        let mut touched = false;
        for entry in array.iter_mut() {
            let Some(hooks) = entry.get_mut("hooks").and_then(Value::as_array_mut) else {
                continue;
            };
            let before = hooks.len();
            hooks.retain(|hook| {
                !(hook.get("type").and_then(Value::as_str) == Some("command")
                    && hook.get("command").and_then(Value::as_str) == Some(command.as_str()))
            });
            if hooks.len() != before {
                touched = true;
            }
        }
        if touched {
            changed.push(event);
        }
    }
    changed
}

/// What one target's removal plan says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookRemovalAction {
    /// relais's command is on this event; it would be removed.
    WouldRemove,
    /// relais's command is not on this event; nothing to do.
    NotPresent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookRemovalPlan {
    pub event: &'static str,
    pub action: HookRemovalAction,
}

/// What planning `relais uninstall --claude --hooks` found. Shares
/// [`HooksPlan::Unrenderable`]'s reasoning: removing the leaf command
/// still rewrites the file, so a file that cannot be re-rendered
/// byte-for-byte is refused exactly the same way an install would be.
#[derive(Debug, Clone, PartialEq)]
pub enum HooksRemovalPlan {
    Ready { events: Vec<HookRemovalPlan> },
    Unrenderable { reason: String },
}

impl HooksRemovalPlan {
    pub fn applicable_count(&self) -> usize {
        match self {
            HooksRemovalPlan::Ready { events } => events
                .iter()
                .filter(|plan| plan.action == HookRemovalAction::WouldRemove)
                .count(),
            HooksRemovalPlan::Unrenderable { .. } => 0,
        }
    }
}

/// Plan removing relais's own hook commands from a settings document.
/// An absent file has nothing to remove — it is `Ready` with every
/// target `NotPresent`, never treated as an error.
pub fn plan_removal(existing_text: Option<&str>, relais_binary: &Path) -> HooksRemovalPlan {
    let value: Value = match existing_text {
        None => serde_json::json!({}),
        Some(text) => match serde_json::from_str(text) {
            Ok(value) => value,
            Err(e) => {
                return HooksRemovalPlan::Unrenderable {
                    reason: format!("settings.json is not valid JSON: {e}"),
                }
            }
        },
    };
    if let Some(text) = existing_text {
        if !round_trips(text, &value) {
            return HooksRemovalPlan::Unrenderable {
                reason: "settings.json cannot be re-rendered byte for byte, so removing \
                         relais's hook here would bury the change in reformatting nobody \
                         asked for; remove the relais command by hand instead"
                    .to_string(),
            };
        }
    }
    let command = hook_command(relais_binary);
    let events = HOOK_TARGETS
        .iter()
        .map(|(event, _)| HookRemovalPlan {
            event,
            action: if entries(&value, event)
                .iter()
                .any(|entry| entry_has_command(entry, &command))
            {
                HookRemovalAction::WouldRemove
            } else {
                HookRemovalAction::NotPresent
            },
        })
        .collect();
    HooksRemovalPlan::Ready { events }
}

/// The set of event names [`HOOK_TARGETS`] installs must never drift
/// from the set [`crate::hook::TARGETS`] probes: they are read out of
/// two different places for two different reasons (SPEC criteria), and
/// nothing compiles that check for a reader — only a test does.
pub fn target_names() -> Vec<&'static str> {
    HOOK_TARGETS.iter().map(|(event, _)| *event).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hook::TARGETS;

    fn binary() -> &'static Path {
        Path::new("/opt/relais/bin/relais")
    }

    #[test]
    fn the_installed_target_set_matches_the_probes_target_set() {
        let mut installed = target_names();
        installed.sort();
        let mut probed: Vec<&str> = TARGETS.to_vec();
        probed.sort();
        assert_eq!(
            installed, probed,
            "install and the compatibility record must name the same seven targets"
        );
    }

    #[test]
    fn tool_scoped_events_are_matched_to_the_agent_tool_and_its_legacy_name() {
        for event in ["PreToolUse", "PostToolUse", "PostToolUseFailure"] {
            let (_, matcher) = HOOK_TARGETS.iter().find(|(e, _)| *e == event).unwrap();
            assert_eq!(*matcher, Some("Agent|Task"), "{event}");
        }
        for event in [
            "SubagentStart",
            "SubagentStop",
            "SessionStart",
            "SessionEnd",
        ] {
            let (_, matcher) = HOOK_TARGETS.iter().find(|(e, _)| *e == event).unwrap();
            assert_eq!(*matcher, None, "{event} must install with no matcher");
        }
    }

    #[test]
    fn planning_an_absent_file_wants_all_seven_as_new_entries() {
        let plan = plan_hooks(None, binary());
        let HooksPlan::Ready { events } = plan else {
            panic!("an absent file is always renderable");
        };
        assert_eq!(events.len(), 7);
        assert!(events.iter().all(|e| e.action == HookEventAction::NewEntry));
    }

    #[test]
    fn applying_then_planning_again_finds_nothing_left_to_do() {
        let mut value = serde_json::json!({});
        let changed = apply_hooks(&mut value, binary());
        assert_eq!(changed.len(), 7, "{changed:?}");
        let rendered = render_canonical(&value);
        let plan = plan_hooks(Some(&rendered), binary());
        let HooksPlan::Ready { events } = plan else {
            panic!("relais's own canonical rendering always round-trips: {rendered}");
        };
        assert!(
            events.iter().all(|e| e.action == HookEventAction::Current),
            "a re-run over what relais itself wrote must be a no-op: {events:?}"
        );
    }

    #[test]
    fn a_foreign_hook_on_the_same_event_is_joined_not_duplicated_beside() {
        let mut value = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {"matcher": "Agent|Task", "hooks": [{"type": "command", "command": "/usr/bin/someone-elses-tool"}]}
                ]
            }
        });
        let plan = plan_events(&value, binary());
        let pre = plan.iter().find(|e| e.event == "PreToolUse").unwrap();
        assert_eq!(pre.action, HookEventAction::JoinExisting);

        apply_hooks(&mut value, binary());
        let entries = value["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(
            entries.len(),
            1,
            "joined into the existing block, not a second one beside it: {entries:?}"
        );
        let hooks = entries[0]["hooks"].as_array().unwrap();
        assert_eq!(hooks.len(), 2, "{hooks:?}");
        assert!(hooks
            .iter()
            .any(|h| h["command"] == "/usr/bin/someone-elses-tool"));
        assert!(hooks.iter().any(|h| h["command"] == hook_command(binary())));
    }

    #[test]
    fn a_different_matcher_on_the_same_event_gets_its_own_entry() {
        let mut value = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {"matcher": "Bash", "hooks": [{"type": "command", "command": "/usr/bin/lint-bash"}]}
                ]
            }
        });
        apply_hooks(&mut value, binary());
        let entries = value["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(
            entries.len(),
            2,
            "a different matcher is not relais's to join: {entries:?}"
        );
    }

    #[test]
    fn uninstall_removes_only_the_leaf_command_never_the_entry() {
        let mut value = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {"matcher": "Agent|Task", "hooks": [
                        {"type": "command", "command": "/usr/bin/someone-elses-tool"},
                        {"type": "command", "command": hook_command(binary())}
                    ]}
                ]
            }
        });
        let changed = remove_hooks(&mut value, binary());
        assert_eq!(changed, vec!["PreToolUse"]);
        let entries = value["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(entries.len(), 1, "the foreign entry object survives");
        let hooks = entries[0]["hooks"].as_array().unwrap();
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0]["command"], "/usr/bin/someone-elses-tool");
    }

    #[test]
    fn uninstall_never_deletes_an_entry_it_leaves_empty() {
        // A foreign entry whose hooks array was already empty before
        // relais joined it: after un-joining, relais's own command is
        // gone but the entry — empty exactly as its author left it — is
        // not swept away, because relais cannot tell "I emptied this"
        // from "this was always empty" apart, and only the first would
        // ever be its call.
        let mut value = serde_json::json!({
            "hooks": {
                "SessionStart": [
                    {"hooks": [{"type": "command", "command": hook_command(binary())}]}
                ]
            }
        });
        remove_hooks(&mut value, binary());
        let entries = value["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(
            entries.len(),
            1,
            "the entry object is left in place: {entries:?}"
        );
        assert!(entries[0]["hooks"].as_array().unwrap().is_empty());
    }

    #[test]
    fn a_hand_formatted_file_is_refused_rather_than_rewritten() {
        let hand_written = "{\n  \"hooks\": {\n    \"PreToolUse\":[]\n  }\n}\n";
        let plan = plan_hooks(Some(hand_written), binary());
        let HooksPlan::Unrenderable {
            reason,
            paste_block,
        } = plan
        else {
            panic!("mismatched spacing must be refused: {plan:?}");
        };
        assert!(reason.contains("re-rendered"), "{reason}");
        assert!(paste_block.contains("PreToolUse"), "{paste_block}");
    }

    /// The one tolerated difference and its boundary. A file that is
    /// relais's canonical rendering plus the newline nearly every editor
    /// adds is accepted, and is written back WITH that newline; the same
    /// rendering with no newline is also accepted, and written back
    /// without one. Anything else in that position — a blank line, a
    /// `\r\n` tail — is refused, because relais would give the file a
    /// tail it did not have, and this module promises it changes nothing
    /// it was not asked to.
    #[test]
    fn the_trailing_newline_is_the_only_tolerated_difference_and_is_preserved() {
        let mut value = serde_json::json!({});
        apply_hooks(&mut value, binary());
        let canonical = render_canonical(&value);

        let with_newline = format!("{canonical}\n");
        assert!(
            matches!(
                plan_hooks(Some(&with_newline), binary()),
                HooksPlan::Ready { .. }
            ),
            "a trailing newline is accepted"
        );
        assert_eq!(
            render_like(Some(&with_newline), &value),
            with_newline,
            "and written back with it"
        );

        assert!(
            matches!(
                plan_hooks(Some(&canonical), binary()),
                HooksPlan::Ready { .. }
            ),
            "no trailing newline is accepted too"
        );
        assert_eq!(
            render_like(Some(&canonical), &value),
            canonical,
            "and written back without one: relais does not add a tail it did not find"
        );

        for (label, text) in [
            ("a trailing blank line", format!("{canonical}\n\n")),
            ("a CRLF tail", format!("{canonical}\r\n")),
            ("trailing spaces", format!("{canonical}\n  ")),
        ] {
            assert!(
                matches!(
                    plan_hooks(Some(&text), binary()),
                    HooksPlan::Unrenderable { .. }
                ),
                "{label} must be refused: relais cannot write that tail back"
            );
        }

        // An absent file is created the way every other text file here
        // is: with a trailing newline.
        assert_eq!(render_like(None, &value), format!("{canonical}\n"));
    }

    #[test]
    fn invalid_json_is_refused_with_a_reason_naming_the_parse_error() {
        let plan = plan_hooks(Some("not json {{{"), binary());
        let HooksPlan::Unrenderable { reason, .. } = plan else {
            panic!("invalid JSON must be refused");
        };
        assert!(reason.contains("not valid JSON"), "{reason}");
    }

    #[test]
    fn the_canonical_renderer_sorts_keys_because_preserve_order_is_off() {
        let value = serde_json::json!({"z": 1, "a": 2});
        let rendered = render_canonical(&value);
        assert!(
            rendered.find("\"a\"").unwrap() < rendered.find("\"z\"").unwrap(),
            "keys must come out sorted, not in insertion order: {rendered}"
        );
    }

    #[test]
    fn the_command_is_always_the_binarys_absolute_path_never_a_bare_name() {
        assert_eq!(
            hook_command(Path::new("/opt/relais/bin/relais")),
            "/opt/relais/bin/relais hook"
        );
    }
}
