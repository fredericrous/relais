# Design: holdout evidence, parse and preflight (ADR-0045, phase 2)

Scope: a contract can name a holdout criterion; relais validates it and resolves
the holdout's definition at preflight. Nothing fetches or runs a holdout yet:
a holdout criterion is an unsettled gap at verification, so a contract that
makes one mandatory cannot be accepted until phase 3.

## Types

`acceptance.rs`
- `Evidence::Holdout { name: String }`, serde `{"kind": "holdout", "name": "…"}`.
- `independent(&Evidence::Holdout { .. }) == true` (written apart from the change).
- Every exhaustive `match` on `Evidence` gets an arm. At verification it settles
  as a gap whose text names only the statement, never the store, path or
  overlay: "holdout `<name>` is not run by this relais (phase 3)".

`policy/mod.rs`, machine-owned only (never in `RepoPolicy`, never in any
authority hash, no flag or env var sets it):

```rust
/// `[holdouts]` in machine.toml (ADR-0045).
#[serde(deny_unknown_fields)]
pub struct HoldoutSettings {
    pub store: Option<String>,          // git URL
    #[serde(rename = "ref", default = "main")]
    pub git_ref: String,
    #[serde(flatten)] repos: BTreeMap<String /*repo dir name*/, BTreeMap<String /*holdout name*/, HoldoutDecl>>,
}
#[serde(deny_unknown_fields)]
pub struct HoldoutDecl {
    pub path: String,        // inside the store
    pub overlay: String,     // repo-relative, clean (same rule as `design`)
    pub command: CommandSpec // reuse; `junit` required for a holdout
    pub config: Option<String>, // runner config inside the store (e.g. nextest.toml)
}
```

TOML shape:

```toml
[holdouts]
store = "git+ssh://git@git.daddyshome.fr/fredericrous/holdouts.git"
ref = "main"
[holdouts.amont.json-escaping]
path = "amont/json-escaping.rs"
overlay = "crates/amont/tests/holdout_json_escaping.rs"
config = "amont/nextest.toml"
command = { argv = ["cargo", "nextest", "run", "-p", "amont", "--test", "holdout_json_escaping"], junit = "target/nextest/default/junit.xml" }
```

`MachineSettings.holdouts: HoldoutSettings`, `#[serde(default, skip_serializing_if = is_default)]`,
so a machine.toml without the table parses unchanged.

## Preflight (runner/preflight.rs, and `relais plan`)

New module `crates/relais/src/holdout.rs`:

```rust
pub struct ResolvedHoldout { name, repo, store, sha, path, overlay, command, config, digest }
pub fn resolve(settings: &HoldoutSettings, repo: &str, name: &str, git: &Git, repo_dir, base_sha) -> Result<ResolvedHoldout, HoldoutRefusal>;
pub enum HoldoutRefusal { Unknown{declared: Vec<String>}, NoJunit, OverlayExists{path, base_sha}, BadOverlayPath(String), Unreadable{store, git_ref, stderr_last_line} }
```

Order, for each holdout criterion, before any worker is paid for:
1. `holdout_unknown`: no `[holdouts.<repo>.<name>]`. Detail lists declared names and the machine.toml path.
2. `holdout_no_junit`: `command.junit` absent.
3. overlay path not clean (absolute, `.`/`..`): `holdout_unknown`'s sibling `holdout_bad_overlay`.
4. `holdout_overlay_exists`: `git cat-file -e <base_sha>:<overlay>` succeeds.
5. `holdout_unreadable`: `git ls-remote <store> <ref>` with env `GIT_TERMINAL_PROMPT=0`,
   `GIT_SSH_COMMAND="ssh -o BatchMode=yes"`, stdin `/dev/null`, a 30 s timeout
   (bounded spawn), resolving to exactly one sha. Detail: store, ref, git's last stderr line,
   and the command to retry.

The overlay MAY lie inside `write_scope` (no refusal). Phase 3 protects it.

Each refusal is a `BlockCode` refused exactly as `design_unreadable` is
(same `fail_preflight` path, same `relais plan` refusal, same exit code).

The digest: sha256 of a canonical JSON of `{repo, name, store, sha, path, overlay,
command.argv, command.junit, config}`. The manifest gains
`holdouts: Vec<HoldoutRecord { name, sha, digest }>`, `#[serde(default, skip_serializing_if = "Vec::is_empty")]`:
it records no store URL, path or overlay (the manifest is not shown to the
worker, but stay minimal).

## Worker context

The worker's context package and prompt carry a holdout criterion's
statement only. A test asserts that the built prompt contains none of the
store, path, overlay or config strings of a resolved holdout.

## Tests (tests/it, plus unit tables)

- Parse: `{"kind":"holdout","name":"x"}` round-trips; a contract with no holdout hashes unchanged.
- machine.toml: without `[holdouts]` parses; with it parses; unknown key refused.
- Preflight table: unknown, no junit, bad overlay, overlay exists at base, unreadable store
  (a `file://` path that does not exist) → each refused by its code before dispatch.
- Happy path: a local bare repo as the store (`file://…`) → manifest records name, sha, digest;
  overlay inside write_scope accepted.
- Prompt leak test above.
- Verification: a mandatory holdout criterion is an unsettled gap (not accepted).
