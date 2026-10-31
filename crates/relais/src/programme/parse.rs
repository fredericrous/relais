//! Pure parsing of a ticket file and of `map.md`, modelled on
//! `task_labels`: every fault is reported with its line number and none
//! stops the parse of the rest.

use super::model::{MapDoc, Mode, Problem, Status, Ticket, TicketType, MAP_FILE, MAP_SECTIONS};

const FENCE_OF_FRONTMATTER: &str = "+++";

fn problem(file: &str, line: usize, message: impl Into<String>) -> Problem {
    Problem {
        file: file.to_string(),
        line,
        message: message.into(),
    }
}

/// The lines outside fenced code blocks, with their 0-based index.
fn prose_lines<'a>(lines: &[&'a str], from: usize) -> Vec<(usize, &'a str)> {
    let mut fenced = false;
    let mut kept = Vec::new();
    for (index, raw) in lines.iter().enumerate().skip(from) {
        let trimmed = raw.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
        } else if !fenced {
            kept.push((index, *raw));
        }
    }
    kept
}

/// The ticket `id` read from `text`. The ticket is `None` when its
/// frontmatter cannot be read or its type, mode or status is missing or
/// unknown: nothing else about it can be trusted then.
pub fn parse_ticket(id: &str, text: &str) -> (Option<Ticket>, Vec<Problem>) {
    let file = format!("{id}.md");
    let lines: Vec<&str> = text.lines().collect();
    let mut problems = Vec::new();
    if lines.first().map(|l| l.trim_end()) != Some(FENCE_OF_FRONTMATTER) {
        problems.push(problem(
            &file,
            1,
            "no TOML frontmatter: the file must start with a `+++` line",
        ));
        return (None, problems);
    }
    let Some(close) = lines
        .iter()
        .skip(1)
        .position(|l| l.trim_end() == FENCE_OF_FRONTMATTER)
        .map(|offset| offset + 1)
    else {
        problems.push(problem(
            &file,
            1,
            "the frontmatter has no closing `+++` line",
        ));
        return (None, problems);
    };
    let frontmatter_lines = &lines[1..close];
    let frontmatter = frontmatter_lines.join("\n");
    let table: toml::Table = match toml::from_str(&frontmatter) {
        Ok(table) => table,
        Err(error) => {
            let line = error.span().map_or(0, |span| {
                frontmatter[..span.start.min(frontmatter.len())]
                    .matches('\n')
                    .count()
            }) + 2;
            problems.push(problem(
                &file,
                line,
                format!("invalid frontmatter: {}", error.message()),
            ));
            return (None, problems);
        }
    };
    let key_line = |key: &str| -> usize {
        frontmatter_lines
            .iter()
            .position(|l| {
                l.strip_prefix(key)
                    .is_some_and(|rest| rest.trim_start().starts_with('='))
            })
            .map_or(2, |index| index + 2)
    };
    for key in table.keys() {
        if !["type", "mode", "status", "blocked_by", "repos"].contains(&key.as_str()) {
            problems.push(problem(
                &file,
                key_line(key),
                format!("unknown frontmatter key `{key}`"),
            ));
        }
    }
    let word = |key: &str| -> Result<String, Problem> {
        match table.get(key) {
            None => Err(problem(&file, 2, format!("frontmatter has no `{key}`"))),
            Some(toml::Value::String(text)) => Ok(text.clone()),
            Some(_) => Err(problem(
                &file,
                key_line(key),
                format!("`{key}` must be a string"),
            )),
        }
    };
    let words = |key: &str| -> Result<Option<Vec<String>>, Problem> {
        let Some(value) = table.get(key) else {
            return Ok(None);
        };
        let not_strings = || {
            problem(
                &file,
                key_line(key),
                format!("`{key}` must be an array of strings"),
            )
        };
        let toml::Value::Array(items) = value else {
            return Err(not_strings());
        };
        items
            .iter()
            .map(|item| item.as_str().map(str::to_string))
            .collect::<Option<Vec<String>>>()
            .map(Some)
            .ok_or_else(not_strings)
    };
    let unknown = |key: &str, text: &str, names: &str| {
        problem(
            &file,
            key_line(key),
            format!("unknown {key} `{text}`: use one of {names}"),
        )
    };
    let kind = word("type").and_then(|text| {
        TicketType::parse(&text).ok_or_else(|| unknown("type", &text, TicketType::NAMES))
    });
    let mode = word("mode")
        .and_then(|text| Mode::parse(&text).ok_or_else(|| unknown("mode", &text, Mode::NAMES)));
    let status = word("status").and_then(|text| {
        Status::parse(&text).ok_or_else(|| unknown("status", &text, Status::NAMES))
    });
    let blocked_by = words("blocked_by");
    let repos = words("repos");
    let kind = kind.map_err(|found| problems.push(found)).ok();
    let mode = mode.map_err(|found| problems.push(found)).ok();
    let status = status.map_err(|found| problems.push(found)).ok();
    let blocked_by = blocked_by
        .map_err(|found| problems.push(found))
        .ok()
        .flatten();
    let repos = repos.map_err(|found| problems.push(found)).ok().flatten();

    let body = prose_lines(&lines, close + 1);
    let heading = body
        .iter()
        .find(|(_, l)| l.starts_with("# "))
        .map(|(index, l)| (*index + 1, l[2..].trim().to_string()));
    if heading.is_none() {
        problems.push(problem(
            &file,
            close + 1,
            "no `# ` heading naming the ticket",
        ));
    }
    let has_section = |title: &str| body.iter().any(|(_, l)| l.trim_end() == title);
    let has_question = has_section("## Question");
    let has_resolution = has_section("## Resolution");
    if !has_question {
        problems.push(problem(
            &file,
            heading.as_ref().map_or(close + 1, |(line, _)| *line),
            "no `## Question` section",
        ));
    }
    let (Some(kind), Some(mode), Some(status)) = (kind, mode, status) else {
        return (None, problems);
    };
    let (heading_line, name) = heading.unwrap_or((close + 1, id.to_string()));
    let ticket = Ticket {
        id: id.to_string(),
        name,
        kind,
        mode,
        status,
        blocked_by: blocked_by.unwrap_or_default(),
        repos,
        has_question,
        has_resolution,
        status_line: key_line("status"),
        blocked_by_line: key_line("blocked_by"),
        repos_line: key_line("repos"),
        heading_line,
    };
    (Some(ticket), problems)
}

