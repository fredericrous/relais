+++
type = "grilling"
mode = "hitl"
status = "open"
blocked_by = ["t01-native-dispatch-amendment", "t05-cluster-capacity", "t07-slot-model", "t13-laptop-load-baseline"]
repos = ["relais"]
+++
# Which signal and rules choose laptop or cluster for each run

## Question

Which laptop-load signal decides, and which task types or repositories may leave the laptop. Also how the placement is shown: status prints laptop or cluster, the node, and the reason, as text rather than by colour alone.

## Update 2026-11-11

Placement is wider than laptop or cluster: a Coder workspace node exists as a
third place, and the programme agents-many-nodes (homelab repo) adds more
nodes later. Reword the choice as "laptop, the cluster lane, or a Coder
node". greengate has no load-based placement today: a gate goes to the
cluster whenever `executor cluster` is set, and the detached lane is chosen
only by `--cluster` or `amont.rehearse.cluster` (`detached.rs:56-58`). A
container's `getloadavg` also reads the host's load, not the pod's.

## Look at

- t13's table (when it is written) and t26 item 6.
- t05 (capacity) and t07 (slot model).
- `crates/greengate-runtime/src/cluster.rs` and `cluster/detached.rs` in the
  greengate repository (local checkout `amont`).

## Answer looks like

The signal (a named measure and a threshold), the task types or repositories
allowed to leave the laptop, the order of preference between the places, and
the exact status text that names the place and the reason.
