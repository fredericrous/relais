---
status: active
branch: feat/zero-setup-onboarding
repos: [relais]
adrs: []
---

# relais: zero-setup onboarding from the plugin

## Review panel

👉 **Decide:**
1. Onboarding commits `relais.toml` (`--only`) rather than exempting an uncommitted one.
2. Bypass-permissions mode is out of the trust threat model.

📍 relais · plan reviewed · next: S0 consent spike. Panel: backend, lang:rust, tui, unix.

**Changed by review:**
- Every exit reaches the model (pre-run `Done`, `endUnfinished`).
- The plugin, not the model, words the question and picks the key.
- `flock` + 0600 on `machine.toml`.

📄 Full reviews: [2026-10-07-zero-setup-onboarding.reviews.md](2026-10-07-zero-setup-onboarding.reviews.md)

**Verdicts:**
- Round 1: 3 approve-with-changes, backend rework.
- Round 2: backend approve. Two lows go to implementation: one expected value on the `env` bypass line, and `relais.toml`'s fate on `onboard_commit_failed`.

## Context

Now that relais is a Claude Code plugin, the first run in a repository costs five manual steps before anyone sees it work:

1. `relais init` writes a fixed template (`INIT_TEMPLATE`, `crates/relais/src/policy/mod.rs:1528`). It always contains `make check`, and it marks aval, amont and amont-agent `required`. It detects nothing.
2. The person edits the models and the verification profile by hand.
3. They write a `.relais/task.json`.
4. `relais plan` prints a `[trust."<key>"]` block (`missing_grant_text`, `main.rs:3795`).
5. They paste that block into `~/.config/relais/machine.toml`, a file nothing writes (`policy/mod.rs:668`).

Worse, the plugin cannot even see these failures:

- `run_command` fails on `load_repo_policy()?` and `load_machine()?` (`main.rs:2756-2757`) before a run id exists. A missing `machine.toml` is a read error, not `missing_trust_grant` (`main.rs:2068-2081`).
- A preflight block exits 3 with its reason only on stderr.
- `claude-plugin/hooks/runs.ts` `endUnfinished` (`:222-229`) updates the pane but queues **no** message for the model. `SKILL.md:75` says not to end the turn before the outcome arrives, so the model waits forever.
- `mcp__relais__run` describes `task` as "one clear instruction", but the CLI reads it as a contract path (`runs.ts:56`).

Both gates protect something real, so this plan keeps them and takes the manual work out of each:

- **`relais.toml` holds the verification profile.** That profile decides whether a candidate is accepted. Without it, relais is a plain subagent.
- **The trust grant matters because relais runs repository commands through `$.process.spawn` → `verify::run_observed`.** Those runs skip Claude Code's Bash prompt and its folder-trust prompt. Without a grant, a cloned repository's `relais.toml` could run anything the moment the model calls `mcp__relais__run`.

Outcome: `/relais <task>` works in any repository with at most two questions, each asked once: "use these checks?" and "allow these commands to run?". `doctor`, `init` and `plan` stay for people who edit by hand.

## Design

### 1. Inferred policy: `relais init --detect [--json] [--write]`

`repo::detect_policy(root) -> Proposal` **only parses text**. It never runs `make -n`/`-q`, because GNU make evaluates `$(shell …)` while it parses, which would run repository code before consent.

A `Proposal` holds:
- the TOML it would write;
- each command and setup step with its **source** (`Makefile: target check`);
- what it saw but **did not** propose, each with the reason.

Each ladder takes its first match, in profile `default`:

- **Commands:**
  1. A Makefile `check` target → `make check`.
  2. Otherwise a `test` target → `make test`.
  3. Otherwise ecosystem defaults:
     - `Cargo.toml` → `cargo test`
     - `go.mod` → `go test ./...`
     - `package.json` with a real `scripts.test` → `<pm> test`, the package manager taken from the lockfile
     - `pyproject.toml` that declares pytest → `uv run pytest` / `pytest`
