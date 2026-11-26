+++
type = "grilling"
mode = "hitl"
status = "open"
blocked_by = ["t02-seams"]
repos = ["relais"]
+++
# What the pieces of relais and the family are called

## Question

What are the names of the pieces and of the family, once? The family name is
greenlift (ADR-0076), so this settles the names of any piece that becomes its
own tool, the crate names, the 37 repositories' `relais.toml`, and whether the
tool rename ships in one window with `epic-vocabulary`'s migration (the hook
text and the branch prefix then change once). `blocked_by` cannot name
another programme, so read that programme's t01 answer by hand.

## Look at

- t02's Resolution (the pieces) and `docs/programmes/epic-vocabulary/t01-vocabulary.md`.
- `harness.bundle-names` (ADR-0076) and decisions#174 for the family set.
- The programmes `greenrack-rename` and `topo-spotter-rename` in decisions
  for what a rename touched and cost.
- The 37 repositories with a `relais.toml`, the MCP tool ids `mcp__relais__*`
  and homelab's references.

## Answer looks like

A table: each piece with its name, what reads the old name (crates, the
plugin, `relais.toml`, MCP ids, homelab) and the order of the rename. One line
on the window: with the vocabulary migration or apart from it, and why.
