//! The ready programme tickets a session that starts empty is told about
//! (SPEC §23, §31): `gather` reads them, `render` writes them.
//!
//! Reading is I/O and lives in [`gather`], which `respond` runs on a
//! worker thread under a deadline. [`render`] is pure over what was read,
//! so each cap and each mark has a unit test.

use std::collections::BTreeSet;
use std::path::Path;

use chrono::NaiveDate;

use crate::programme::frontier::{self, due_mark, has_handoff, Ready, ReadyRow};
use crate::programme::load::{self, main_checkout, Found, Source};
use crate::programme::model::MAP_FILE;

/// Ticket lines shown before the rest are counted.
pub const TICKET_LINE_CAP: usize = 8;

/// Characters of a ticket name shown before it is cut with `…`.
pub const NAME_CAP: usize = 200;

/// Programmes whose `## Handoff` is pointed at.
pub const HANDOFF_POINTER_CAP: usize = 3;

/// The version of the contract that a map's `## Handoff` heading is read
/// under (SPEC §31): the heading's name is what relais reads, nothing in
/// its body.
pub const HANDOFF_CONTRACT: &str = "relais-handoff/1";

/// The context text for a session that starts in `cwd`, or `None` when
/// `cwd` is in no repository, its repository holds no programme, nothing
/// is ready, or anything fails: a session start is never failed by this.
/// Reads checkouts and origin as the last fetch left them; fetches nothing
/// and prints nothing.
pub fn gather(cwd: &Path) -> Option<String> {
    let repo = main_checkout(cwd).ok()?;
    let listed = load::programmes_of(&repo).ok()?;
    let ready = frontier::ready(listed.found);
    let today = chrono::Local::now().date_naive();
    render(&repo, &ready, today, |found| {
        found.map_text().is_ok_and(|text| has_handoff(&text))
    })
}

/// Text for the ready tickets, or `None` when there is none. `handoff_in`
/// says whether a programme's selected map has a `## Handoff`; it is asked
/// only of programmes with a shown ticket, at most
/// [`HANDOFF_POINTER_CAP`] of them being named.
pub fn render(
    repo: &Path,
    ready: &Ready,
    today: NaiveDate,
    handoff_in: impl Fn(&Found) -> bool,
) -> Option<String> {
    if ready.rows.is_empty() {
        return None;
    }
    let mut text = format!(
        "relais/programmes: {} ready in {}",
        ready.rows.len(),
        repo.display()
    );
    if let Some(first) = ready.unreadable.first() {
        let count = ready.unreadable.len();
        let noun = if count == 1 {
            "programme"
        } else {
            "programmes"
        };
        text.push_str(&format!(
            " ({count} {noun} could not be read; run relais programme check {} for why)",
            first.display()
        ));
    }
    text.push_str(":\n");

    let (shown, hidden) = ready.rows.split_at(ready.rows.len().min(TICKET_LINE_CAP));
    for row in shown {
        text.push_str(&format!(
            "- {} {}: {}{}\n",
            row.slug,
            row.id,
            due_mark(row.due, today),
            cut_name(&row.name)
        ));
    }
    if !hidden.is_empty() {
        let programmes: BTreeSet<usize> = hidden.iter().map(|row| row.programme).collect();
        text.push_str(&format!(
            "- … and {} more ready ({} programmes)\n",
            hidden.len(),
            programmes.len()
        ));
    }

    let shown_programmes = programmes_shown(shown);
    text.push_str("read a ticket from:\n");
    for (position, first_id) in &shown_programmes {
        let found = &ready.found[*position];
        text.push_str(&format!(
            "- {}: {}\n",
            found.slug,
            ticket_locator(found, first_id)
        ));
    }

    let pointers: Vec<String> = shown_programmes
        .iter()
        .map(|(position, _)| &ready.found[*position])
        .filter(|found| handoff_in(found))
        .take(HANDOFF_POINTER_CAP)
        .map(|found| {
            format!(
                "- {}: the ## Handoff in {} says where the last session stopped ({HANDOFF_CONTRACT})\n",
                found.slug,
                map_locator(found)
            )
        })
        .collect();
    if !pointers.is_empty() {
        text.push_str("handoff:\n");
        pointers.iter().for_each(|pointer| text.push_str(pointer));
    }
    Some(text)
}

