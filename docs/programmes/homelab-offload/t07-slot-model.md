+++
type = "research"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais", "amont"]
+++
# How remote capacity becomes a slot kind

## Question

`CapGroup::Remote` (`crates/relais/src/admission/mod.rs:101`) already means model-API work, so the name collides with remote execution. Host slots are local flock files (`amont-slots`) and the coordinator listens on a local Unix socket; the cluster needs its own source of truth for remote capacity, inside the amont slot ownership set by decisions:ADR-0051. A flock slot is freed when its holder dies; a remote slot is not, so this also settles a lease with a TTL and a heartbeat.
