//! How a looping worker's command is written down and shown.

use crate::router::capture::redact;

/// The longest command the stderr event line shows, in characters.
pub(super) const SHOWN_COMMAND_CHARS: usize = 60;

/// `text` with known secret shapes redacted, on one line: whitespace runs
/// (newlines included) become one space and other control characters are
/// escaped.
pub(super) fn redacted_one_line(text: &str) -> String {
    let (redacted, _) = redact(text);
    let mut line = String::with_capacity(redacted.len());
    let mut in_gap = false;
    for ch in redacted.trim().chars() {
        if ch.is_whitespace() {
            if !in_gap {
                line.push(' ');
            }
            in_gap = true;
            continue;
        }
        in_gap = false;
        if ch.is_control() {
            line.extend(ch.escape_default());
        } else {
            line.push(ch);
        }
    }
    line
}

/// `line` cut to `SHOWN_COMMAND_CHARS` characters (Unicode scalar values),
/// the last one `…`, when it is longer.
pub(super) fn shown(line: &str) -> String {
    if line.chars().count() <= SHOWN_COMMAND_CHARS {
        return line.to_string();
    }
    let mut cut: String = line.chars().take(SHOWN_COMMAND_CHARS - 1).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_becomes_one_redacted_line_with_control_bytes_escaped() {
        let line =
            redacted_one_line("curl -H 'Authorization: Bearer abc123xyz'\n\n  echo\t\u{1b}[31mhi");
        assert!(!line.contains("abc123xyz"), "{line}");
        assert!(!line.contains('\n') && !line.contains('\u{1b}'), "{line}");
        assert!(line.contains("\\u{1b}"), "{line}");
        assert!(line.contains(" echo "), "{line}");
    }

    #[test]
    fn a_long_command_is_cut_to_sixty_characters_ending_in_an_ellipsis() {
        let long = "é".repeat(200);
        let cut = shown(&long);
        assert_eq!(cut.chars().count(), 60);
        assert!(cut.ends_with('…'));
        assert_eq!(shown("make test"), "make test");
        let exact = "a".repeat(60);
        assert_eq!(shown(&exact), exact);
    }
}
