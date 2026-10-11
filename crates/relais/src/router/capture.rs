//! Classifier inputs kept for training (relais ADR-0002, learning-loop
//! plan Phase 5). The text of each classifier call goes to a local day file
//! (`<state dir>/classifier/<YYYY-MM-DD>.jsonl`, directory 0700, file 0600),
//! with the secret shapes redacted first; the ledger holds only the text's
//! hash. Day files older than [`KEEP_DAYS`] days are deleted. The functions
//! that redact and name files are pure; [`write_inputs`] and
//! [`purge_expired`] are the effects.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

/// Day files older than this many days are deleted.
pub const KEEP_DAYS: i64 = 90;

/// The classifier input the contract allows: at most this many bytes.
pub const INPUT_MAX_BYTES: usize = 3072;

/// Whether `router-observe` keeps classifier inputs, and where.
#[derive(Debug, Clone, PartialEq)]
pub enum CapturePolicy {
    /// Nothing is written: no day file, no ledger row.
    Off,
    On {
        dir: PathBuf,
        now: DateTime<Utc>,
    },
}

/// One classifier call's text, as `router-observe` received it.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassifierInput {
    pub session: String,
    pub call_id: String,
    pub at: String,
    pub input: String,
    /// The person's words the classifier quoted as accept or correct.
    pub quote: Option<String>,
}

/// One line of a day file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DayLine {
    pub session: String,
    pub call_id: String,
    pub at: String,
    pub input: String,
    #[serde(default)]
    pub quote: Option<String>,
    /// How many secret shapes were replaced in `input` and `quote`.
    #[serde(default)]
    pub redactions: usize,
}

/// The day file's name for a date.
pub fn day_file_name(day: NaiveDate) -> String {
    format!("{}.jsonl", day.format("%Y-%m-%d"))
}

/// The date a day file's name carries, when it is one.
pub fn parse_day_file_name(name: &str) -> Option<NaiveDate> {
    let stem = name.strip_suffix(".jsonl")?;
    NaiveDate::parse_from_str(stem, "%Y-%m-%d").ok()
}

/// The line a call is written as: its text redacted.
pub fn day_line(input: &ClassifierInput) -> DayLine {
    let (text, in_text) = redact(&input.input);
    let (quote, in_quote) = match &input.quote {
        Some(quote) => {
            let (quote, n) = redact(quote);
            (Some(quote), n)
        }
        None => (None, 0),
    };
    DayLine {
        session: input.session.clone(),
        call_id: input.call_id.clone(),
        at: input.at.clone(),
        input: text,
        quote,
        redactions: in_text + in_quote,
    }
}

// --- redaction --------------------------------------------------------------

const KEY_PREFIXES: [&str; 17] = [
    "sk-",
    "sk_",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "glpat-",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "xoxr-",
    "xoxs-",
    "xapp-",
    "AKIA",
    "AIza",
];

/// What must follow a key prefix before the token counts as a key.
const KEY_BODY_MIN: usize = 8;
const RUN_MIN: usize = 40;

/// The text with the known secret shapes replaced, and how many were:
/// private-key blocks, bearer tokens, provider API keys (`sk-`, `ghp_`,
/// `xox…-` and kin) and base64 or hex runs of 40 characters or more.
pub fn redact(text: &str) -> (String, usize) {
    let (text, keys) = redact_private_keys(text);
    let (text, bearers) = redact_bearers(&text);
    let (text, providers) = replace_tokens(&text, is_key_char, provider_key);
    let (text, runs) = replace_tokens(&text, is_run_char, long_run);
    (text, keys + bearers + providers + runs)
}

fn redact_private_keys(text: &str) -> (String, usize) {
    const BEGIN: &str = "-----BEGIN ";
    const END_TAG: &str = "PRIVATE KEY-----";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut count = 0;
    while let Some(start) = rest.find(BEGIN) {
        let after = &rest[start + BEGIN.len()..];
        // The header line must name a private key: `BEGIN CERTIFICATE`
        // is public and stays.
        let header_end = after.find("-----").map(|i| i + 5);
        let is_private = header_end.is_some_and(|end| after[..end].ends_with(END_TAG));
        if !is_private {
            out.push_str(&rest[..start + BEGIN.len()]);
            rest = after;
            continue;
        }
        out.push_str(&rest[..start]);
        out.push_str("[REDACTED:private-key]");
        count += 1;
        let body = &after[header_end.unwrap_or(0)..];
        rest = match body.find("-----END ") {
            Some(end_at) => {
                let tail = &body[end_at + "-----END ".len()..];
                match tail.find("-----") {
                    Some(close) => &tail[close + 5..],
                    None => "",
                }
            }
            // An unterminated block (the input was clipped): the rest of
            // the text is the key.
            None => "",
        };
    }
    out.push_str(rest);
    (out, count)
}

