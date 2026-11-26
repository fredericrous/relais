+++
type = "grilling"
mode = "hitl"
status = "open"
blocked_by = ["t01-coupling-map"]
repos = ["relais"]
+++
# Which crates relais splits into, and in what order

## Question

Which member crates does the workspace get, in what order are they cut, and
which of them, if any, become separate tools later? Does the split land before
or after the `epic-vocabulary` rename implementation? Both rewrite
`crates/relais/src/programme/` and `cli/programme.rs`, so one lands before the
other starts.

## Look at

- t01's Resolution: the coupling graph, the ties and the baseline.
- t03's Resolution once it closes (whether `admission`/`coordinator` stay).
- `docs/programmes/epic-vocabulary/map.md` and its t01.
- `harness.tool-boundaries` (ADR-0064): what a separate tool may reach.

## Answer looks like

An ordered list of crates, each with the modules it takes, the ties to cut
first, and the order against the rename implementation. For each crate one
line: stays inside the relais binary, or becomes its own tool later. The two
targets for the map's Destination, stated as numbers from t01's baseline.
