+++
type = "research"
mode = "afk"
status = "open"
blocked_by = ["t26-execution-service"]
repos = ["homelab", "amont", "relais"]
+++
# How a cold pod builds a Rust gate without a full rebuild

## Question

The Rust gates (`cargo-test`, clippy) of amont and relais are the gates that
hold host slots longest: on 2026-11-07 a land's `cargo-test` waited 1m53s
behind other Rust suites, and slot waits reached 12–18 min (amont 2.3.0's
plan, `docs/plans/2026-11-07-a-land-waits-for-a-slot-once.md` in amont).
They stay on the laptop today. t23 said `cargo-test` is macOS-only; that was
wrong (reopened 2026-11-11: CI passes it on Linux for both repos), so a Linux
pod or Coder workspace could run them, and would start cold every time. The cluster
has mirrors for npm, Go and images, but none for cargo (map notes). Which
cache makes a cold pod's Rust build cost close to an incremental one:

- a crates.io mirror or proxy (registry and index, so no build fetches from
  the internet under default-deny egress);
- sccache, with which backend (an in-cluster S3 such as Ceph RGW, a
  node-local disk on wk-3, a PVC), and how its hit rate is measured;
- a per-repo `target/` cache keyed by `Cargo.lock` and toolchain, carried
  as an Argo artifact or a volume.

It also asks what each one costs in disk and I/O on wk-3, given t05's
finding that OSD disk I/O is the binding limit.

## Look at

- t05's Resolution (capacity, I/O limit) and t26's (Argo lane, snapshots).
- t23's inventory: which Rust gates could ever be `platform=any`.
- t09's floor: a cache must not widen egress beyond a named mirror host.
- `crates/amont-runtime/src/host_slots.rs` in amont: why Rust gates are
  heavy and how long they hold a slot (`waits.jsonl` names each gate since
  amont 2.3.0).

## Answer looks like

A desk answer, with no cluster pod run. One row per option: what it caches,
where it runs, the egress it needs, and the disk and I/O it costs on wk-3.
Then one line: the option the first Rust slice uses, or that no cache is
needed yet and why.

A measured cold vs. warm build in a wk-3 pod is **not** part of this ticket.
It is needed only once a cache option is chosen, and it then becomes its own
ticket, with the chosen option as its question.