/// The programmes with a shown ticket, in order of first appearance, each
/// with the id of its first shown ticket.
fn programmes_shown(shown: &[ReadyRow]) -> Vec<(usize, String)> {
    let mut seen = BTreeSet::new();
    shown
        .iter()
        .filter(|row| seen.insert(row.programme))
        .map(|row| (row.programme, row.id.clone()))
        .collect()
}

fn cut_name(name: &str) -> String {
    if name.chars().count() <= NAME_CAP {
        return name.to_string();
    }
    let kept: String = name.chars().take(NAME_CAP).collect();
    format!("{kept}…")
}

/// Where ticket `id` of `found` is read from: the programme directory for
/// a checkout's files, the `git show` that prints it for a ref, which has
/// no file in any checkout.
fn ticket_locator(found: &Found, id: &str) -> String {
    match &found.source {
        Source::WorkingTree => found.dir().display().to_string(),
        Source::Ref(_) => show_command(found, &format!("{id}.md")),
    }
}

/// Where the map of `found` is read from, by the same source as its tickets.
fn map_locator(found: &Found) -> String {
    match &found.source {
        Source::WorkingTree => found.dir().join(MAP_FILE).display().to_string(),
        Source::Ref(_) => show_command(found, MAP_FILE),
    }
}

fn show_command(found: &Found, file: &str) -> String {
    let Source::Ref(rev) = &found.source else {
        return String::new();
    };
    format!(
        "git -C {} show {}",
        shell_quote(&found.repo.display().to_string()),
        shell_quote(&format!("{rev}:docs/programmes/{}/{file}", found.slug))
    )
}

