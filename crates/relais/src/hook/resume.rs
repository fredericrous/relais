//! The ready programme tickets a session that starts empty is told about
//! (SPEC §23, §31): `gather` reads them, `render` writes them.
//!
//! Reading is I/O and lives in [`gather`], which `respond` runs on a
//! worker thread under a deadline. [`render`] is pure over what was read,
//! so each cap and each mark has a unit test.

use std::collections::BTreeSet;
use std::path::Path;

use chrono::NaiveDate;

use super::respond::SkipReason;
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

/// The context text for a session that starts in `cwd`, or why there is
/// none: [`SkipReason::Nothing`] when its repository holds no programme or
/// nothing is ready, [`SkipReason::Error`] when the repository or its
/// programmes cannot be listed. A session start is never failed by this.
/// Reads checkouts and origin as the last fetch left them; fetches nothing
/// and prints nothing.
pub fn gather(cwd: &Path) -> Result<String, SkipReason> {
    let repo = main_checkout(cwd).map_err(|_| SkipReason::Error)?;
    let listed = load::programmes_of(&repo).map_err(|_| SkipReason::Error)?;
    let ready = frontier::ready(listed.found, listed.unreadable);
    let today = chrono::Local::now().date_naive();
    render(&repo, &ready, today, |found| {
        found.map_text().is_ok_and(|text| has_handoff(&text))
    })
    .ok_or(SkipReason::Nothing)
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
            shell_quote(&first.display().to_string())
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
        let noun = if programmes.len() == 1 {
            "programme"
        } else {
            "programmes"
        };
        let first_hidden = ready.found[hidden[0].programme].dir();
        text.push_str(&format!(
            "- … and {} more ready ({} {noun}; relais programme frontier --no-fetch {} lists them)\n",
            hidden.len(),
            programmes.len(),
            shell_quote(&first_hidden.display().to_string())
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
        .map(handoff_line)
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

/// The line that points at `found`'s `## Handoff`: the file for a
/// checkout's map, the command to run for a ref's.
fn handoff_line(found: &Found) -> String {
    match &found.source {
        Source::WorkingTree => format!(
            "- {}: the ## Handoff in {} says where the last session stopped ({HANDOFF_CONTRACT})\n",
            found.slug,
            found.dir().join(MAP_FILE).display()
        ),
        Source::Ref(rev) => format!(
            "- {}: run {}; its ## Handoff says where the last session stopped ({HANDOFF_CONTRACT})\n",
            found.slug,
            show_command(found, rev, MAP_FILE)
        ),
    }
}

/// Where ticket `id` of `found` is read from: the programme directory for
/// a checkout's files, the `git show` that prints it for a ref, which has
/// no file in any checkout.
fn ticket_locator(found: &Found, id: &str) -> String {
    match &found.source {
        Source::WorkingTree => found.dir().display().to_string(),
        Source::Ref(rev) => show_command(found, rev, &format!("{id}.md")),
    }
}

fn show_command(found: &Found, rev: &str, file: &str) -> String {
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

    /// A programme directory as `Found::dir` builds it: the checkout joined
    /// with the one relative `docs/programmes/<slug>`, so on Windows only the
    /// join itself is a backslash (`/work/repo\docs/programmes/alpha`).
    fn programme_dir(checkout: &str, slug: &str) -> PathBuf {
        Path::new(checkout).join(format!("docs/programmes/{slug}"))
    }

    /// The path as the text quotes it for a shell.
    fn quoted(path: &Path) -> String {
        shell_quote(&path.display().to_string())
    }

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
            text.contains(&format!(
                "- … and 2 more ready (2 programmes; relais programme frontier --no-fetch {} lists them)\n",
                quoted(&programme_dir("/work/repo", "alpha"))
            )),
            "{text}"
        );
        assert!(
            !text.contains("- beta:"),
            "a programme with no shown ticket has no locator: {text}"
        );
    }

    #[test]
    fn one_hidden_programme_is_singular_and_its_directory_is_quoted() {
        let mut founds = two_programmes();
        founds[0].checkout = PathBuf::from("/work/my repo");
        let mut rows: Vec<ReadyRow> = (0..8)
            .map(|n| row(1, "beta", &format!("t{n}"), "Shown", None))
            .collect();
        rows.push(row(0, "alpha", "t9", "Hidden", None));
        let text = render_of(&ready_of(founds, rows), false);
        assert!(
            text.contains(&format!(
                "- … and 1 more ready (1 programme; relais programme frontier --no-fetch {} lists them)\n",
                quoted(&programme_dir("/work/my repo", "alpha"))
            )),
            "{text}"
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
    fn the_unreadable_header_quotes_the_directory_to_check() {
        let mut ready = ready_of(two_programmes(), vec![row(0, "alpha", "t1", "Ready", None)]);
        ready.unreadable = vec![PathBuf::from("/work/my repo/docs/programmes/broken")];
        let text = render_of(&ready, false);
        assert!(
            text.contains(
                "(1 programme could not be read; run relais programme check '/work/my repo/docs/programmes/broken' for why)"
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
            text.contains(&format!(
                "- alpha: {}\n",
                programme_dir("/work/repo", "alpha").display()
            )),
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
        assert_eq!(
            text.matches("says where the last session stopped").count(),
            3,
            "{text}"
        );
        assert!(
            text.contains(&format!(
                "- p0: the ## Handoff in {}",
                programme_dir("/work/repo", "p0").join("map.md").display()
            )),
            "{text}"
        );
        assert!(
            text.contains(
                "- p1: run git -C /work/repo show origin/main:docs/programmes/p1/map.md; its ## Handoff says where the last session stopped (relais-handoff/1)\n"
            ),
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
    fn gather_says_why_it_said_nothing() {
        let outside = crate::test_support::short_temp_dir("gather-outside");
        assert_eq!(gather(&outside), Err(SkipReason::Error));

        let repo = crate::test_support::short_temp_dir("gather-empty");
        let git = |args: &[&str]| {
            let out = crate::workspace::git_command(&repo)
                .args(args)
                .output()
                .expect("git runs");
            assert!(out.status.success(), "git {args:?}");
        };
        git(&["init", "-q"]);
        assert_eq!(gather(&repo), Err(SkipReason::Nothing));
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
