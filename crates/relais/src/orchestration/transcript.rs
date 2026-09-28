//! Parsing Claude Code transcript lines into typed usage records.
//!
//! A transcript is NDJSON this binary does not write (Claude Code's own
//! session log, `~/.claude/projects/<slug>/<session>.jsonl`). Usage lives
//! on lines with `type == "assistant"`, under `message.usage`. One API
//! message is split across several transcript lines that repeat the same
//! `message.id` (streamed snapshots of the same response). Their input and
//! cache figures agree, but `output_tokens` GROWS from line to line: in
//! one 6,334-message session 1,105 of 2,677 repeated ids differed (8 →
//! 316, 8 → 394), and keeping the first line undercounted output by 1.25M
//! tokens. So records are deduplicated by `message.id` keeping the line
//! with the most output — the final snapshot — once per file: a caller combining several files (the main transcript and its
//! subagents') relies on the ledger's own `message_id` uniqueness for
//! cross-file dedup, since two different conversations do not share ids.

use std::collections::HashMap;

use serde::Deserialize;

/// A model of `<synthetic>` carries no billable usage — Claude Code
/// emits it for lines that never reached the API — and is skipped
/// outright rather than priced as an unknown model.
const SYNTHETIC_MODEL: &str = "<synthetic>";

/// `usage.speed`: `"standard"`, or fast mode (`"fast"`), which bills at
/// a different rate. Not `usage.service_tier` — that is a separate field
/// (standard, priority, batch) and says nothing about fast mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Speed {
    Standard,
    Other(String),
}

impl Speed {
    pub fn as_str(&self) -> &str {
        match self {
            Speed::Standard => "standard",
            Speed::Other(tier) => tier,
        }
    }

    pub fn from_stored(stored: &str) -> Self {
        if stored == "standard" {
            Speed::Standard
        } else {
            Speed::Other(stored.to_string())
        }
    }
}

/// Cache writes split by TTL (SPEC §11): a 5-minute write and a 1-hour
/// write cost different multiples of base input, so summing them into
/// one `cache_write` figure would misprice whichever TTL is not the
/// 1.25x a single column implies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CacheWrites {
    pub ephemeral_5m_input_tokens: u64,
    pub ephemeral_1h_input_tokens: u64,
}

/// Which transcript a record came from: the orchestrating session's main
/// file, or one of its subagents'. Subagent turns are never sidechain
/// lines inside the main file — they live in their own
/// `<slug>/<session>/subagents/agent-*.jsonl` file — so this is always
/// which FILE was read, not a position inside one file.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TranscriptSource {
    Main,
    Subagent(String),
}

impl TranscriptSource {
    /// What the ledger stores in the `transcript` column: `"main"`, or
    /// the subagent file's own name.
    pub fn label(&self) -> String {
        match self {
            TranscriptSource::Main => "main".to_string(),
            TranscriptSource::Subagent(file_name) => file_name.clone(),
        }
    }

    pub fn from_stored(stored: &str) -> Self {
        if stored == "main" {
            TranscriptSource::Main
        } else {
            TranscriptSource::Subagent(stored.to_string())
        }
    }
}

/// One API message's usage, already deduplicated by `message.id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRecord {
    pub message_id: String,
    pub model: String,
    pub speed: Speed,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_writes: CacheWrites,
    /// The message's own timestamp (RFC 3339, as Claude Code wrote it):
    /// import attributes spend to the report window this fell in, not
    /// to the run's `created_at`.
    pub timestamp: String,
}

#[derive(Deserialize)]
struct RawLine {
    #[serde(rename = "type")]
    line_type: String,
    message: Option<RawMessage>,
    timestamp: Option<String>,
}

#[derive(Deserialize)]
struct RawMessage {
    id: Option<String>,
    model: Option<String>,
    usage: Option<RawUsage>,
}

#[derive(Deserialize)]
struct RawUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    cache_creation: Option<RawCacheCreation>,
    #[serde(default)]
    speed: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawCacheCreation {
    #[serde(default)]
    ephemeral_5m_input_tokens: u64,
    #[serde(default)]
    ephemeral_1h_input_tokens: u64,
}

