+++
type = "grilling"
mode = "hitl"
status = "open"
blocked_by = ["t02-remote-launcher", "t09-sandbox-floor", "t10-pod-denial-signals"]
repos = ["relais", "homelab"]
+++
# How the sandbox policy adapts from denials by itself

## Question

The old loop was manual (#164, #170: people edited rules after reading denials). Settle the mechanism, the rollback and the evidence threshold. The Resolution is a new decision record modelled on ADR-0003 (`learning.automatic-promotion`): a granted envelope, automatic rollback, anti-flap, and it never edits the floor that t09 sets.

## Update 2026-11-11

- t10's signal (Hubble DROPPED flows under default-deny egress) is only real
  where egress is enforced. The clusterwide `dns-visibility` policy allowed
  everything until the egress-enforcement programme (homelab repo) began
  enforcing namespaces one by one on 2026-11-11. `amont-gates` is cleared but
  not enforced yet. Until it is, the loop would read no denials.
- t26 item 3 amended t09's floor: detached pods may reach the apiserver.
- The loop is premature until a worker runs remotely at all (t02).

## Look at

- t09's Resolution and t26 item 3.
- t10's Resolution.
- The egress-enforcement programme in the homelab repository, tickets for
  `amont-gates`.
- decisions ADR-0003.

## Answer looks like

The mechanism (what is observed, what may change, who approves), the evidence
threshold, the rollback rule, the anti-flap rule, and the line that proves the
floor is never edited. The decision record is linked from the Resolution.
