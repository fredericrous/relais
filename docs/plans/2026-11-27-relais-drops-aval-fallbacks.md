---
status: active
branch: chore/t22-drop-aval-fallbacks
repos: [relais]
adrs: [ADR-0072]
plan-id: p-01M8D26RDJRFW4DD47J0GGWFZT
---
# relais drops the aval fallbacks

## Review panel
👉 **Decide:** none — approve if one key and one binary, with `aval =` refused by name, is the whole t22 relais slice.
📍 relais · 2 phases · 1 release · 2 steps for you · next: Phase 1. Panel: backend, lang:rust, tui, unix, architect, po (opus).
**Changed by review:** doctor/plan refusal exits pinned; `Integrations::entries()` + PATH seam; gate parses every org's `relais.toml`.
**Verdicts:** 6 approve-with-changes then backend, unix, architect approve; 4 carried; words 770 → 1221 (×1.59, mostly high/blocking fixes); remove 0, merge 0.
📄 Full reviews: [2026-11-27-relais-drops-aval-fallbacks.reviews.md](2026-11-27-relais-drops-aval-fallbacks.reviews.md)

## Picture

```
 relais.toml [integrations]          before            after
 --------------------------          ------            -----
 greentopo = ...  ----------->  Integrations.aval   Integrations.greentopo
 aval = ...       ----------->  Integrations.aval   refused: "rename the
                                                     key aval to greentopo"
 spawn, no bin    ----------->  greentopo on PATH?  greentopo
                                 else aval
```

One key and one binary: the old name is refused at parse, never read.

## Goal
decisions topo-spotter-rename t22, relais slice: the old name `aval` stops
being read or spawned. `[integrations] greentopo` is the only key for the
decision corpus, and relais only ever spawns `greentopo` (or a declared
`bin`). A repository that still writes `aval =` gets a clear refusal that
says to rename the line, never a silent read.

## Non-goals
- Renaming the internal types `AvalCli`, `AvalVerdict`, `aval_resolve*` and
  the many `aval_*` locals in `context/`. That would be a large diff that
  collides with other sessions' open work (an-t35, f73, f33), and nobody
  outside the crate sees them. Filed as a follow-up.
- Anything persisted and read back keeps its name: the block code
  `aval_tool_failure`, the context manifest's `tool_versions.aval`, receipt
  and ledger fields. Renaming them would stop old records from being read.
- The `checked-by.txt` fixture, CHANGELOG history, `docs/plans`,
  `docs/programmes`, `.adr.yaml`'s `aval 1.7` comment.
- `~/.config/relais/machine.toml`: `MachineSettings` has no integrations
  table, so no machine key changes. It is never read or edited here.

## Code review
- relais: backend, lang:rust, unix, tui

## Behaviour
- `[integrations] greentopo = …` (string or `{ mode, bin }`) parses as
  before. The policy serializes it under `greentopo`, so every authority
  hash moves once (expected by t22).
- `[integrations] aval = …`, in any TOML spelling (table, dotted
  `integrations.aval`, inline table), is refused by `RepoPolicy::from_toml_str`
  with its own error (a command that loads the policy, such as `relais
  plan`, exits 2 with `relais: …` on stderr; `relais doctor` shows it on
  its `relais.toml` FAIL line and exits 3):
  `[integrations] aval is no longer read: rename the key aval to greentopo
  (for example greentopo = "required")`.
  A typed parse that skips that entry point (JSON, a bare `toml::from_str`)
  fails on the unknown field, never drops it.
- The both-keys error (`BothTopoIntegrationKeys`) goes: there is one key.
- With no `bin` declared, relais spawns, probes and labels `greentopo`. No
  PATH lookup chooses between two names any more; an absent `greentopo` is
  "not installed" (only `greentopo` is looked for). `default_bin` is a pure
  key-to-binary map (`amont_agent` → `greenbelay`, else the key).
- Blocker, gap and doctor texts lose "(`aval` still works for now)" and
  "(greentopo or aval)"; doctor's missing name is `greentopo`. The missing
  blocker reads "install greentopo, or set [integrations] greentopo =
  \"optional\" in relais.toml"; the launch hint "check that `greentopo` on
  PATH runs, or set `bin` under [integrations] in relais.toml". A dangling
  or non-executable `greentopo` still blocks (ADR-0073).
- The trust declaration names the integration `greentopo`. Repositories
  trusted once (`[trust_repos]`) stay trusted; the changed name fires the
  commands-changed warning once per repository. A legacy exact grant
  (`[trust."<key>"]`) stops matching, as the hash moved, and must be
  granted again.

