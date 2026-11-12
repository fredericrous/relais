---
status: active
branch: feat/programme-due-dates
repos: [relais]
adrs: [ADR-0062]
---
# Programme tickets carry a due date

## Review panel
👉 **Decide:** none — approve if a name prefix (`[due …]`/`[overdue …]`) and `null` JSON fields are the right shape for `frontier`.
📍 relais · plan reviewed, nothing built · next: worktree + Phase 1 (via relais). Panel: backend, lang:rust (opus), codex (gpt-6-astra).
**Changed by review:** sort key `(due.is_none(), due)`; strict 10-char date parse; rollout holds `due` until the release is installed.
**Verdicts:** round 1 2× approve-with-changes; round 2 2× approve-with-changes; codex approve-with-changes; 9 carried into implementation.
📄 Full reviews: [2026-11-12-programme-tickets-carry-a-due-date.reviews.md](2026-11-12-programme-tickets-carry-a-due-date.reviews.md)

## Picture
```
ticket.md  +++ type = "task" ... due = "2026-11-19" +++
     |
     v
parse.rs parse_ticket -- due on task, valid date --> Ticket.due
     |                +- malformed / not a task --> Problem (check exit 2)
     v
cli/programme.rs frontier
     sort: due tickets first (soonest), then undated in id order
     text: id  type/mode  repos  [overdue 2026-11-10] name
     json: {"due": "2026-11-19", "overdue": false, ...}
```
The picture shows the one new field going from a ticket's front matter
through `check` into the order and the marks `frontier` prints.

## Goal
A `task` ticket may carry `due = "YYYY-MM-DD"`. `relais programme check`
accepts it there and refuses it malformed or on another type, and `relais
programme frontier` lists due tickets first, soonest first, marking the
overdue ones. This is the precondition `work.programme-map` (ADR-0062)
names before a dated measure becomes a ticket; it resolves the
session-boundaries ticket "relais accepts and orders due tickets"
(`decisions:docs/programmes/session-boundaries/t01-due-tickets.md`) and
unblocks its t04, t05, t08 and t09.

## Non-goals
- No `due` on research, prototype or grilling tickets.
- No new column in the text output: its four-column shape stays (the mark
  is a prefix on the name).
- No reminder or notification: SessionStart showing due tickets is t05.
- `list` and `show` text output are unchanged.

## Code review
- relais: backend, lang:rust, unix, tui

## Behaviour
- `due = "2026-11-19"` on a `task` ticket: `check` exit 0.
- `due` on a `research`/`prototype`/`grilling` ticket: a problem
  `<file>:<line>: due is only allowed on a task ticket`, exit 2.
- `due = "2026-13-40"`, `due = "next week"`, an unquoted TOML date or
  another non-string: a problem
  `<file>:<line>: due must be a date as "YYYY-MM-DD"`, exit 2.
- `frontier` orders open, unblocked tickets: those with `due` first, by
  date ascending, ties by id; then those without `due` in today's id order.
  Under `--all`, in-flight rows sort with the same key and carry the same mark.
- Text rows keep `id\ttype/mode\trepos\tname`; a due ticket's name is
  prefixed `[due YYYY-MM-DD] `, or `[overdue YYYY-MM-DD] ` when the date is
  before today (local date). Today itself is due, not overdue.
- `frontier --json` rows gain `"due": "YYYY-MM-DD"` and `"overdue":
  true|false`, both `null` when the ticket has no `due` (as `branch` is).

## Phases
- [ ] Phase 1 — model and check: `crates/relais/src/programme/model.rs`
  (`Ticket.due: Option<NaiveDate>`, `due_line`),
  `crates/relais/src/programme/parse.rs` (allow-list, strict date parse requiring exactly 10 chars before `NaiveDate::parse_from_str` so `2026-1-5` is refused, an unquoted TOML date given its own message, the task-only problem raised only when `kind` is `Some`, `Ticket.due = None` on every failure, unit tests incl. `2026-1-5`, `2026-02-30`, unquoted date, valid `due` giving zero problems), `crates/relais/src/programme/analysis.rs` (test
  helper `ticket()` and any struct literal).
- [ ] Phase 2 — frontier order and marks: `crates/relais/src/cli/programme.rs`
  (a pure `order_frontier(tickets, today)` using a stable sort on the key
  `(due.is_none(), due)`, so undated tickets stay last in id order, also
  for `--all` rows; a pure `due_mark(due, today)` unit-tested on the day
  before, the same day and the day after; the name prefix counted in the
  `fit_name` width and absent from the JSON `name`; today from
  `chrono::Local`),
  `crates/relais/tests/it/programme_cli.rs` (binary tests with far-past and
  far-future dates: order, overdue mark, JSON fields, `check` problems).
- [ ] Phase 3 — docs and release notes:
  `claude-plugin/skills/programme/SKILL.md` (the field in the ticket
  example), `docs/SPEC.md` (front matter and frontier order; after the SKILL.md edit
  run `cargo test -p relais --lib install::` and the plugin embed test),
  `CHANGELOG.md` (Unreleased → Added). Release via `tag-release` after the
  merge; the install is the person's. No ticket in the decisions repo gets
  `due` until `relais --version` shows this release installed: an older
  relais refuses the key (`unknown frontmatter key`, parse.rs:87-95).

## Decision log
- 2026-11-12 — The overdue mark is a name prefix, not a column, because the
  four-column text shape is relied on by `cut`/`awk` users (the comment by
  the row format in `cli/programme.rs`).
- 2026-11-12 — Today is the local date from `chrono::Local`, because a due
  date is a calendar day the person reads; the order is a pure function
  taking `today`, so tests never touch the clock.

## Verification
- Phase 2 unit: `order_frontier` with `today` equal to a ticket's due date →
  `[due …]`, not `[overdue …]`; the day before today → overdue → <observed>.
- Phase 1: `relais programme check` on three scratch programmes, one each
  with a valid, a misplaced and a malformed `due` → exit 0 / 2 / 2, the problem lines at
  the right line numbers → <observed>.
- Phase 2: `relais programme frontier` on a scratch programme with tickets
  due 2000-01-01, 2999-01-01 and undated → that order; the first prefixed
  `[overdue 2000-01-01]`; `--json` carries `due` and `overdue` →
  <observed>.
- Real input: `check` and `frontier` on
  `decisions/docs/programmes/session-boundaries` (no `due` yet) → output
  identical to the installed relais → <observed>.

## Outcome

<!-- panel: repos=relais reviewers=backend,lang:rust,codex body-sha=24611cdf3d40 -->
