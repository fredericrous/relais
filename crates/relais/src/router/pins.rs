//! Agent definitions that pin a model (plan §6). The `agent.spawn` event
//! cannot show where a model came from (S0), so relais reads the
//! definitions the person maintains and serves the pins. Pure: the caller
//! reads the files.

/// The pin a definition states: its subagent type and its model. `None`
/// when the definition names no model, or `model: inherit` (which defers
/// to the parent, so it is no constraint).
pub type Pin = Option<(String, String)>;

/// Why a definition could not be read.
#[derive(Debug, Clone, PartialEq)]
pub struct PinError(pub String);

/// Read the YAML front matter of one `.claude/agents/<stem>.md`: the
/// `name:` (else the file stem) and `model:` keys. Only flat `key: value`
/// lines are read; anything else in the front matter is ignored, since
/// only those two keys matter here.
pub fn parse_definition(stem: &str, text: &str) -> Result<Pin, PinError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return Err(PinError(
            "no front matter (the file does not start with ---)".into(),
        ));
    }
    let mut name = None;
    let mut model = None;
    let mut closed = false;
    for line in lines {
        if line.trim_end() == "---" {
            closed = true;
            break;
        }
        if line.starts_with([' ', '\t']) {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = unquote(value.trim());
        match key.trim() {
            "name" if !value.is_empty() => name = Some(value.to_string()),
            "model" if !value.is_empty() => model = Some(value.to_string()),
            _ => {}
        }
    }
    if !closed {
        return Err(PinError("the front matter is never closed with ---".into()));
    }
    Ok(match model {
        Some(model) if model != "inherit" => {
            Some((name.unwrap_or_else(|| stem.to_string()), model))
        }
        _ => None,
    })
}

fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_definition_with_a_model_is_a_pin_named_by_its_name() {
        let text = "---\nname: my-reviewer\ndescription: x: y\nmodel: \"opus\"\ntools:\n  - Read\n---\nbody\n";
        assert_eq!(
            parse_definition("file", text),
            Ok(Some(("my-reviewer".into(), "opus".into())))
        );
        assert_eq!(
            parse_definition("file", "---\nmodel: haiku\n---\n"),
            Ok(Some(("file".into(), "haiku".into())))
        );
    }

    #[test]
    fn no_model_or_inherit_is_no_pin() {
        assert_eq!(parse_definition("a", "---\nname: a\n---\n"), Ok(None));
        assert_eq!(
            parse_definition("a", "---\nname: a\nmodel: inherit\n---\n"),
            Ok(None)
        );
    }

    #[test]
    fn a_malformed_definition_is_an_error_not_a_pin() {
        assert!(parse_definition("a", "no front matter\nmodel: opus\n").is_err());
        assert!(parse_definition("a", "---\nmodel: opus\n").is_err());
    }
}
