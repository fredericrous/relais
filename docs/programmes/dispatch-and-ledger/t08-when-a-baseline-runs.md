+++
type = "grilling"
mode = "hitl"
status = "open"
blocked_by = ["t01-verdict-consumers", "t03-attempt-ends-on-amont"]
repos = ["relais", "amont"]
+++
# When relais runs a baseline

## Question

Outside the relais repository, the baseline cache almost never hits (t02),
so each run pays a full check before the worker starts. Which rule replaces
it: skip it when greengate proves the base and otherwise run it up front;
run it only after a candidate goes red, to attribute the failure; or run it
alongside the worker under the host slot queue?

## Resolution