fn redact_bearers(text: &str) -> (String, usize) {
    let chars: Vec<char> = text.chars().collect();
    let word: Vec<char> = "bearer".chars().collect();
    let is_token = |c: char| c.is_ascii_alphanumeric() || "._~+/=-".contains(c);
    let mut out = String::with_capacity(text.len());
    let mut count = 0;
    let mut i = 0;
    while i < chars.len() {
        let starts_word = i == 0 || !chars[i - 1].is_ascii_alphanumeric();
        let matches = starts_word
            && chars.len() >= i + word.len()
            && chars[i..i + word.len()]
                .iter()
                .zip(&word)
                .all(|(a, b)| a.eq_ignore_ascii_case(b));
        if matches {
            let mut j = i + word.len();
            let spaces_from = j;
            while j < chars.len() && (chars[j] == ' ' || chars[j] == '\t') {
                j += 1;
            }
            let token_from = j;
            while j < chars.len() && is_token(chars[j]) {
                j += 1;
            }
            if j > spaces_from && token_from > spaces_from && j - token_from >= KEY_BODY_MIN {
                out.extend(&chars[i..token_from]);
                out.push_str("[REDACTED:bearer]");
                count += 1;
                i = j;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    (out, count)
}

fn is_key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

fn is_run_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '='
}

fn provider_key(token: &str) -> Option<&'static str> {
    KEY_PREFIXES
        .iter()
        .any(|prefix| {
            token.len() >= prefix.len() + KEY_BODY_MIN
                && token.starts_with(prefix)
                // `AKIA` and `AIza` are mixed case by design; the dashed
                // and underscored prefixes are lowercase.
                && token[prefix.len()..]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
        .then_some("[REDACTED:api-key]")
}

/// A run of 40 or more base64 (or hex) characters. A path or a long word
/// is not one: the run needs a digit and a letter, and when it has a `/`
/// it must also mix cases, as base64 does and a path does not.
fn long_run(token: &str) -> Option<&'static str> {
    let body = token.trim_end_matches('=');
    if body.len() < RUN_MIN {
        return None;
    }
    let has_digit = body.chars().any(|c| c.is_ascii_digit());
    let has_letter = body.chars().any(|c| c.is_ascii_alphabetic());
    if !(has_digit && has_letter) {
        return None;
    }
    let is_hex = body.chars().all(|c| c.is_ascii_hexdigit());
    let mixed_case = body.chars().any(|c| c.is_ascii_uppercase())
        && body.chars().any(|c| c.is_ascii_lowercase());
    if is_hex || mixed_case || !body.contains('/') && body.contains('+') {
        Some("[REDACTED:long-run]")
    } else {
        None
    }
}

/// Every maximal run of `in_token` characters that `decide` replaces, and
/// how many it replaced.
fn replace_tokens(
    text: &str,
    in_token: fn(char) -> bool,
    decide: fn(&str) -> Option<&'static str>,
) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut count = 0;
    let mut token_start: Option<usize> = None;
    let flush = |out: &mut String, token: &str, count: &mut usize| match decide(token) {
        Some(marker) => {
            out.push_str(marker);
            *count += 1;
        }
        None => out.push_str(token),
    };
    for (index, c) in text.char_indices() {
        if in_token(c) {
            token_start.get_or_insert(index);
        } else {
            if let Some(start) = token_start.take() {
                flush(&mut out, &text[start..index], &mut count);
            }
            out.push(c);
        }
    }
    if let Some(start) = token_start {
        flush(&mut out, &text[start..], &mut count);
    }
    (out, count)
}

// --- effects ----------------------------------------------------------------

/// Append the calls to today's day file (the directory forced to 0700, the
/// file 0600, each line one `write_all`), then delete the files past
/// [`KEEP_DAYS`]. Returns how many files were deleted.
pub fn write_inputs(
    dir: &Path,
    now: DateTime<Utc>,
    inputs: &[ClassifierInput],
) -> std::io::Result<usize> {
    if inputs.is_empty() {
        return Ok(0);
    }
    crate::fsutil::ensure_private_dir(dir)?;
    let path = dir.join(day_file_name(now.date_naive()));
    for input in inputs {
        let line = serde_json::to_string(&day_line(input)).expect("a day line serializes");
        crate::fsutil::append_private_line(&path, &line)?;
    }
    purge_expired(dir, now)
}

/// Delete the day files more than [`KEEP_DAYS`] days older than `now`. A
/// directory that does not exist has nothing to delete. Other files in it
/// are left alone.
pub fn purge_expired(dir: &Path, now: DateTime<Utc>) -> std::io::Result<usize> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let cutoff = now.date_naive() - Duration::days(KEEP_DAYS);
    let mut deleted = 0;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if parse_day_file_name(&name).is_some_and(|day| day < cutoff) {
            std::fs::remove_file(entry.path())?;
            deleted += 1;
        }
    }
    Ok(deleted)
}

