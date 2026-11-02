---
status: active
branch: feat/holdouts-from-store
repos: [relais]
adrs: [ADR-0045]
---
# Plan: holdouts declare themselves in the store

## Review panel

👉 **Decide:** none open. Your trust rule is in: automatic while a holdout's fingerprint matches its confirmation, and any change asks you once before it runs.
📍 relais · plan reviewed, not started · next: Phase 1 (store discovery, defaults, confirm-on-change). Panel: backend, lang:rust, unix (opus-5-5), codex (gpt-6-astra).
**Changed by review:**
- one store commit per run, a race-free cache, and a 30 s bound with the unreachable store shown;
- the fingerprint covers the whole declaration, env and contents, and any change asks;
- a confirmation binds to the reviewed sha, and symlinks are refused.

📄 Full reviews: [2026-10-31-holdouts-declare-themselves-in-store.reviews.md](2026-10-31-holdouts-declare-themselves-in-store.reviews.md)
**Verdicts:** backend approve on the final body; lang:rust, unix and codex approve-with-changes; none rework. 7 findings carried into implementation.

## Context

A holdout today is declared entirely in the person's `~/.config/relais/machine.toml`:
- `[holdouts] store, ref`;
- plus, for each holdout, a `[holdouts.<repo>.<name>]` table with `path`, `overlay`, `command {argv, junit, timeout_seconds}`, `inputs` and `attach`.

That is six fields per holdout, kept by hand in step with a script that lives in another repository (the store). Every change is a `!` command the person types, because the relais plugin refuses agent edits of machine.toml. On 2026-10-31 one such paste lost its `/**` globs to Markdown, and both holdouts silently stopped attaching.

The person asked for developer experience: "the more automatic things are and the less we have to declare in machine.toml the better".

Facts this plan builds on:
- Preflight takes only the store's commit sha (`holdout::ls_remote`, holdout.rs:698). The store's files are fetched at verification (`holdout::fetch`, holdout.rs:778).
- The whole `HoldoutDecl` is frozen per run in `holdout_definitions` (ledger/holdout_definitions.rs:18).
- No field has a default (`HoldoutDecl`, policy/mod.rs:1050).
- Holdout commands are trusted only because machine.toml is the person's (policy/mod.rs:996, SPEC §4 :143). No grant covers them.

## Goal

machine.toml names only the store. Each holdout's declaration lives in the store beside its script, every field has a default, and adding a holdout is a store commit with no machine.toml edit.

## Non-goals