/// Parse one transcript file's content into its deduplicated usage
/// records.
///
/// A line this cannot make sense of — malformed JSON, a `type` other
/// than `assistant`, no `message.usage`, no `message.id`, no top-level
/// `timestamp` — is skipped, not an error: a transcript is host-owned
/// NDJSON, and one line the parser does not recognise is absent usage,
/// not a reason to fail the whole file. A `<synthetic>` model is skipped
/// the same way — it never reached the API, so it is not "usage of an
/// unpriced model", it is not usage at all.
pub fn parse_transcript(content: &str) -> Vec<UsageRecord> {
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut records: Vec<UsageRecord> = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(raw) = serde_json::from_str::<RawLine>(line) else {
            continue;
        };
        if raw.line_type != "assistant" {
            continue;
        }
        let Some(message) = raw.message else {
            continue;
        };
        let Some(model) = message.model else {
            continue;
        };
        if model == SYNTHETIC_MODEL {
            continue;
        }
        let Some(message_id) = message.id else {
            continue;
        };
        let Some(usage) = message.usage else {
            continue;
        };
        let Some(timestamp) = raw.timestamp else {
            continue;
        };
        let cache_creation = usage.cache_creation.unwrap_or_default();
        let record = UsageRecord {
            message_id,
            model,
            speed: match usage.speed {
                Some(tier) if tier != "standard" => Speed::Other(tier),
                _ => Speed::Standard,
            },
            input_tokens: usage.input_tokens.unwrap_or(0),
            output_tokens: usage.output_tokens.unwrap_or(0),
            cache_read_input_tokens: usage.cache_read_input_tokens.unwrap_or(0),
            cache_writes: CacheWrites {
                ephemeral_5m_input_tokens: cache_creation.ephemeral_5m_input_tokens,
                ephemeral_1h_input_tokens: cache_creation.ephemeral_1h_input_tokens,
            },
            timestamp,
        };
        match index.get(&record.message_id) {
            Some(&at) if records[at].output_tokens >= record.output_tokens => {}
            Some(&at) => records[at] = record,
            None => {
                index.insert(record.message_id.clone(), records.len());
                records.push(record);
            }
        }
    }
    records
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(id: &str, model: &str, extra: &str) -> String {
        line_with_cache(id, model, 0, 0, extra)
    }

    fn line_with_cache(id: &str, model: &str, cache_5m: u64, cache_1h: u64, extra: &str) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"2026-09-28T00:00:00Z","message":{{"id":"{id}","model":"{model}","usage":{{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":0,"cache_creation":{{"ephemeral_5m_input_tokens":{cache_5m},"ephemeral_1h_input_tokens":{cache_1h}}}{extra}}}}}}}"#
        )
    }

    #[test]
    fn repeated_identical_lines_count_once() {
        let one = line("msg_1", "claude-haiku-4-5", "");
        let content = format!("{one}\n{one}\n{one}\n");
        let records = parse_transcript(&content);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].message_id, "msg_1");
    }

    #[test]
    fn synthetic_model_is_skipped_not_priced_as_unknown() {
        let content = line("msg_1", "<synthetic>", "");
        assert!(parse_transcript(&content).is_empty());
    }

    #[test]
    fn non_assistant_lines_are_skipped() {
        let content = r#"{"type":"user","timestamp":"2026-09-28T00:00:00Z"}"#;
        assert!(parse_transcript(content).is_empty());
    }

    #[test]
    fn cache_writes_are_split_by_ttl() {
        let content = line_with_cache("msg_1", "claude-opus-5-5", 0, 44_700_000, "");
        let records = parse_transcript(&content);
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].cache_writes.ephemeral_1h_input_tokens,
            44_700_000
        );
        assert_eq!(records[0].cache_writes.ephemeral_5m_input_tokens, 0);
    }

    /// Fast mode is `usage.speed`. `usage.service_tier` is a different
    /// field (every real line carries `"service_tier":"standard"` beside
    /// `"speed":"standard"`) and must not be read as a speed.
    #[test]
    fn fast_speed_is_recorded_and_service_tier_is_not_a_speed() {
        let content = line("msg_1", "claude-opus-5", r#","speed":"fast""#);
        let records = parse_transcript(&content);
        assert_eq!(records[0].speed, Speed::Other("fast".to_string()));
        let content = line(
            "msg_2",
            "claude-opus-5",
            r#","service_tier":"priority","speed":"standard""#,
        );
        let records = parse_transcript(&content);
        assert_eq!(records[0].speed, Speed::Standard);
    }

    /// Streamed snapshots of one message repeat its id with GROWING
    /// `output_tokens` (measured: 8, 8, 316 on one real message). The
    /// final snapshot is the one billed. FALSIFIED: keeping the first
    /// line instead read 8 here, and the real 6,334-message session came
    /// out 1.25M output tokens short; restored.
    #[test]
    fn repeated_message_id_keeps_the_final_snapshot() {
        let snapshot = |output: u64| {
            format!(
                r#"{{"type":"assistant","timestamp":"2026-09-28T00:00:00Z","message":{{"id":"msg_1","model":"claude-opus-5","usage":{{"input_tokens":10,"output_tokens":{output},"cache_read_input_tokens":16436}}}}}}"#
            )
        };
        let content = format!("{}\n{}\n{}\n", snapshot(8), snapshot(8), snapshot(316));
        let records = parse_transcript(&content);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].output_tokens, 316);
        assert_eq!(records[0].cache_read_input_tokens, 16436);
    }

    #[test]
    fn standard_speed_is_the_default() {
        let content = line("msg_1", "claude-haiku-4-5", "");
        let records = parse_transcript(&content);
        assert_eq!(records[0].speed, Speed::Standard);
    }

    #[test]
    fn a_malformed_line_is_skipped_not_an_error() {
        let content = "not json at all\n";
        assert!(parse_transcript(content).is_empty());
    }
}
