//! The run marker relais puts in a native worker's prompt.
//!
//! A native worker is a subagent the parent session spawns. The only
//! thing that travels with the spawn from relais to the hook is the
//! prompt, so the prompt carries one line, `[relais-dispatch: <id>]`, and
//! the hook reads it back. Pure: text in, text or a verdict out.

const MARKER_OPEN: &str = "[relais-dispatch:";
const MAX_ID_CHARS: usize = 128;

/// What a piece of text says about the dispatch it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Marker {
    /// No marker at all: an ordinary prompt.
    None,
    /// One or more copies of exactly one dispatch id.
    One(String),
    /// Two different ids, or a marker whose id is empty, too long or has
    /// a character outside `[A-Za-z0-9_-]`. Never guessed at.
    Ambiguous,
}

/// The marker line for a dispatch.
pub fn marker_line(dispatch_id: &str) -> String {
    format!("{MARKER_OPEN} {dispatch_id}]")
}

/// Read the marker out of `text`.
pub fn find_marker(text: &str) -> Marker {
    let mut found: Option<&str> = None;
    let mut rest = text;
    while let Some(start) = rest.find(MARKER_OPEN) {
        let after = &rest[start + MARKER_OPEN.len()..];
        // An opening with no closing bracket names no id: malformed.
        let Some(end) = after.find(']') else {
            return Marker::Ambiguous;
        };
        let id = after[..end].trim();
        if !is_valid_id(id) {
            return Marker::Ambiguous;
        }
        match found {
            Some(earlier) if earlier != id => return Marker::Ambiguous,
            Some(_) | None => found = Some(id),
        }
        rest = &after[end + 1..];
    }
    match found {
        Some(id) => Marker::One(id.to_string()),
        None => Marker::None,
    }
}

fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.chars().count() <= MAX_ID_CHARS
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_marker_line_round_trips() {
        for id in ["d1", "dispatch_9-x", &"a".repeat(128)] {
            assert_eq!(find_marker(&marker_line(id)), Marker::One(id.to_string()));
        }
    }

    #[test]
    fn a_marker_is_found_inside_a_longer_prompt() {
        let text = format!("do the work\n{}\nthanks", marker_line("d1"));
        assert_eq!(find_marker(&text), Marker::One("d1".into()));
    }

    #[test]
    fn text_without_a_marker_has_none() {
        assert_eq!(find_marker(""), Marker::None);
        assert_eq!(find_marker("just a prompt [with brackets]"), Marker::None);
    }

    #[test]
    fn copies_of_one_id_are_one() {
        let text = format!("{}\n{}", marker_line("d1"), marker_line("d1"));
        assert_eq!(find_marker(&text), Marker::One("d1".into()));
    }

    #[test]
    fn two_different_ids_are_ambiguous() {
        let text = format!("{}\n{}", marker_line("d1"), marker_line("d2"));
        assert_eq!(find_marker(&text), Marker::Ambiguous);
    }

    #[test]
    fn a_bad_id_is_ambiguous() {
        for id in ["", "a b", "a/b", "a;b", &"a".repeat(129)] {
            assert_eq!(find_marker(&marker_line(id)), Marker::Ambiguous, "{id:?}");
        }
        assert_eq!(find_marker("[relais-dispatch: d1"), Marker::Ambiguous);
    }
}
