# Reviews: the relais pane shows the session's programme


Round 1 (all approve-with-changes): lang:rust 37k/56 s (2 blocking, 1 high: claim `repo` is display-only; plain claim then /clear leaves nothing; tombstone race), tui 33k/52 s (1 high: narrow-width tail fallback), unix 33k/49 s (1 high: exit 2 is also a usage error), po 33k/61 s (1 high: after-release measure; ship-tail-first rejected), backend 41k/91 s, architect 51k/100 s (map logic stays in relais; `Store::claims_of`).

Round 2: architect, po, backend, unix, tui approve-with-changes; lang:rust approve. Carried into implementation:
- architect: drop the git-common-dir hop (a removed worktree has no common dir; order is `--dir`, claim `repo`, error); test `--dir` in another repo.
- po + backend: the After-release counts need one local journal line per read, freeze and hide; verify cited line numbers.
- backend: one `seq` sentence; exit 1 keeps the last view with a dim note; `--dir` in the Picture.
- unix: declare `--dir` in the flag list.
- unix + rust + backend: Verification expects `{"programme":null,"reason":"none"}`.
- rust: `expired` is best-effort, shown like `none`.
- tui: the `next:` line names `relais programme claim <slug> <id>`; the tail's status string goes first; source the 300 ms / 95% numbers.

Codex (approve-with-changes): highs 1 (state bound to session across clear/resume/fork) and 2 (late tail reads) applied; medium 3 carried (a claim naming a closed, out-of-scope or missing ticket renders its real standing or the error shape, without sweeping).
Backend delta on Codex edits: approve-with-changes, 54k/27 s; carried: freeze reads `liveTail` before the reset; an empty freeze keeps the frozen tail.