/// `map.md` read from `text`: the text of the sections the commands use,
/// and a problem for each section missing, repeated or out of order.
pub fn parse_map(text: &str) -> (MapDoc, Vec<Problem>) {
    let lines: Vec<&str> = text.lines().collect();
    let mut problems = Vec::new();
    let headings: Vec<(usize, String)> = prose_lines(&lines, 0)
        .into_iter()
        .filter_map(|(index, l)| {
            l.strip_prefix("## ")
                .map(|title| (index, title.trim().to_string()))
        })
        .collect();
    let section_text = |title: &str| -> String {
        headings
            .iter()
            .position(|(_, found)| found == title)
            .map(|at| {
                let end = headings.get(at + 1).map_or(lines.len(), |(next, _)| *next);
                lines[headings[at].0 + 1..end].join("\n").trim().to_string()
            })
            .unwrap_or_default()
    };
    let mut furthest: Option<usize> = None;
    for (index, title) in &headings {
        let Some(position) = MAP_SECTIONS.iter().position(|s| s == title) else {
            continue;
        };
        match furthest {
            Some(seen) if position == seen => problems.push(problem(
                MAP_FILE,
                index + 1,
                format!("section `## {title}` appears twice"),
            )),
            Some(seen) if position < seen => problems.push(problem(
                MAP_FILE,
                index + 1,
                format!(
                    "section `## {title}` is out of order: the order is {}",
                    MAP_SECTIONS.map(|s| format!("`## {s}`")).join(", ")
                ),
            )),
            _ => furthest = Some(position),
        }
    }
    for section in MAP_SECTIONS {
        if !headings.iter().any(|(_, title)| title == section) {
            problems.push(problem(
                MAP_FILE,
                1,
                format!("missing section `## {section}`"),
            ));
        }
    }
    let doc = MapDoc {
        destination: section_text("Destination"),
        decisions: section_text("Decisions so far"),
        out_of_scope: section_text("Out of scope"),
    };
    (doc, problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICKET: &str = "+++\ntype = \"research\"\nmode = \"afk\"\nstatus = \"open\"\nblocked_by = [\"A\"]\n+++\n# Pick a store\n\n## Question\n\nWhich?\n";

    #[test]
    fn a_ticket_is_read_with_its_defaults() {
        let (ticket, problems) = parse_ticket("B", TICKET);
        assert_eq!(problems, []);
        let ticket = ticket.expect("ticket");
        assert_eq!(ticket.name, "Pick a store");
        assert_eq!(ticket.blocked_by, ["A"]);
        assert_eq!(ticket.repos, None);
        assert_eq!(ticket.blocked_by_line, 5);
        assert!(ticket.has_question && !ticket.has_resolution);
    }

    #[test]
    fn every_fault_of_a_ticket_is_reported_with_its_line() {
        let text = TICKET
            .replace("research", "poetry")
            .replace("\"afk\"", "\"never\"")
            .replace("## Question", "## Ask");
        let (ticket, problems) = parse_ticket("B", &text);
        assert!(ticket.is_none(), "an unknown type or mode drops the ticket");
        let lines: Vec<usize> = problems.iter().map(|p| p.line).collect();
        assert_eq!(lines, [2, 3, 7]);
        assert!(problems[0].message.contains("poetry"));
    }

    #[test]
    fn an_unknown_status_or_broken_frontmatter_drops_the_ticket() {
        let (ticket, problems) = parse_ticket("B", &TICKET.replace("\"open\"", "\"done\""));
        assert!(ticket.is_none());
        assert_eq!(problems[0].line, 4);
        let (ticket, problems) = parse_ticket("B", "# no frontmatter\n");
        assert!(ticket.is_none());
        assert_eq!(problems[0].line, 1);
        let (ticket, problems) = parse_ticket("B", "+++\nstatus = \n+++\n");
        assert!(ticket.is_none());
        assert_eq!(problems[0].line, 2);
    }

    #[test]
    fn headings_inside_code_fences_do_not_count() {
        let text = TICKET.replace("Which?", "```\n## Resolution\n```");
        let (ticket, _) = parse_ticket("B", &text);
        assert!(!ticket.expect("ticket").has_resolution);
    }

    #[test]
    fn a_map_reports_missing_repeated_and_misordered_sections() {
        let (doc, problems) = parse_map(
            "# Map\n## Destination\nGo far.\n## Decisions so far\n- x (A.md)\n## Notes\n## Notes\n",
        );
        assert_eq!(doc.destination, "Go far.");
        assert_eq!(doc.decisions, "- x (A.md)");
        let found: Vec<(usize, &str)> = problems
            .iter()
            .map(|p| (p.line, p.message.as_str()))
            .collect();
        assert_eq!(found.len(), 4, "{found:?}");
        assert_eq!(found[0].0, 6, "Notes after Decisions so far");
        assert_eq!(found[1].0, 7, "Notes twice");
        assert!(found[2].1.contains("Not yet specified"));
        assert!(found[3].1.contains("Out of scope"));
    }
}
