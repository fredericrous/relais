# Full reviews: Attribute each home drift to its writer

Round 1 (opus, all four seats):
- backend approve-with-changes (43k, 78 s): 1 calls before writers (accepted); 2 drop cwd condition (rejected: transcript records carry `cwd`); 3 partial transcript reads (accepted); 4 status/explain check (accepted).
- lang:rust approve-with-changes (50k, 140 s): 1 settings.json hook point (resolved by dropping its writer); 2 cwd + regex (cwd rejected, no-regex accepted); 3 plugin JSON depth (accepted); 4 TranscriptBackend + zero dispatches (accepted); 5 serde shape (accepted).
- tui approve-with-changes (34k, 48 s): 1 one count line per writer; 2 cause on its own line; 3 follow-up names other sessions; 4 weaker unexplained printed apart + 80-column test (all accepted).
- unix approve-with-changes (35k, 61 s): 1 inside-repo by record cwd and common dir; 2 pilot in a scratch clone; 3 serde tag (all accepted).

Round 2:
- lang:rust approve-with-changes (58k, 28 s): 1 [high] read-only git forms counted as writes (accepted, applied); 2 cargo test line restored (accepted, applied with 1).
- backend approve-with-changes (50k, 23 s): 5 picture draws writers first; 6 "count lines" wording; 7 settings.json out of the pilot's jq check (carried into implementation).
- tui approve (40k, 19 s): picture order, "count lines" (carried).
- unix approve-with-changes (43k, 28 s): 1 cargo test line (resolved by rust 2); 2 one line per drift for grep (carried; conflicts with tui 2, decided in implementation by keeping one line when it fits 80 columns).
- Final-body rebind: rust approve (60k, 9 s), tui approve (42k, 13 s), backend approve (53k, 20 s; 8 git 2.46 `config set/unset/rename-section/remove-section`, carried), unix approve (45k, 14 s; same git 2.46 forms, carried).

Codex (gpt-6-astra, high effort) approve-with-changes:
1. [high] A path mention is not write evidence (accepted, applied).
2. [high] Writer hid a simultaneous mode change (accepted, applied).
3. [high] An agent transcript does not identify the dispatch (accepted; the record names the agent instead of using per-dispatch byte offsets).

Delta on the Codex edits (carried into implementation, no body change):
- tui approve-with-changes (46k, 13 s): print the full agent ID; one cause wording.
- unix approve (50k, 21 s): `$HOME/` and `${HOME}/` forms; `>|`, `&>`, `n>` redirections; one cause wording.
- backend approve (58k, 31 s): 9 count only `tool_use` records timestamped after the run's first snapshot, with a fixture.
- lang:rust approve-with-changes (65k, 32 s): a quote-aware word split by hand, no new dependency; `calls_read` defined per agent.
