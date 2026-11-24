---
status: active
plan-id: p-01M86ZFN6VWAK4K34N1Y38SHPM
branch: feat/greentopo-integration-key
repos: [relais, decisions]
adrs: [ADR-0072]
---
# relais reads the greentopo key

## Review panel
👉 **Decide:** none — approve if one key-normalising parser change plus a release and a programme hand-off is the right size for t11.
📍 relais, decisions · 3 phases · 2 PRs · 2 releases · next: Phase 1. Panel: backend, lang:rust, tui, unix, po, architect (opus).
**Changed by review:** both-keys error needs a raw-table pre-check; blocker text and tests specified; Phase 3 adds release, repo list and sweep ordering.
**Verdicts:** 6 approve-with-changes, then rust approve; 1 carried (Phase 3 verification covers all four decisions edits); words 350 → 893 (×2.55, findings fixing wrong claims); remove 0, merge 0.
📄 Full reviews: [2026-11-24-relais-reads-greentopo-key.reviews.md](2026-11-24-relais-reads-greentopo-key.reviews.md)

## Picture

```
 relais.toml                       relais
 [integrations]                    -----
   aval = "required"   ---+
                             +--> Integrations.aval --> every reader
   greentopo = "required" -+       (both keys: refused)
```

Either key in `relais.toml` lands in the one `Integrations.aval` value, so no reader changes.

## Goal
`relais.toml` accepts `[integrations] greentopo = …` as the same dependency
that `[integrations] aval = …` declares, so the topo-spotter-rename fleet
sweep (decisions t10) can rename the key in each repository. Programme
ticket `topo-spotter-rename/t11-relais-aval-key`.

## Non-goals
- No change to the binary relais spawns (`aval`, the default at
  `verify/mod.rs:3558` and `context/mod.rs:531`). A `required` aval with no
  `aval` on PATH blocks every run, so a follow-up ticket that spawns
  `greentopo` is filed in the programme and added to t09's `blocked_by`.
  Until then a repository can write
  `greentopo = { mode = "required", bin = "greentopo" }`.
- No alias for the future: `aval` is read only until t10 closes; a relais
  release that removes the `aval` key is added to t10's list of fallback
  reads to remove (ADR-0072: no aliases).
- No change to `amont` or `amont_agent` keys.

## Code review
- relais: backend, lang:rust, unix, tui
- decisions: backend (programme Markdown with TOML front matter only; no code, so no lang or ui profile)

## Behaviour
- `[integrations] greentopo = "required"|"optional"|"off"` (or the inline
  table with `bin`) parses to the same `Integrations.aval` value as the
  `aval` key. Every reader (context, doctor, verify, trust, tooling) is
  unchanged, and the serialized policy, its authority hash and its trust
  declaration are byte-identical whichever key a repository wrote.
- Both keys set: refused with a dedicated `PolicyError` (not
  `MalformedToml`, whose "not valid TOML" prefix would be false). A `try_from`
  error reaches `from_toml_str` as a toml error and is mapped to
  `MalformedToml` (`policy/mod.rs:702`), so `from_toml_str` first checks the
  raw `integrations` table for both keys and returns the new variant;
  `try_from` still refuses as a backstop. It prints `set only one of [integrations] aval and greentopo; remove the aval line`.
- Unknown keys under `[integrations]` stay refused.
- The missing-integration blocker (`tooling.rs:57-60`) names both keys, so a
  greentopo repository is not told to add the key that triggers the
  both-keys refusal. For aval it reads: ``required integration `aval` (binary
  `{bin}`) is not available on PATH; set its [integrations] key (aval or
  greentopo) to "optional", or point `bin` at the binary you have``; the
  `amont` and `amont_agent` text and `default_bin("aval")` are unchanged. Doctor and gap messages keep the label `aval` whichever
  key was written; SPEC says so.
- Rollout: a relais older than this release refuses `greentopo` as an unknown
  field (no schema_version bump), so the t10 sweep starts only once this
  release is installed on every host that runs relais (laptop, homelab agent
  pods, CI); t11 is added to t10's `blocked_by`. Rollback after the sweep reverts the sweep PRs first.

## Phases
- [ ] Phase 1 — read the key: `crates/relais/src/policy/mod.rs` (a
  `try_from` wire struct; serde ignores the container's
  `deny_unknown_fields`/`default` under `try_from`, so `IntegrationsWire`
  carries `#[serde(deny_unknown_fields, default)]` itself and `Integrations`
  keeps `Default` and its derived `Serialize`), `crates/relais/src/tooling.rs`
  (blocker text, `aval` entry only), with unit tests beside the existing
  ones; becomes one commit
- [ ] Phase 2 — document and release note: `docs/SPEC.md`, `CHANGELOG.md`;
  becomes one commit
- [ ] Phase 3 — release and hand off: tag the release, install it on the
  laptop, a homelab agent pod and CI; in the decisions programme
  (`docs/programmes/topo-spotter-rename/t11-relais-aval-key.md`,
  `t10-fleet-sweep.md`, `t09-machine-switch.md`, `map.md`) write t11's
  Resolution (the version, and the fleet repositories whose `relais.toml`
  sets `[integrations] aval`, counted at that time), add t11 to t10's
  `blocked_by` and put it before t10 in the map's Handoff order, file the
  greentopo-spawn ticket into t09's `blocked_by`, and file the removal of
  the `aval` key into t10's fallback-read list; one decisions commit

## Decision log
- 2026-11-24 — normalise at deserialization into the existing `aval` field,
  because every reader, the candidate-boundary comparison and the authority
  hash then stay untouched. Renaming the field itself waits for the release
  that removes the `aval` key; that release moves every authority hash, so
  every repository grants trust again (ADR-0007).
- 2026-11-24 — `relais init` (`crates/relais/src/repo/detect.rs`) keeps
  writing `aval` until t10 closes, so an older relais still reads new
  repositories; switching it is a follow-up.
- 2026-11-24 — the binary stays `aval`; spawning `greentopo` is not in any
  ticket of the programme and is filed as a follow-up.

## Verification
- Phase 1, all through `RepoPolicy::from_toml_str` in `policy::tests`:
  `greentopo = "required"` vs `aval = "required"` → equal policies, equal
  `authority_hash()`, equal `serde_json::to_string` with no `greentopo` key,
  and that JSON deserializes back to an equal policy;
  `greentopo = { mode = "optional", bin = "x" }` → `Dependency::Full`, bin
  `x`; both keys → `matches!` the new `PolicyError` variant, its Display
  `contains` the refusal text and not `not valid TOML`; `greentop = "required"`
  → `unknown field` error; in `tooling` tests,
  `greentopo = { mode = "required", bin = "relais-test-no-such-bin" }` → a
  blocker naming both `aval` and `greentopo`.
- Phase 2 (end to end for Phase 1's parse): `relais doctor` in a scratch
  repository with `greentopo = "optional"` → aval reported optional, exit 0;
  with `greentopo = "required"` and no `aval` on PATH → the blocker above.
- Phase 3: `relais --version` on the laptop, one homelab agent pod and CI
  prints the release; `relais doctor` in each listed repository exits 0;
  t10's `blocked_by` names t11.

## Outcome

## Full reviews (reference)
See the sidecar file.

<!-- panel: repos=relais,decisions adds=contract reviewers=backend,lang:rust,tui,unix,po,architect body-sha=bc92dd2d08c5 -->
