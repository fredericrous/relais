# Workers see the repository's rules and relevant decisions — reviews


**backend (opus-5-5).** Round 1: rework (42k tokens, 61 s). Blocking: reviewer agents have no Bash, so they cannot use the id index; give them full statements. High: advisory text counted in package_bytes; an older aval without `relevant` stops every run. Medium: latency not measured; aval optional-and-failing not specified; Phase 4 not observable. Low: brace globs; use a minor release. All accepted. Round 2: approve-with-changes (27k, 19 s). A [high] (the reviewer fallback named `aval rule`) was applied. Final body: approve (21k, 17 s).
Carried into implementation: B fetch the index once per run and give guidance its own 5 s timeout instead of PROBE_TIMEOUT 30 s, then update the Latency line; C the planner gets the relevant block only, so fix the wording at "worker and planner prompts that carry the index"; D dedupe the reviewer's full statements against the relevant rules by id; E name where the aval version comes from (`tooling::integration_version`) or drop the field; F state one reviewer budget rule (statements counted with the patch inside 192 KiB, patch trimmed first, or outside it) and whether the 16 KiB cap applies to the reviewer's relevant block.

**lang:rust (opus-5-5).** Round 1: approve-with-changes (24k, 22 s). Accepted: guidance sizing kept apart from SizingProblem; guidance failures are not ToolFailure; joined argv; default trait bodies; more fence-escape tests. Rejected: index cache (measured 26 ms and 165 ms); manifest-hash determinism (the hash is recorded as evidence and never compared).

**tui (sonnet-5-5).** Round 1: approve-with-changes (18k, 22 s). Accepted: relevant never blocks; full copy-pastable ids; the `aval rule` line only when the index is present; the reviewer needs statement text; a receipt gap line; a guidance budget; argv safety; ranked ids recorded.

**unix (sonnet-5-5).** Round 1: approve-with-changes (20k, 16 s). Accepted: joined argv and a 2 KiB objective truncation; non-zero exit (including 2) becomes a gap; relevant never blocks; a cancel test; `omitted` recorded. Rejected: non-UTF-8 paths (contract paths are already String).

Note: `amont-agent plan-review record` does not exist in the installed amont-agent, so these rounds are not in its ledger.