## Phases
- [x] Phase 1 — one key, one binary: `crates/relais/src/policy/mod.rs`,
  `crates/relais/src/tooling.rs`, `crates/relais/src/context/mod.rs`,
  `crates/relais/src/runner/preflight.rs`, `crates/relais/src/verify/mod.rs`,
  `crates/relais/src/doctor/mod.rs`, `crates/relais/src/trust.rs`,
  `crates/relais/src/cli/run.rs`, `crates/relais/src/cli/replay.rs`,
  `crates/relais/src/repo/detect.rs`, `crates/relais/src/route/candidate.rs`,
  `crates/relais/src/runner/tests/mod.rs`, `docs/SPEC.md`, `CHANGELOG.md`
  - rename `Integrations.aval` → `greentopo`; drop `IntegrationsWire`,
    put `deny_unknown_fields` on `Integrations`;
  - replace `sets_both_topo_keys` with a pre-parse retired-key check and a
    `PolicyError::RetiredAvalIntegrationKey` variant;
  - `default_bin` pure; remove `default_bin_in` and its PATH tests;
    `RunEngine::aval_program` → `greentopo_program` (declared `bin`, else
    `greentopo`); remove `AvalCli`'s `default_program` cache and
    `aval_resolve_in`'s `default_program` parameter; `confirmed_absent`
    checks `greentopo` alone;
  - `Integrations::entries()` (key, dependency) and `TOPO_KEY =
    "greentopo"` in `policy/mod.rs` replace the copied triples and every
    `"aval"` key string (`tooling.rs`, `trust.rs`, `doctor/mod.rs`,
    `context/mod.rs`, `preflight.rs`); a seam `probe_integrations_in(repo, path)`
    (through a `which_in(name, dirs, extensions)` that keeps `which`'s
    path-name branch and `is_file` test; `which` and `probe_integrations`
    pass the process PATH; a test: a declared relative `bin` still resolves)
    and a test on it: a tempdir PATH holding only `aval`, with `greentopo =
    "required"`, gives `IntegrationMissing` with no `aval` in its detail;
    no `std::env::set_var`; afterwards `"aval"` in
    `crates/relais/src` matches only persisted names and refusal fixtures;
  - `INIT_TEMPLATE` and every policy fixture write `greentopo =`;
  - tests: fixtures writing `aval =` become `greentopo =`; new tests for
    the refusal in three spellings and for the JSON unknown field; tests
    faking both binaries rewritten to show that `aval` alone on PATH is
    "not installed"; hash pins updated from the failing assertions, plus an assertion
    that the serialized `integrations` has key `greentopo` and no `aval`;
  - `docs/SPEC.md` (the fallback, both-keys and "`aval` still works"
    text; absence names `greentopo` alone);
  - CHANGELOG Unreleased, marked breaking.
- [ ] Phase 2 — release: `CHANGELOG.md`, `Cargo.toml`, `Cargo.lock`, after
  every repository's `relais.toml` on origin is free of an `aval` key
  (the Phase 1 build's `relais doctor`, with a scratch `RELAIS_STATE_DIR`,
  in every checkout: none refused; plus a TOML parse of `relais.toml` on
  every unarchived repository of the `fredericrous` and `greenharness`
  GitHub orgs and the Forgejo organisations (count per org in the report); a line grep misses the dotted and inline spellings); `greentopo` on PATH on
  every host that runs relais (this Mac; t11: no homelab pod installs it); `relais trust show --json` in every checkout,
  with the installed relais before the new one, lists the legacy exact
  grants (`granted: true`, no `repository_grant`) for the report; install
  on this Mac; after install, count refusals and greentopo-missing blocks
  in a week of runs (target 0).

### 🧑 Your steps
- `claude plugin update relais` — because a plugin reinstall is the person's
- `relais trust grant --reviewed-by <name>` in each repository the report lists as legacy-exact — because a trust grant is the person's

## Decision log
- 2026-11-27 — Keep the internal `Aval*` type names, because the rename is
  cosmetic and collides with three open branches; follow-up filed.
- 2026-11-27 — Refuse `aval =` with its own error before the typed parse
  (not as an unknown field), because the person must be told the fix.
- 2026-11-27 — Code review changed the texts the Behaviour quotes: the
  refusal says "rename the key aval to greentopo and keep its value" (a
  copied `"required"` example could harden an optional key), and "… and
  greentopo is already set: remove the aval line" when both are set; the
  missing-greentopo blocker shows "(binary …)" only for a declared `bin`;
  doctor's FAIL line names the fix; a dangling `greentopo` blocks the run
  as "found at <path> but it is not a runnable file". `context/guidance.rs`
  changed too (doc comments and the too-old gap text, which still said
  "(greentopo or aval)").
