# Full reviews: relais programme commands read origin

**backend (opus)**: round 1 approve-with-changes (25k, 31 s): squash-merged branch counted as local work [high]; fetch kill/once per common dir [medium]; check test row ref [low]; mtime heuristic [low]; merged-branch test row [medium]. Round 2 approve-with-changes (31k, 30 s): all resolved; carried: SKILL.md step 1 reads the map with `git show <ref>:…` when list's 4th column is a ref [medium]; scope the Ctrl-C test's pgrep to its process group [low].

**lang:rust (opus)**: round 1 approve-with-changes (39k, 134 s): run_with_timeout SIGKILL leaves git locks [high]; commit-identity local work misses squash merges [high]; `status -z -uall` [medium]; `std::path::absolute` for a missing relative dir [medium]; deterministic dates + tie case [medium]. Round 2 approve-with-changes (43k, 20 s): all resolved; carried: SIGKILL through `child.kill()` [medium]; measure rev-list cost, optional cap [low]; assert the fetch runs before SIGINT [low].

**tui (opus)**: round 1 approve-with-changes (28k, 38 s): check --json paths [high]; warnings too [medium]; TTY progress line [medium]; failure reason + closing hint [medium]; 4th source column [low]. Round 2 approve-with-changes (34k, 20 s): all resolved; carried: pipe and drain fetch stdout/stderr [medium]; TTY line for the single-repo fetch [low].

**unix (opus)**: round 1 approve-with-changes (28k, 72 s): SIGINT orphans the fetch [medium]; GNU-parseable file:line [medium]; fetch-line ordering test [low]. Round 2 approve-with-changes (36k, 32 s): all resolved; carried: ssh ServerAliveInterval=5/CountMax=1 [low]; CHANGELOG names the 4th column [low].

**codex**: not run — usage limit until 2026-11-11 23:43.
