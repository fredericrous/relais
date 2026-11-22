+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
due = "2026-11-29"
+++
# The question stratum's corrected rate a week after D34

## Question

On 2026-11-21 `main question d1-2 unknown` had 2 of 5 drawn strong outcomes
corrected (40%) against 7% undrawn; under label policy 2 it reads 12 drawn
(17%) and 94 undrawn (4%). A week later: what are its drawn and undrawn
counts and corrected shares, has the pooled stop rule fired, and does the
drawn arm's corrected share still exceed the undrawn arm's by more than the
15-point margin inside that stratum?

## Look at

- `relais report --since 2026-11-01 --no-import | awk '/^session routing \(/{p=1} /^learning:/{p=0} p'`
  (the exploration block), `router_exploration_stops`.

## Answer looks like

The stratum's two rows before and after, the stop rule's state, and one
sentence on whether exploration on main questions looks harmful at that
size; if the drawn share stays over the undrawn by 15 points at 20 drawn,
a ticket on ε for that stratum.
