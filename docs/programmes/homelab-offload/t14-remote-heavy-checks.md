+++
type = "research"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais", "amont"]
+++
# How a heavy check runs in a pod and returns its outcome and logs

## Question

Today the coordinator holds a lease by local process-group id and host slots are local flocks (`crates/relais/src/verify/heavy.rs`); a pod has neither. Heavy checks need no model credentials and no native-dispatch amendment. Research a result contract binding each verdict to the immutable tree, run/attempt, check profile and the actual execution environment (OS/architecture, toolchain, container image digest); how base and candidate checks stay comparable; how cache keys use remote provenance; which platform-specific checks must stay local. Report the decisions:ADR-0045 holdout boundary (where acceptance executes, which identity fetches holdouts, how worker and reviewer inputs exclude their sources and logs) before this Resolution closes. Align with t04's transport research.
