//! How a looping worker's command is written down and shown. The text
//! rules live beside the guard, so the adapter's shadow line and the
//! runner's stop line cannot drift apart.

pub(super) use crate::adapter::loopguard::{
    failing_phrase, redacted_one_line, same_output_phrase, shown,
};

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