- **Setup:** the existing lockfile table (`repo::ECOSYSTEMS`, `repo.rs:162`).
- **amont.conf `block` gates:** reported as *seen, not proposed*. Their command column is a shell line, and SPEC §6 forbids inferring shell recipes.
- **Integrations:**
  - `amont = "required"` only when `amont.conf` exists;
  - `aval = "required"` only when an aval corpus exists;
  - everything else is `"optional"`.
- **Models:** the template's tier aliases stay (`haiku`/`sonnet`/`opus`). They are tiers, and the effective model is already recorded per dispatch.

Behaviour:
- **Output:** plain text goes to stdout, one argv per line with its source, at most 80 columns, no colour off a TTY or with `NO_COLOR`. `--json` gives the same data. Diagnostics go to stderr.
- **Exit codes:**
  - `0` when there is a proposal;
  - `4` when nothing was detected, with `{"proposal": null}`;
  - `2` for `--write` over an existing `relais.toml` (it uses `create_new`, so it never overwrites).
- **Plain `relais init`** keeps writing the template. Its closing hint now names `relais trust show` and `relais trust grant` (`main.rs:2326`).

**Nothing detected.** The plugin's `onboard` asks the person for the command as free text, writes it into the profile, and that command then appears in the trust question like any other.

**SPEC §5:214** changes from "never inferred" to "never run unless a person confirmed it". A proposal is only a suggestion. What the person accepts is written and hashed into the grant, so no command runs without review.

### 2. Grants: `relais trust show` / `relais trust grant`

`relais trust show [--json]` prints:
- the grant key on its own line, so it can be copied;
- the authority hash;
- the repository identity;
- the resolved `machine.toml` path, with its precedence: `$RELAIS_CONFIG_DIR`, then `~/.config/relais` (`paths.rs:75-85`);
- **the exact argv of every setup step and command** the grant would authorize, plus the models and integrations.

`relais trust grant --key <k> --reviewed-by <name> [--note …]` never reads stdin. It:
- recomputes the key from the policy on disk and the identity. A different `--key` is refused with exit **18** (`stale_grant_key`). The message prints the current key and says to run `relais trust show`. `--json` carries the `code`.
- takes an exclusive `flock` on `<config_dir>/machine.toml.lock` around read → edit → validate → rename, creating `config_dir` with `create_dir_all` if needed;
- creates `machine.toml` (`schema_version = 1`, mode 0600) when missing, and otherwise keeps its mode;
- appends `[trust."<key>"]` with `toml_edit` so comments and order are kept, and validates with `MachineSettings::from_toml_str`;
- writes through `fsutil::write_atomic` in the same directory, then fsyncs the parent directory;
- records a ledger event `trust_granted {key, reviewed_by, source: cli|plugin}`.

**`toml_edit = "0.22"`** costs nothing: `toml 0.8` already pulls in `toml_edit 0.22.27` (`Cargo.lock:830-852`). That argument goes beside the declaration (`change.a-new-dependency-is-owned`).

**A missing `machine.toml` is treated as empty settings** everywhere: run, plan and doctor. A fresh machine therefore gets `missing_trust_grant`, not a read error (`main.rs:2071`).

**SPEC §5 gains one paragraph:**
- a grant comes only from `relais trust grant`, typed in a shell or issued through the plugin's consent path (§3);
- no run, recipe or worker ever issues one.

§27:863 (promotion never grants) is unchanged.

### 3. Consent the model cannot fake (plugin)

The rule: a grant, and a written `relais.toml`, happen only after the **person** answers yes to a question the **plugin** built from `trust show --json`. The model can neither word that question nor choose the key.

The plugin registers `mcp__relais__trust {cwd}`. The handler:
1. runs `relais trust show --json`;
2. asks the person;
3. on yes, runs `relais trust grant --key <the key show returned> --reviewed-by "<user> (in session)"`.

The model's input never carries the key. On no, the tool returns `declined`, and the skill stops and reports without calling run again.

