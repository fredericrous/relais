+++
type = "grilling"
mode = "hitl"
status = "open"
blocked_by = []
repos = ["relais", "decisions"]
+++
# What renames from programme to epic, and how long both spellings work

## Question

What exactly renames, what stays, and how long do both spellings work? Cover:

- the directory `docs/epics/` and the file `epic.md`, and the `sNN` story ids;
- the branch prefix: both `prog/` and `epic/` count as a claim for the whole
  window, so a branch already in flight never shows as ready again, and
  `frontier` treats both as in flight;
- which directory wins when a slug exists in both `docs/programmes` and
  `docs/epics`;
- the session-start text: the header, every noun in it such as "ticket", and
  the commands it prints (`relais programme check`, `relais programme list`)
  switch in one release, and every consumer that matches the header string is
  listed;
- the `relais epic` command with `programme` as an alias: its deprecation
  note goes to stderr only, never into `--json` or hook output, and names
  `relais epic <same args>`; exit codes and `--json` keys stay the same;
- the `relais-handoff/1` contract, and the migration of the 34 maps.

Output: a decisions ADR that supersedes the `work.programme-map` head
(ADR-0084) and the `harness.skills-source` head (ADR-0051), which names the
`programme` skill.

## Look at

- `crates/relais/src/programme/` (`load.rs:355-356`, `analysis.rs:49` for the
  `prog/` prefix), `crates/relais/src/cli/mod.rs:468-490` and
  `cli/programme.rs`.
- `crates/relais/src/hook/resume.rs` (the session-start text and the
  `HANDOFF_CONTRACT`) and the test pinning it,
  `crates/relais/tests/it/programme_cli.rs`.
- `claude-plugin/skills/programme/SKILL.md` and `claude-plugin/hooks/programme.ts`.
- decisions `docs/adr/HEADS.md` rows for `work.programme-map` and
  `harness.skills-source`; `docs/programmes/relais-modularise/t04-names.md`.

## Answer looks like

A table with one row per surface (directory, file, id, branch prefix, command,
hook text, skill, contract, maps): the old spelling, the new, how long both
work, and who reads it. The ADR's decision text, and the list of consumers of
the header string.