/// Every day line on disk, oldest file first; a line that does not parse
/// is skipped (counted in the second value).
pub fn read_lines(dir: &Path) -> std::io::Result<(Vec<DayLine>, usize)> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), 0)),
        Err(e) => return Err(e),
    };
    let mut files: Vec<(NaiveDate, PathBuf)> = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(day) = parse_day_file_name(&name) {
            files.push((day, entry.path()));
        }
    }
    files.sort();
    let mut lines = Vec::new();
    let mut skipped = 0;
    for (_, path) in files {
        for line in std::fs::read_to_string(&path)?.lines() {
            match serde_json::from_str::<DayLine>(line) {
                Ok(parsed) => lines.push(parsed),
                Err(_) if line.trim().is_empty() => {}
                Err(_) => skipped += 1,
            }
        }
    }
    Ok((lines, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn provider_keys_bearer_tokens_and_private_keys_are_redacted_and_counted() {
        // Assembled at run time: a secret-shaped literal in the source
        // would itself trip the repository's secret scan.
        let key = |prefix: &str, body: &str| format!("{prefix}{body}");
        let block = |edge: &str| format!("-----{edge} OPENSSH PRIVATE {}-----", "KEY");
        let text = format!(
            "use {} and {}, {}; Authorization: Bearer abc.def-ghi_jkl123\n{}\nb3BlbnNzaA\n{}\nbye",
            key("sk-ant-", "api03-abcdefghijklmnop"),
            key("ghp_", "0123456789abcdefghij"),
            key("xoxb-", "1234-5678-abcdef"),
            block("BEGIN"),
            block("END"),
        );
        let (out, n) = redact(&text);
        assert_eq!(n, 5, "{out}");
        for secret in [
            "sk-ant-api03",
            "ghp_0123",
            "xoxb-1234",
            "abc.def-ghi",
            "b3BlbnNzaA",
        ] {
            assert!(!out.contains(secret), "{secret} survived: {out}");
        }
        assert!(out.starts_with("use [REDACTED:api-key] and [REDACTED:api-key],"));
        assert!(out.ends_with("[REDACTED:private-key]\nbye"), "{out}");
        assert!(out.contains("Bearer [REDACTED:bearer]"), "{out}");
    }

    #[test]
    fn long_hex_and_base64_runs_are_redacted_but_ordinary_text_is_not() {
        let hex = "a".repeat(20) + &"1".repeat(20);
        let b64 = "dGhpcyBpcyBhIHNlY3JldCB0b2tlbiB3aXRoIG1peGVkIGNhc2U=";
        let (out, n) = redact(&format!("sha {hex} and {b64} end"));
        assert_eq!(n, 2, "{out}");
        assert_eq!(out, "sha [REDACTED:long-run] and [REDACTED:long-run] end");
        // 39 hex characters are a word, not a run.
        let short = "1".repeat(39);
        assert_eq!(redact(&short), (short.clone(), 0));
        // A long path is not base64.
        let path = "crates/relais/src/router/capture/very/long/path/to/some/file.rs";
        assert_eq!(redact(path), (path.to_string(), 0));
        let prose = "rename foo to bar in the task-1234 helper; the disk-cache is fine";
        assert_eq!(redact(prose), (prose.to_string(), 0));
        // A public certificate is not a private key.
        let cert = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----";
        assert_eq!(redact(cert), (cert.to_string(), 0));
    }

    #[test]
    fn an_unterminated_private_key_block_takes_the_rest_of_the_text() {
        let text = format!("before -----BEGIN PRIVATE {}-----\nMIIEvQIBADAN", "KEY");
        let (out, n) = redact(&text);
        assert_eq!((out.as_str(), n), ("before [REDACTED:private-key]", 1));
    }

    #[test]
    fn day_files_are_named_by_date_and_nothing_else_is_one() {
        let day = NaiveDate::from_ymd_opt(2026, 10, 11).unwrap();
        assert_eq!(day_file_name(day), "2026-10-11.jsonl");
        assert_eq!(parse_day_file_name("2026-10-11.jsonl"), Some(day));
        assert_eq!(parse_day_file_name("notes.jsonl"), None);
        assert_eq!(parse_day_file_name("2026-10-11.json"), None);
    }

    #[test]
    fn files_past_ninety_days_are_deleted_and_the_rest_kept() {
        let dir = crate::test_support::short_temp_dir("cap-ttl");
        let dir = dir.to_path_buf();
        let now = at("2026-10-11T12:00:00Z");
        for name in [
            "2026-07-13.jsonl", // 90 days: kept
            "2026-07-12.jsonl", // 91 days: deleted
            "2026-01-01.jsonl",
            "2026-10-11.jsonl",
            "README",
        ] {
            std::fs::write(dir.join(name), "{}\n").unwrap();
        }
        assert_eq!(purge_expired(&dir, now).unwrap(), 2);
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["2026-07-13.jsonl", "2026-10-11.jsonl", "README"]);
        assert_eq!(purge_expired(&dir.join("absent"), now).unwrap(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn an_existing_directory_is_forced_to_0700_and_the_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = crate::test_support::short_temp_dir("cap-mode");
        let dir = scratch.to_path_buf().join("classifier");
        // The directory exists already, world readable.
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let now = at("2026-10-11T12:00:00Z");
        let input = ClassifierInput {
            session: "s".into(),
            call_id: "c1".into(),
            at: "2026-10-11T12:00:00Z".into(),
            input: "rename foo".into(),
            quote: None,
        };
        write_inputs(&dir, now, &[input]).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join("2026-10-11.jsonl")), 0o600);
    }
}