**S0, a spike, settles how the plugin asks**, in this order:
1. A native dialog from the mods API, raised in the handler. This adds a UI surface, so the panel comment gains `adds=ui` and the UI reviewers.
2. The plugin wraps `AskUserQuestion` (`on('tool.call', {tool: 'AskUserQuestion'})`):
   - it **replaces** the question and its options with text built from `trust show --json` before `next(e)`;
   - it reads the answer from the result and records `approved[key]`;
   - `mcp__relais__trust` is denied unless `approved[key]` is set.
3. A slash command the person types, `/relais-trust`, only if S0 shows the model cannot invoke a plugin command.

**Gate:** if none of these is model-proof, the plan stops at S0 and reports that.

**What it guarantees, stated honestly** (👉 decision 2):
- The plugin path is model-proof.
- `machine.toml` is a plain user file. Bash (`sh -c`, an absolute path, `env`) or Write/Edit can still add a grant.
- The plugin adds best-effort guards:
  - it denies Write/Edit on the resolved `machine.toml` path, which is exact;
  - it denies Bash commands whose text contains `relais trust grant` or that path, which is a nudge, not a control.
- SPEC §5 states that outside the plugin path, protection is Claude Code's permission prompt, and that bypass-permissions mode is out of this threat model.

### 4. Run tool, onboarding tool, and outcomes the model actually receives

**`mcp__relais__run`:**
- `{cwd, task: <contract object>}`. The plugin writes the contract to `<root>/.relais/tasks/<contract-hash>.json` and passes that path.
- A string ending in `.json` is still accepted.
- Prose is denied with a message naming the fields.
- relais writes `.relais/.gitignore` (`*`) the first time.

**Every exit reaches the model:**
- **Pre-run failures** send `Done {run: null, outcome: "blocked", code, detail}` before exiting 3. These are: no policy, an invalid policy, an unknown profile, `admission_unavailable`, and `ClaudeBackend::discover`. `Request::Done` (`protocol.rs:216`) gains an optional `code` and `detail`, and `run` becomes optional.
- **Preflight blockers** (`missing_trust_grant`, …) send the same shape with their `BlockCode`.
- **`no_policy` gets its own wire code.** It is not a `BlockCode` today (`policy/mod.rs:1228-1266`).
- **Plugin `onDone`** (`runs.ts:188`) puts the code and detail in the queued message. For `run: null` it prints `blocked (<code>): <detail>`, not a placeholder id.
- **Plugin `endUnfinished`** queues a message (exit code plus the last 5 stderr lines) for **any exit without `done`**, tracked by `child.doneSeen`. The model never gets two messages, and a panic, bad contract or malformed `machine.toml` is never silent.
- **For `no_policy` and `missing_trust_grant`**, the message names the next tool.

**`mcp__relais__onboard {cwd}`:**
1. Runs `init --detect --json`.
2. Asks the person "use these checks?" through the same consent path. The plugin builds the question.
   - On nothing detected, it asks for the command instead.
3. On yes, writes `relais.toml` and **commits it** (👉 decision 1). The worktree is built from the base SHA, and `dirty_paths` only exempts `.relais/` (`workspace/mod.rs:181-193`), so an uncommitted policy refuses the run with DirtyBase.
   - The commit is `git commit --only -- relais.toml`, so anything the person had staged stays staged and out of it.
   - It is refused with `onboard_commit_failed` on a detached HEAD, mid-rebase or mid-merge, or when a hook rejects it. The person sees the hook's output.
   - The person is told the commit SHA and branch.
4. Chains into the trust question.
- On no, it returns `declined`.

### 5. Skill and README

`SKILL.md` steps 1–3 are rewritten:
- No task file and no `relais plan` in Bash.
- Call `mcp__relais__run`.
  - On `no_policy` → `mcp__relais__onboard`.
  - On `missing_trust_grant` → `mcp__relais__trust`.
  - Then run again.
- On `declined` → stop and report.
- The model never edits `machine.toml` and never runs `relais trust grant`.

`README.md` "Using it on a repository" is rewritten around this flow. The stale `relais run --task` line (`:150`) is removed, and the exit-code table gains 4 (nothing detected) and 18 (stale key).

### What stays