- No change to how a holdout runs, settles, is frozen, is hidden from the worker, or attaches (ADR-0045 and the widen plan's Phase 1 behaviour stay as they are).
- No holdout declaration in a repository's `relais.toml`: that is the worker's tree.
- No new store protocol: the store stays a plain git repository fetched over the existing BatchMode ssh path.

## Behaviour

**machine.toml** needs only:

```toml
[holdouts]
store = "ssh://git@git.daddyshome.fr/fredericrous/holdouts.git"
ref = "main"
```

**In the store,** a holdout is a directory `<repo>/<name>/`. Here `<repo>` is the repository's main checkout name, `main_checkout_name`, the key used today. The directory holds:
- `check.sh`, the script (any name, through `path`);
- an optional `holdout.toml`, whose fields all have defaults:

```toml
# relais/report-values/holdout.toml (every key optional)
timeout_seconds = 1200
attach = { task_types = ["feature", "review-fix"], paths = ["crates/relais/src/report/**", "crates/relais/src/ledger/**"] }
inputs = ["Cargo.toml", "crates/relais/Cargo.toml"]
```

**Defaults:**

| field | default |
|---|---|
| `path` | `check.sh`, relative to the holdout's directory |
| `overlay` | `.relais-holdout/<name>.sh`, a path no repository uses. The existing refusal `holdout_overlay_exists` still applies. |
| `command.argv` | `["sh", <overlay>]` |
| `command.junit` | `target/holdout/<name>-junit.xml` |
| `timeout_seconds` | 1200 |

- **`RELAIS_HOLDOUT_JUNIT`:** the command also receives the JUnit path as this variable: an absolute path inside the candidate copy, whose parent directory relais creates before the command runs. A script need not hard-code the path.
- **`timeout_seconds`:** the same key in both sources, top-level in `holdout.toml`. The 1200 default also applies to a machine.toml table that sets none: holdouts build the candidate, so the generic 300 s is too short (the pilot's lesson).
- **Paths:** `path`, `overlay` and `config` must pass `clean_relative`. Discovery validates Git entry modes before reading: reject symlinks in repository/holdout directories, holdout.toml, script/config paths and recursively included overlay contents, including intermediate components. Read and fingerprint only material belonging to the pinned store tree. Verification applies the same checks and verifies supplied contents against the confirmed fingerprint before execution. Test external and cyclic symlinks without reading their targets, and a directory overlay whose nested file changes.
- `config`, `inputs` and `attach` default to absent, as today.

**Discovery at preflight:**
1. One `ls_remote` per preflight. That sha is the one fetched, read and frozen for every holdout of the run.
2. relais fetches the store at that sha into a cache keyed by sha (`<state>/holdouts/cache/<sha>`), reusing `holdout::fetch`'s shallow fetch:
   - each fetch builds in its own `<sha>.partial.<run id>` and renames it into place;
   - a rename that finds `<sha>` already there counts as a cache hit (another session won), so concurrent preflights never delete each other's tree;
   - the cache is pruned like the run holdout directories (entries other than the 5 most recent shas).
3. Discovery as a whole, `ls_remote` included, is bounded by 30 s, the current `ls_remote` bound. A slow store therefore no longer adds minutes to preflight.
4. It reads `<repo>/*/holdout.toml` and every `<repo>/*/check.sh`.

**Merging.** The declarations found are merged with machine.toml's into one effective `HoldoutSettings.repos` before `Lookup` and `attachments` are built (preflight.rs:557, :591). So `resolve`, `resolve_inheriting` and attachment all see them, for names a contract gives and for attachment alike.
- **machine.toml wins.** A `[holdouts.<repo>.<name>]` table there replaces the store's declaration for that name, so the current machine.toml keeps working unchanged.
- **Turning one off.** `{ enabled = false }` alone turns off a holdout the store declares. It is a separate variant of the override table; `enabled` and `source` are never fields of the serialized `HoldoutDecl`.
- **Invalid declaration.** A `holdout.toml` that does not parse refuses the run naming the file (`holdout_declaration_invalid`, exit 3 like the other holdout refusals).

**Store unreachable or past the bound.** No declaration can be read, so:
- a holdout the contract names is refused with `holdout_unreadable`, not `holdout_unknown`;
- since which holdouts would attach is unknown, the run records `holdout_store_unreachable` (store, cause), and `relais plan` and `relais doctor` show it. An attachment is never lost silently.
- A successor of such a run skips definition-change detection.

**Freezing:** the resolved declaration, store-held or machine.toml-held, is frozen per run as today.
- `digest_of` adds the declaration's source (store or machine) and the effective `timeout_seconds`, `inputs` and `attach`. A store sha already changes whenever a store file changes.
- The digest carries a version. A row frozen before this change is compared with the old formula, so after the upgrade no successor reports a spurious definition change.

**Visibility:**
- `relais doctor` lists the holdouts discovered for the current repository: name, source (store or machine), its attach rule, and its confirmation state.
- `relais plan` shows what would attach, as today.

**Trust: confirm on change** (the person's rule). A store holdout's command is text from the store, so what it runs is confirmed by the person, not assumed.
- **Fingerprint:** relais computes a versioned fingerprint of the complete effective HoldoutDecl, including every CommandSpec field such as env, plus the contents supplied through path and config. Directory overlays are hashed recursively in deterministic path order, including file paths, modes and bytes. Confirmation covers this exact declaration and supplied contents.
- **Confirmed list:** the confirmed fingerprints are kept per store URL and holdout name in a new ledger table `holdout_confirmations`, with its migration.
- **Unchanged since confirmed:** automatic, nothing is asked.
- **New or changed:** this covers any change, whoever pushed it, since git cannot tell the author reliably. An attached holdout is skipped and counted as skipped, so ordinary work is not blocked. A named holdout refuses the run with `holdout_unconfirmed`. Either way the event `holdout_unconfirmed` names the holdout.
  - The person confirms with `relais trust holdouts` through the plugin's dialog. Review shows the complete effective declaration and all supplied-content diffs against a retained confirmed snapshot. Acceptance must supply the exact reviewed store SHA and fingerprint; it refuses a stale request and never substitutes newly fetched contents. Persist the reviewed snapshot and fingerprint atomically, independently of cache eviction. Test a store update between display and acceptance: the unseen version receives no confirmation and cannot run.
  - A changed command never runs before it is confirmed.
- **Declaration changes:** any change to the effective declaration requires confirmation, including attach, timeout_seconds, command.env and additions or removals in inputs. There is no data-only exemption.
- **machine.toml tables:** these are the person's own text, so they need no confirmation, as today.

## Phases

- [ ] Phase 1 — relais (built and reviewed 2026-11-02; only the release is left):
  - store discovery at preflight: one sha, the cached shallow fetch, the 30 s bound;
  - `holdout.toml` with the defaults above, and `RELAIS_HOLDOUT_JUNIT`;
  - machine.toml override and `enabled = false`;
  - `holdout_declaration_invalid`;
  - the `digest_of` additions;
  - confirm-on-change: `holdout_confirmations`, `holdout_unconfirmed`, `relais trust holdouts` and the plugin dialog;
  - the `relais doctor` listing;
  - SPEC §4, §5 and §10.
  
  Release.
- [ ] Phase 2 — holdouts store: add `holdout.toml` for `report-json` and `report-values` (each script reads `RELAIS_HOLDOUT_JUNIT` when set), through a Forgejo PR.
- [ ] Phase 3 — the person confirms both store holdouts once, then reduces machine.toml's `[holdouts]` to `store` and `ref`: one `!` command, given in a code span, with a backup. `relais plan` then shows both holdouts attached from the store, and `relais doctor` lists them with source `store`.
- [ ] Phase 4 — the widen plan's Phase 4 observation continues across this change. Its counts do not reset, because attachment behaviour is unchanged.

## Decision log

- 2026-10-31 — Declarations move into the store, not into a second machine-wide file, because the script and its declaration change together and belong in one commit. The person: "the less we have to declare in machine.toml the better devx".
- 2026-10-31 — Trust is confirm on change, not warn: the person asked, "when we hit that case (I mean toml changes coz of someone else) we get ask for confirmation otherwise it is automatic?" Any change asks, because git cannot tell who pushed. A changed command never runs before it is confirmed.
- 2026-10-31 — machine.toml tables keep precedence, so nothing breaks before Phase 3, and a person can still override a store holdout locally.
- 2026-11-01/02 — Phase 1 was built in five relais runs (A1, A2, B1, B2, C). Each candidate was finished by hand, with no paid second round (the cost-saving rule):
  - A1 (budget_exhausted → verify starved): clippy `large_enum_variant` (boxed `HoldoutTable::Declared`); the test-only `fetch_count` moved to `test_support`, because `check-module-cycles` allows one trailing `mod tests`; the store fixture sets `core.hooksPath`, because the machine's shellcheck hook judged the fixture's scripts.
  - A2 (budget_exhausted): applied as-is.
  - B1 (verification gap): one clippy `type_complexity` fix in a test.
  - B2 and C (scope_exceeded): the overflows were the `HoldoutUnconfirmed` BlockCode in `policy/mod.rs` (4 lines) and `UNCONFIRMED_OUTCOME` in `preflight.rs` (1 line). Both are required by this plan; the contracts left those files out.
- 2026-11-02 — On the rebase onto main, A1's store fetch at preflight turned a 4 s total-budget test into `holdout_unfetchable`. A fetch cut short by a spent budget now ends on that budget (`runner/holdouts.rs`, ADR-0010).
- 2026-11-02 — Implementation review, round 1, found two trust holes, both fixed:
  - The plugin dialog cut a review at 40 lines but still offered Confirm. A review too long for the dialog now offers only Not now and sends the person to `relais trust holdouts` in a terminal.
  - The model could confirm a holdout itself in Bash. The plugin guard now refuses `relais trust holdouts --accept`, and `holdout_confirmations.confirmed_via` records `plugin` or `cli` (ledger v48 is unreleased, so the column is part of its migration).
- 2026-11-02 — The Delta found that the first fix moved long reviews to the terminal, which printed store text unescaped. An escape sequence or `\r` in a script could redraw lines the person reads before accepting. `relais trust holdouts` now renders each review through `trust::holdout_review_lines`, which escapes every store-supplied line with `escape_display`, the escaping the plugin dialog uses. Test: `a_review_escapes_control_characters_in_store_text`. The renderer lives in `trust`, not in `holdout`: `holdout` calling `trust` closes a module cycle.
- 2026-11-02 — "A successor of such a run skips definition-change detection" was built as: a successor whose own store is unreachable skips the comparison (`a_successor_whose_store_is_unreachable_reports_no_definition_change`). The opposite case (a predecessor whose store was unreachable) freezes no store holdout, so there is nothing to compare.

## Verification

- Phase 1:
  - Unit tests:
    - each default (path, overlay, argv, junit, timeout from both sources);
    - a `holdout.toml` with every key, one with none, and a `path` that fails `clean_relative`;
    - an invalid `holdout.toml` refuses the run with `holdout_declaration_invalid` naming the file;
    - a machine.toml table overrides a store declaration, and `{ enabled = false }` removes it;
    - `digest_of` changes when the declaration's source or timeout changes;
    - a definition frozen before the change compares equal under the old formula.
  - Runner tests with a local store repository:
    - a holdout found only in the store attaches and runs, writing its JUnit to the absolute `RELAIS_HOLDOUT_JUNIT` with no hard-coded path;
    - one preflight resolves the store sha once and freezes every holdout at it;
    - the fetch count, through a counting seam: two preflights at one sha fetch once, and four started concurrently all succeed, leaving one cache directory and no `.partial`;
    - an unreachable store: discovery ends within 30 s, a named holdout is refused with `holdout_unreadable`, and `holdout_store_unreachable` is recorded;
    - a successor of a run frozen before the upgrade reports 0 definition changes;
    - confirmation:
      - a new store holdout is skipped when attached, or refused when named, with `holdout_unconfirmed`, and does not run;
      - after `relais trust holdouts` confirms it, it runs;
      - a changed script asks again;
      - changes to attach alone, timeout_seconds, command.env, or any other effective declaration field require confirmation and cannot execute before it;
      - a store commit pushed between `relais trust holdouts` displaying a holdout and the acceptance: the acceptance (carrying the displayed sha and fingerprint) is refused as stale, the new version records `holdout_unconfirmed` and no `holdout_settled`;
      - a confirmed snapshot survives eviction of its sha from the cache (6 newer shas fetched), and the review still diffs against it;
      - a directory overlay whose nested file changes asks again;
      - supplied contents altered after confirmation, before verification: execution is refused before the command starts.
    - symlinks: a store whose `&lt;repo&gt;/&lt;name&gt;` directory, `holdout.toml`, `check.sh`, config, an intermediate path component, or a nested overlay file is a symlink (external target, and a cycle) is refused at discovery and at verification, with no read of the target.
  - `relais doctor` on an isolated config with a local store lists the discovered holdouts with their source and confirmation state.
  - The run on this change records `holdout_settled` for `report-json` with exit 0, and a JUnit file with 0 failures.
- Phase 1, observed on 2026-11-02 (input → expected → actual):
  - Each item above was run by name on the tree being pushed, the rebased branch with the review fixes. Every one prints `... ok`. The full suite also passes: 2510 passed, 0 failed.
    - Defaults: `a_store_holdout_without_a_toml_takes_every_default`.
    - A toml with every key: `a_store_holdout_toml_with_every_key_is_read`.
    - A toml with none: `a_store_holdout_toml_with_no_key_is_all_defaults`.
    - An unclean path: `a_store_path_that_is_not_clean_is_refused_naming_the_file`.
    - An invalid toml: `an_invalid_store_holdout_toml_refuses_the_run_naming_the_file`.
    - The machine.toml override and `enabled = false`: `a_machine_table_replaces_the_store_declaration_and_enabled_false_removes_it`.
    - The digest: `the_digest_changes_with_the_source_the_timeout_the_inputs_and_the_attach`, `the_legacy_formula_ignores_source_timeout_inputs_and_attach`, `a_definition_frozen_without_a_digest_version_compares_under_the_old_formula`.
    - A store-only holdout attaches and writes its JUnit: `a_holdout_only_the_store_declares_attaches_runs_and_writes_to_the_given_report_path`.
    - One store sha per preflight: `one_preflight_asks_the_store_once_and_freezes_every_holdout_at_that_commit`.
    - The fetch count and concurrency: `preflights_at_one_commit_fetch_the_store_once_and_concurrent_ones_leave_one_tree`.
    - An unreachable store: `an_unreachable_store_ends_discovery_within_the_bound_and_refuses_a_named_holdout_as_unreadable`.
    - Successors: `a_successor_of_a_run_frozen_before_digest_versions_reports_no_definition_change`, `a_successor_whose_store_is_unreachable_reports_no_definition_change`.
    - Confirmation: `a_new_store_holdout_is_skipped_when_attached_refused_when_named_and_runs_once_confirmed`, `any_change_to_a_confirmed_store_holdout_asks_again_before_it_can_execute`, `a_store_commit_between_display_and_acceptance_is_refused_as_stale_and_the_new_version_cannot_run`, `a_confirmed_snapshot_survives_cache_eviction_and_the_review_still_diffs_against_it`.
    - Overlays and supplied contents: `a_directory_overlay_hashes_nested_bytes_and_modes_in_path_order`, `supplied_contents_altered_after_preflight_refuse_execution_before_the_command_starts`.
    - Symlinks: `a_store_symlink_anywhere_in_a_holdout_is_refused_at_discovery_unread`, `a_symlink_anywhere_in_the_supplied_contents_is_refused_unread`.
    - Keys and shadowing: `a_confirmation_is_keyed_by_store_repository_and_name`, `the_review_lists_a_store_holdout_a_machine_table_shadows_as_shadowed`.
    - Listings: `doctor_lists_each_discovered_holdout_with_its_source_and_confirmation_state`, `plan_json_marks_an_attached_store_holdout_that_is_not_confirmed`, `the_report_counts_an_unconfirmed_skip_apart_from_other_skips`.
    - Review fixes: the consent tests `a review too long for the dialog offers no confirmation` and `the guard keeps the model from confirming a store holdout itself`, both passing under `make plugin`; and `a_confirmation_records_who_asked_the_person`.
  - Pilot of the branch binary, with isolated `HOME`/`RELAIS_CONFIG_DIR`/`RELAIS_STATE_DIR` and a local store declaring `demo/report-json` (holdout.toml with attach):
    - `relais trust holdouts` → `new`, with sha, fingerprint, declaration and diff. Actual: as expected.
    - `--accept` with a wrong fingerprint → refused as stale, exit 3. Actual: as expected.
    - `--accept` with the printed sha and fingerprint → `confirmed at <sha>`. Actual: as expected.
    - `relais doctor` → `holdouts report-json source store confirmed`. Actual: as expected.
    - The store script changed → `changed`, in the review and in doctor. Actual: as expected.
    - `relais plan --json` on a feature contract touching `src/**` → attached with `"unconfirmed": true, "confirmation": "changed"`, and the text says it is skipped until accepted. Actual: as expected.
  - "The run on this change records `holdout_settled` for `report-json`": not met by a relais run. A2 skipped both attached holdouts because its budget was spent, and B2 and C stopped on scope before verification. Instead, both store holdouts were run by hand on the rebased branch tree: `report-json` passed 25/25, `report-values` 19/19, exit 0.
- Phase 3: the person's machine.toml has no per-holdout table. After the person confirms both holdouts once, `relais plan --json` on a `feature` contract touching `crates/relais/src/report/` shows `report-json` and `report-values` under `holdouts.attached`, and `relais doctor` lists both with source `store`, confirmed.

## Implementation review

- **Verdict:** approve, after round 1 (approve-with-changes), a Delta (approve-with-changes with one new blocker) and a tree bind (approve).
- **Fixed:**
  - Three trust holes: a dialog that cut the review short while still offering Confirm; the model able to confirm through Bash; store text printed unescaped in the terminal.
  - Five code-rule violations: a flag argument, a done-line on stdout, two out-parameters, and weak names.
  - The verification record, now per test on the pushed tree.
- **Deliberate:**
  - The candidate prints of the review and listings go to stdout, because they are the data the caller asked for.
  - Test-only statics and panics.
  - Std `Option`/`Result` wildcard arms.
- **Cost:** 131k + 147k + 154k tokens, 152 s + 49 s + 26 s.

## Outcome

(Filled when the plan closes.)

<!-- panel: repos=relais reviewers=backend,lang:rust,unix,codex body-sha=701e09aba747 -->