/// `word` as one shell word: bare when it holds only characters a shell
/// never treats specially, else in single quotes.
fn shell_quote(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-./:@%+=,".contains(c));
    if plain {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn day(year: i32, month: u32, date: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, date).expect("a real day")
    }

    fn found(slug: &str, source: Source) -> Found {
        Found {
            repo: PathBuf::from("/work/repo"),
            checkout: PathBuf::from("/work/repo"),
            slug: slug.to_string(),
            source,
        }
    }

    fn row(programme: usize, slug: &str, id: &str, name: &str, due: Option<NaiveDate>) -> ReadyRow {
        ReadyRow {
            programme,
            slug: slug.to_string(),
            id: id.to_string(),
            name: name.to_string(),
            due,
        }
    }

    fn ready_of(founds: Vec<Found>, rows: Vec<ReadyRow>) -> Ready {
        Ready {
            found: founds,
            rows,
            unreadable: Vec::new(),
        }
    }

    fn two_programmes() -> Vec<Found> {
        vec![
            found("alpha", Source::WorkingTree),
            found("beta", Source::Ref("origin/main".into())),
        ]
    }

    fn render_of(ready: &Ready, handoff: bool) -> String {
        render(Path::new("/work/repo"), ready, day(2026, 11, 17), |_| {
            handoff
        })
        .expect("something is ready")
    }

    #[test]
    fn nothing_ready_says_nothing() {
        let ready = ready_of(two_programmes(), Vec::new());
        assert_eq!(
            render(Path::new("/work/repo"), &ready, day(2026, 11, 17), |_| true),
            None
        );
    }

    #[test]
    fn rows_print_their_marks_and_today_is_due_not_overdue() {
        let ready = ready_of(
            two_programmes(),
            vec![
                row(1, "beta", "t2", "Overdue one", Some(day(2026, 11, 1))),
                row(0, "alpha", "t1", "Due today", Some(day(2026, 11, 17))),
                row(0, "alpha", "t3", "Undated", None),
            ],
        );
        let text = render_of(&ready, false);
        assert!(
            text.starts_with("relais/programmes: 3 ready in /work/repo:\n"),
            "{text}"
        );
        assert!(
            text.contains("- beta t2: [overdue 2026-11-01] Overdue one\n"),
            "{text}"
        );
        assert!(
            text.contains("- alpha t1: [due 2026-11-17] Due today\n"),
            "{text}"
        );
        assert!(text.contains("- alpha t3: Undated\n"), "{text}");
    }

    #[test]
    fn a_name_past_the_cap_is_cut_with_an_ellipsis() {
        let long = "x".repeat(NAME_CAP + 40);
        let ready = ready_of(
            two_programmes(),
            vec![
                row(0, "alpha", "t1", &long, None),
                row(0, "alpha", "t2", &"y".repeat(NAME_CAP), None),
            ],
        );
        let text = render_of(&ready, false);
        assert!(
            text.contains(&format!("- alpha t1: {}…\n", "x".repeat(NAME_CAP))),
            "{text}"
        );
        assert!(
            text.contains(&format!("- alpha t2: {}\n", "y".repeat(NAME_CAP))),
            "{text}"
        );
    }

    #[test]
    fn past_eight_tickets_the_rest_is_counted_with_its_programmes() {
        let mut rows: Vec<ReadyRow> = (0..8)
            .map(|n| row(0, "alpha", &format!("t{n}"), "Shown", None))
            .collect();
        rows.push(row(0, "alpha", "t8", "Hidden", None));
        rows.push(row(1, "beta", "t9", "Hidden", None));
        let text = render_of(&ready_of(two_programmes(), rows), false);
        assert_eq!(text.matches("Shown\n").count(), 8, "{text}");
        assert!(!text.contains("Hidden\n"), "{text}");
        assert!(
            text.contains("- … and 2 more ready (2 programmes)\n"),
            "{text}"
        );
        assert!(
            !text.contains("- beta:"),
            "a programme with no shown ticket has no locator: {text}"
        );
    }

    #[test]
    fn exactly_eight_tickets_print_no_more_line() {
        let rows: Vec<ReadyRow> = (0..8)
            .map(|n| row(0, "alpha", &format!("t{n}"), "Shown", None))
            .collect();
        let text = render_of(&ready_of(two_programmes(), rows), false);
        assert!(!text.contains("more ready"), "{text}");
    }

    #[test]
    fn a_failing_load_is_counted_and_names_the_first_directory_to_check() {
        let mut ready = ready_of(two_programmes(), vec![row(0, "alpha", "t1", "Ready", None)]);
        ready.unreadable = vec![
            PathBuf::from("/work/repo/docs/programmes/broken"),
            PathBuf::from("/work/repo/docs/programmes/worse"),
        ];
        let text = render_of(&ready, false);
        assert!(
            text.starts_with(
                "relais/programmes: 1 ready in /work/repo (2 programmes could not be read; run relais programme check /work/repo/docs/programmes/broken for why):\n"
            ),
            "{text}"
        );
    }

    #[test]
    fn a_checkout_programme_prints_its_directory_and_a_ref_programme_a_quoted_git_show() {
        let mut founds = two_programmes();
        founds[1].repo = PathBuf::from("/work/my repo");
        let ready = ready_of(
            founds,
            vec![
                row(0, "alpha", "t1", "A", None),
                row(1, "beta", "t2", "B", None),
            ],
        );
        let text = render_of(&ready, false);
        assert!(
            text.contains("- alpha: /work/repo/docs/programmes/alpha\n"),
            "{text}"
        );
        assert!(
            text.contains(
                "- beta: git -C '/work/my repo' show origin/main:docs/programmes/beta/t2.md\n"
            ),
            "{text}"
        );
    }

    #[test]
    fn handoff_pointers_follow_each_programmes_source_and_stop_at_three() {
        let founds: Vec<Found> = (0..5)
            .map(|n| {
                let source = if n == 1 {
                    Source::Ref("origin/main".into())
                } else {
                    Source::WorkingTree
                };
                found(&format!("p{n}"), source)
            })
            .collect();
        let rows: Vec<ReadyRow> = (0..5)
            .map(|n| row(n, &format!("p{n}"), "t1", "Ready", None))
            .collect();
        let text = render_of(&ready_of(founds, rows), true);
        assert_eq!(text.matches("the ## Handoff in").count(), 3, "{text}");
        assert!(
            text.contains("- p0: the ## Handoff in /work/repo/docs/programmes/p0/map.md"),
            "{text}"
        );
        assert!(
            text.contains("- p1: the ## Handoff in git -C /work/repo show origin/main:docs/programmes/p1/map.md says"),
            "{text}"
        );
        assert!(text.contains(HANDOFF_CONTRACT), "{text}");
        assert!(!text.contains("- p3: the ## Handoff"), "{text}");
    }

    #[test]
    fn a_programme_with_no_handoff_has_no_pointer() {
        let ready = ready_of(two_programmes(), vec![row(0, "alpha", "t1", "A", None)]);
        let text = render_of(&ready, false);
        assert!(!text.contains("handoff:"), "{text}");
    }

    #[test]
    fn a_word_is_quoted_only_when_a_shell_would_read_it_specially() {
        assert_eq!(
            shell_quote("origin/main:docs/a.md"),
            "origin/main:docs/a.md"
        );
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote(""), "''");
    }
}
