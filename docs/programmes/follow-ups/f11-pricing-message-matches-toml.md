+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
+++
# The unpriced-model message names the setting the way it is written

## Question

`relais report` says `no [pricing.models] entry for:` and the runner says
`<model> has no [pricing.models] entry`, while the entry a person adds to
machine.toml is an array of tables written `[[pricing.models]]`
(`docs/INTEGRATIONS.md:286`). Should the messages print the double-bracket
form, or say "a `[[pricing.models]]` entry in machine.toml", so a person
can paste what they read?

## Look at

- `crates/relais/src/router/report.rs` (the two `[pricing.models]` lines),
  `crates/relais/src/orchestration/pricing.rs:187`,
  `crates/relais/src/native_pricing.rs:177`, their render tests.

## Answer looks like

One wording across the three sites, matching the TOML, with the tests
updated, merged.
