+++
type = "research"
mode = "afk"
status = "out-of-scope"
blocked_by = []
repos = ["relais", "amont"]
+++
# What relais's documented interface to amont must specify

## Question

For someone running relais without amont, which commands, exit codes,
outputs and stamp or attestation formats must their replacement provide?

## Resolution

Out of scope, 2026-11-10: the person chose "Delegate, stay standalone".
Without amont, relais runs its checks itself, so there is no interface for
a user to implement.