- 2026-11-27 — The re-grant list is empty: every granted checkout on this
  Mac holds a repository grant (`[trust_repos]`, ADR-0007), which the hash
  move does not touch; none is legacy-exact. This corrects the ticket's
  "every repository grants trust again".

## Verification
Observed 2026-11-27 on the branch build, every CLI row with a scratch
`RELAIS_STATE_DIR` in scratch repositories (`/tmp/t22-f2/pilot.sh`).

| Check | Input | Expected | Observed |
|---|---|---|---|
| hash | serialized fixture policy | `integrations` key `greentopo`, no `aval`; pins moved | `greentopo_is_the_serialized_integration_key` passes; five pins moved (own policy `93ca8f03…` → `bc14c309…`, v1 `62accc6a…` → `1f9f2b43…`, recipe and tiered pins) |
| suite | `greengate run` gates (fmt, clippy, tests) | pass, 0 failed | pre-commit gate on the fix commit: 31 checks passed (whole `cargo test`); clippy `--all-targets -D warnings` clean after the rebase |
| refusal (CLI) | `relais plan --task <valid task file>` in a scratch repo whose `relais.toml` says `aval = "required"` | exit 2, stderr contains `rename the key aval to greentopo`; with `greentopo =` it does not exit 2 | exit 2, stdout empty, stderr `relais: the repository policy …/relais.toml is invalid: [integrations] aval is no longer read: rename the key aval to greentopo and keep its value`; dotted spelling the same; both keys: `… and greentopo is already set: remove the aval line`; with `greentopo =`: exit 3 (missing trust grant), not 2 |
| refusal (doctor) | `relais doctor` in the same repo | exit 3, the rename message on the `relais.toml` FAIL line | exit 3, `✗ relais.toml  [integrations] aval is no longer read: rename the key aval to greentopo and keep its value` |
| JSON | `Integrations` from `{"aval":"required"}` | Err, unknown field `aval` | `an_aval_key_that_skips_the_toml_entry_point_is_an_unknown_field` passes (JSON and bare `toml::from_str`) |
| new key | `relais doctor` in a scratch repo with `greentopo = "required"` | `greentopo` found, version printed | exit 0, `✓ greentopo    required — `greentopo` greentopo 3.0.0` |
| no fallback | `relais doctor` with only `aval` on a scratch PATH | `greentopo` not installed; no line says `aval` | `✗ greentopo    required by [integrations] and `greentopo` is not on PATH; every run is blocked on it: install …`; the one `aval` in the output is the `aval` repository's URL in the trust line |
| broken install | dangling `greentopo` on a scratch PATH | blocks, not "not installed" | run probe: `a_dangling_greentopo_blocks_as_a_broken_install` ("found at … but it is not a runnable file"); spawn: `a_dangling_greentopo_is_a_tool_failure_not_absence`; doctor blocks but says "not on PATH" (deliberate, see review) |
| declared bin | `greentopo = { mode = "required", bin = "tools/greentopo" }` with that file present | no `IntegrationMissing` | doctor `✓ greentopo    required — `…/tools/greentopo` greentopo 3.0.0`; relative name: `a_relative_name_with_a_path_is_checked_as_it_is` |
| real repo | after install, `relais run` in one fleet repository with `[trust_repos]` | runs; commands-changed warning once, not on the second run | Phase 2 row, after the release |
| pins | `cargo test -p relais --lib install::` and `policy::tests` | 0 failed, refusal tests in three spellings run | 596 passed, 0 failed over `policy:: tooling:: context:: doctor:: verify:: trust:: install::`; `install::` needed no change (no skill or agent text named aval) |

## Implementation review
- implementation-review: approve-with-changes, then Delta approve-with-changes; guidance text and relative-bin test fixed; plan rows recorded here (81k+107k, 79+22 s).
- backend: approve-with-changes → approve; both-keys message and broken-install text fixed; deliberate: a declared dangling `bin` keeps the missing text, the spawn hint names it (75k+100k).
- lang:rust: approve-with-changes ×2; `default_bin`, Option hint, dead assertion fixed; deliberate: the `greentopo_also_set` field and the `then().flatten()` stay, follow-up (64k+87k).
- unix, tui: approve-with-changes ×2; JSON rename paragraph, fix hints, keep-its-value text fixed; deliberate: doctor's dangling-link wording differs from the run's, follow-up (57k+82k, 56k+82k).

<!-- panel: repos=relais adds=contract reviewers=backend,lang:rust,tui,unix,architect,po body-sha=7ac7ee9c2e09 -->
