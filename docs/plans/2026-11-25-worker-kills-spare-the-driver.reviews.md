# Full reviews: Keep a worker's process kills off its driver

Plan: [2026-11-25-worker-kills-spare-the-driver.md](2026-11-25-worker-kills-spare-the-driver.md).
All panel seats ran on opus-5-5; Codex on gpt-6-astra.

## Round 1

- **backend** (approve-with-changes, 44k, 76 s): 1 high, release phase (plugin embedded by `build.rs`, cached by version), accepted as Phase 2; 2 medium, seat unix, accepted; 3 low, name the prompt assertion, accepted; 4 low, the silent lexical fallback, obsolete after unix 2.
- **lang:rust** (approve-with-changes, 37k, 62 s): 1 medium, name both pins (`runner/tests/mod.rs:7715`, `:8191`) and the 80-column gate, accepted; 2 medium, the pid source and pid-by-pattern, accepted through tui 1.
- **lang:typescript** (approve-with-changes, 40k, 55 s): 1 medium, component-wise prefix match, obsolete; 2 medium, no `node:` import, obsolete; 3 low, test through the run tool with scripted stdout, accepted; 4 low, pin location, accepted.
- **tui** (approve-with-changes, 28k, 43 s): 1 high, the rule must forbid `pgrep -f` / `ps | grep` and name `$!`, accepted; 2 low, a real assertion, accepted.
- **unix** (approve-with-changes, 33k, 55 s): 1 medium, verify with `pgrep -fl`, not `ps`, accepted; 2 medium, compute the relative path from physical directories (`native contract --relative`), accepted as option (a): the plugin and binary are versioned together; 3 low, record `$!`, never `kill 0` / `kill -1`, accepted.

## Round 2

- **backend** (approve-with-changes, 53k, 26 s): all round-1 findings resolved. Carried: a symlink-cwd test case (medium); drop the stale "subdirectory cwd" from the guidance list; "the flag cannot be missing" overstates it (an older relais on PATH refuses it loudly); the pilot should pass `--relative`.
- **lang:rust** (approve-with-changes, 48k, 85 s): resolved. Carried: join with `/` on every platform, because Windows `display()` prints `\` (medium); a `#[cfg(unix)]` symlink case.
- **lang:typescript** (approve, 47k, 33 s): resolved. Carried: the reply says the path is relative to the run's cwd; a string absolute path stays a gap the guidance covers.
- **tui** (approve, 33k, 20 s): resolved, no new findings.
- **unix** (approve, 39k, 25 s): resolved. Carried: the same two text fixes as backend.

## Codex (gpt-6-astra, high effort)

approve-with-changes. Carried: 1 medium, add the `full-ci` label so that macOS, Ubuntu and Windows all run; 2 medium, the pilot identifies the live driver's pid and checks that pid against the pattern.