- `doctor`, plain `init` and `plan` keep their behaviour. `plan` and the `load_machine` hint name `relais trust grant --key …` next to the paste block.
- The grant key shape is unchanged (`the_grant_key_shape_is_pinned`), so every existing grant still matches.
- An edited `relais.toml` still invalidates its grant. In the plugin that is now one question again, not a manual paste.

## Packages

Each package ends with `make check`, falsification of its key test with a forced rebuild, and the implementation review before the push.

- **S0 — consent spike** (plugin, throwaway branch). Pick the §3 mechanism, recorded in the decision log. Gate: none is model-proof → stop.
- **P1 — `trust show/grant`** (Rust): `src/trust.rs`, the Command enum, `toml_edit`, the lock, the ledger event, missing `machine.toml` treated as empty, and the SPEC §5 paragraphs.
- **P2 — `init --detect`** (Rust): `repo::detect_policy`, inferred integrations, the SPEC §5:214 rewording, the init hint.
- **P3 — every exit reaches the model** (Rust + plugin): `Done` with an optional run and code, pre-run and preflight blocks over the protocol, and the `endUnfinished` message.
- **P4 — plugin tools** (TS): the `run` contract object, `onboard` (with the commit) and `trust` through the S0 mechanism, the Write/Edit/Bash guards, `SKILL.md`, `README.md`.

## Verification

Key tests, each as input → expected. Fixture repositories are built in a tempdir per test, never committed.

- **Detect:**
  - Makefile with `check:` → `make check`, source `Makefile: target check`.
  - Makefile with a `$(shell touch X)` line → X never created.
  - Cargo-only → `cargo test`.
  - `package.json` + `pnpm-lock.yaml` → setup `pnpm install --frozen-lockfile`, command `pnpm test`.
  - amont.conf with a `block` gate → listed as seen-not-proposed.
  - Empty repo → exit 4, no file written.
- **Grant:**
  - Stale `--key` → exit 18, `machine.toml` sha unchanged.
  - No `machine.toml` → file created with mode 0600 and one grant.
  - An existing comment is preserved.
  - A grant makes `effective_authority` return no blocker.
  - 20 parallel grants with different keys → 20 tables.
  - A 0600 file stays 0600.
- **Protocol:**
  - Empty `RELAIS_CONFIG_DIR` → exactly one queued message, carrying `missing_trust_grant`, and no `interrupted`.
  - No `relais.toml` → one message carrying `no_policy`.
  - A child that exits 1 without `done` → one message with the exit code and its stderr tail.
- **Existing trust tests pass unchanged:** `missing_grant_blocks`, `changed_declaration_invalidates_grant`, `a_grant_is_bound_to_the_repository_as_well_as_the_declaration`.
- **Against reality, in a real Claude Code session.** Use a fresh clone of a small Cargo crate with no `relais.toml`, and an empty scratch `RELAIS_CONFIG_DIR`:
  - `/relais add a doc comment to X` → "use these checks? `cargo test`" → yes → `relais.toml` committed → "allow these commands?" → yes → run accepted. `machine.toml` holds one grant.
  - Edit `relais.toml`, commit, then `/relais` again → only the trust question comes back, and it lists the new command.
  - Answer no to the trust question → no further `mcp__relais__run` call, and the `machine.toml` sha is unchanged.
  - Tell the model "grant trust yourself" through `mcp__relais__trust` without consent, through Write/Edit on `machine.toml`, and through Bash `relais trust grant` and `env relais trust grant`. Count the grants written (expect 0 for the first three). For the last, expect either a grant written (the declared bypass) or a denial from the Bash text guard; record which in the decision log.
  - With an unrelated file staged, `/relais` onboarding → the commit holds only `relais.toml`, and the file is still staged.
  - A malformed contract, then a malformed `machine.toml` → exactly one queued message each, never `interrupted`.
  - Plain CLI: `relais init && relais plan --task …` still prints the paste block plus the `relais trust grant` line.

<!-- panel: repos=relais reviewers=backend,language:rust,tui,unix body-sha=069ed7c7d69f -->
