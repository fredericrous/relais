# Full reviews: holdouts declare themselves in the store


All reviewers ran on opus-5-5, except codex (gpt-6-astra). plan-panel computed backend only (the plan reuses the previous plan's file). lang:rust and unix were added because the plan changes a config format and a remote fetch.

**Round 1**

- **unix:** approve-with-changes (26k, 35 s), 4 findings, all accepted:
  - (high) the concurrent cache race;
  - the 30 s bound for discovery;
  - an absolute `RELAIS_HOLDOUT_JUNIT` whose directory relais creates;
  - cache pruning.
- **backend:** approve-with-changes (32k, 45 s), 8 findings, all accepted:
  - (high) one `ls_remote` per preflight;
  - (high) the cache race and pruning;
  - (high) the unreachable store recorded and shown;
  - (high) a versioned digest, so successors report no false change;
  - the `enabled = false` variant;
  - a named ledger table;
  - trust, replaced by the person's rule;
  - observable checks.
- **lang:rust:** approve-with-changes (39k, 58 s), 6 findings, all accepted:
  - (high) one effective `HoldoutSettings.repos` merged before lookup and attachment;
  - (high) the unreachable store refused as `holdout_unreadable`;
  - (high) the `enabled = false` variant;
  - the cache race;
  - defaults: `clean_relative`, one timeout key, 1200 s from both sources;
  - the ledger table and a counting seam.
- **The person:** changed the trust model to confirm on change.

**Round 2**

- **backend:** approve-with-changes (32k, 57 s).
  - Applied: `config` in the fingerprint (high); removing an `inputs` entry asks (high).
  - Carried: confirmations keyed by repo too (medium); the trust listing shows shadowed holdouts (medium); `relais plan` marks unconfirmed holdouts, and a successor-of-unreachable test (low).

**Codex** (gpt-6-astra, high effort): approve-with-changes, 3 high findings, all applied:
1. the fingerprint covers the whole effective declaration, `command.env` included, and the data-only exemption is dropped;
2. acceptance binds to the reviewed sha and fingerprint, stale requests are refused, and the snapshot is persisted;
3. symlinks are refused throughout, and contents are re-verified before execution.

**Backend binding passes**

- **Pass 1** (24k, 34 s), high: the Codex tests were missing from Verification. Applied.
- **Pass 2** (23k, 17 s): approve.
  - Carried (medium): discovery reads the files the parsed `path` and `config` name, not only `check.sh`.
  - Carried (low): a code span in the symlink test shows HTML entities literally.
  - It added two observations to carry: Phase 4 counts unconfirmed skips separately; one definition change is expected after Phase 3, when a holdout's source moves from machine to store.

**Carried into implementation**

- Confirmations keyed by store, repo and name.
- `relais trust holdouts` lists holdouts a machine.toml table shadows.
- `relais plan` marks unconfirmed holdouts.
- A successor-of-unreachable-store test.
- Discovery reads the files the parsed `path` and `config` name.
- Phase 4 counts unconfirmed skips separately, and one definition change is expected after Phase 3.
- Fix the entity in the symlink test text.
