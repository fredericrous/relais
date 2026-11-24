# Full reviews: relais reads the greentopo key

Round 1 (backend, lang:rust, tui, unix): approve-with-changes each. Findings
accepted: adds=contract, rollout/rollback gate, observable verification,
wire struct `deny_unknown_fields`, hash and JSON round-trip tests, dedicated
both-keys error, blocker text naming both keys, a follow-up that spawns
`greentopo`. Rejected: unix, `relais init` writes `greentopo` (an older relais
refuses it; init keeps `aval` until t10 closes).

Round 2 (all six seats; architect and po seated by adds=contract):
approve-with-changes. Accepted: raw-table pre-check in `from_toml_str`,
blocker wording with `bin`, tooling test with a no-such binary, Phase 3
release and hand-off with fleet repository list, t11 before t10, removal of
the `aval` key on t10's list, deferred field rename moves every authority
hash. Rejected: architect's separate Phase 0 (folded into Phase 3).

Final: rust approve; backend approve-with-changes, its blocker (declare
`decisions`) fixed; one low finding carried into implementation: Phase 3's
verification must cover all four decisions edits (t09 and t10 `blocked_by`,
t10's fallback list, the map's Handoff order).
