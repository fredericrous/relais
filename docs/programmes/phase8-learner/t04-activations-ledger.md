+++
type = "research"
mode = "afk"
status = "closed"
blocked_by = ["t01-gate-inputs"]
repos = ["relais"]
+++
# What `router_activations` records, and how rollback reads it

## Question

Which columns does `router_activations` need so that `relais router
evaluate` can show why a stratum passed, `relais router rollback` can undo
exactly one activation, the early trigger can roll back on evidence, and
the same data never activates the same stratum twice (ADR-0003's rule for
the artifact learner)? Does the stop-row `label_policy_version` column
(owed per the `holds-until:` on `ROUTER_LABEL_POLICY_VERSION`) ride the
same ledger step (48 → 49)?

## Look at

- `crates/relais/src/ledger/schema/migrations.rs` (`router_exploration_stops`,
  the v45–v48 steps), `crates/relais/src/ledger/learning.rs` and
  `learn/registry.rs`, `learn/promote.rs` and `learn/typed_rollback.rs`
  (how artifact activations and rollbacks are recorded and undone),
  `.adr/records/0003-*.md`; the map's open point on how `router rollback`
  names what it undoes.

## Answer looks like

The table definition, the accessor signatures, the dedup rule, and whether
the stop-row column rides along, each with the reason.

## Resolution

2026-11-23, research, run in this session. Sources: ADR-0003, `learn/cycle.rs`
(`withheld`), `ledger/learning.rs`, `learn/typed_rollback.rs`, `ledger/router.rs`
(`record_router_exploration_stop`), migrations v22–v48, session-router plan §5.
The live ledger holds **0** rows in `router_exploration_stops`, ever. No
in-flight worktree adds a migration past v48. So the step is v49 unless
another branch takes it first; the build plan rechecks.

**1. One table, one row per activation; the row is the registry.** The
artifact learner keeps its artifacts in a registry and appends separate
rollback rows to `learning_cycles`. A stratum has no artifact and no
registry, so the activation row carries its own rollback. One partial unique
index lets the database refuse a second active row for a stratum.

```sql
CREATE TABLE router_activations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    -- the stratum, as columns (never the label): '*' in kind/scope is a
    -- merged stratum (§5: thin strata merge into their difficulty band)
    relation TEXT NOT NULL,            -- new_task | subagent
    kind TEXT NOT NULL,
    band TEXT NOT NULL,                -- d1-2 | d3 | d4-5
    scope TEXT NOT NULL,
    -- what is served; its shape is t03's, so it is versioned JSON
    adjustment_json TEXT NOT NULL,
    adjustment_version INTEGER NOT NULL,
    -- why it passed: all four gates' numbers, as `router evaluate --json`
    -- printed them; the evidence identity the dedup reads
    label_policy_version INTEGER NOT NULL,
    evidence_fingerprint TEXT NOT NULL,
    drawn_strong INTEGER NOT NULL,     -- the count the floor reads
    evaluate_json TEXT NOT NULL,
    activated_at TEXT NOT NULL,
    activated_by TEXT NOT NULL,        -- cycle | person
    envelope_json TEXT,                -- the routing envelope in force
    -- the way back, NULL while active
    rolled_back_at TEXT,
    rollback_source TEXT,              -- early_trigger | cycle | person
    rollback_reason TEXT,              -- flag_miss | revert_miss | four_misses | bound | person
    rollback_counts_json TEXT          -- misses, completed, bound at the rollback
);
CREATE UNIQUE INDEX router_activations_one_active
    ON router_activations (relation, kind, band, scope)
    WHERE rolled_back_at IS NULL;
ALTER TABLE router_decisions ADD COLUMN activation_id INTEGER;
ALTER TABLE router_exploration_stops ADD COLUMN label_policy_version INTEGER;
```

Why each part is there:
- `evaluate_json` lets `router evaluate` show why the stratum passed at the
  time it activated, not as it reads today.
- `rollback_counts_json` records each rollback with its counts, as §5
  requires.
- `envelope_json` and `activated_by` follow ADR-0003: every envelope action
  is recorded with its source.

**2. `router_decisions.activation_id` is the missing link.** Today no
column ties a routed task to the adjustment that routed it. Without it, the
early trigger cannot count "the tasks routed by the adjustment,
cumulatively" (§5). Two consequences, both for t03's ADR:
- **Wire contract.** `router-state` serves each adjustment with its `id`,
  and the plugin sends it back in `router-observe` (a new `accepts` entry).
- **The silent hole.** A plugin that applies an adjustment but does not echo
  the `id` makes the early trigger read zero tasks and never roll back. So
  `router-state` serves `adjustments` only to a plugin whose wire version
  echoes the id. Applying and echoing ship in one release.
- **Arms.** A task an adjustment routed has no exploration propensity. It
  never enters the drawn or undrawn arm, and it feeds only the activation's
  early trigger.

**3. Accessors** (`ledger/router.rs`, modelled on `ledger/learning.rs`):

```rust
pub struct StratumKey { pub relation: String, pub kind: String, pub band: String, pub scope: String }
pub fn record_router_activation(&self, a: &NewActivation) -> Result<i64>;          // Err(AlreadyActive) on the unique index
pub fn active_router_activations(&self) -> Result<Vec<RouterActivation>>;         // what router-state serves
pub fn router_activations_for(&self, key: &StratumKey) -> Result<Vec<RouterActivation>>; // newest first: dedup, evaluate
pub fn rollback_router_activation(&self, id: i64, r: &ActivationRollback) -> Result<bool>; // UPDATE … WHERE id = ? AND rolled_back_at IS NULL
pub fn tasks_routed_by_activation(&self, id: i64) -> Result<Vec<RouterTaskOutcome>>; // the early trigger's sample
pub fn drawn_strong_since(&self, key: &StratumKey, since: &str) -> Result<usize>;  // the anti-flap floor
```

`rollback_router_activation` returns whether a row changed. That makes the
rollback undo exactly one activation, and repeating it is harmless.

**4. Rollback names a stratum, prints by default, and applies with
`--write`.** `relais router rollback <stratum label>` (for example
`sub review d3 module`) resolves to the one active row, through the unique
index. It prints the row and its evidence, and `--write` rolls it back. This
follows `relais recipe rollback <name>` [--write], the existing person-facing
rollback, rather than adding `--dry-run`/`--yes`. `--id <n>` exists for a
stratum label that a later merge has made ambiguous. The early trigger calls
the same accessor with `rollback_source = early_trigger`.

**5. Dedup: the same evidence never activates a stratum twice.** This is
ADR-0003's rule, adapted. A stratum is withheld when either holds:
- **Same evidence.** Its `evidence_fingerprint` and `label_policy_version`
  match any earlier row of that stratum, active or rolled back. The
  fingerprint is the SHA-256 of the stratum key, the label policy, and the
  sorted `(task_id, strong outcome, drawn)` triples of both arms.
- **Too few new draws.** Fewer than 10 new strong drawn tasks have arrived
  in that stratum since its last rollback. This is the analogue of ADR-0003's
  10 runs.

Why each part:
- **The fingerprint alone does not suffice.** Outcomes are rewritten after
  the fact: a worse outcome overwrites a better one, and a parent backfill
  replaces an `unknown`. Either changes the hash with no new evidence; the
  floor stops that from re-activating.
- **Gate thresholds stay out of the key.** Otherwise a threshold edit would
  re-activate rolled-back data, which ADR-0003 forbids.
- **A person's rollback counts like the cycle's.**

**6. The stop-row `label_policy_version` rides the same step, and this
releases the `holds-until:` early.** Reasons:
- **An extra step costs an upgrade.** An older binary refuses a newer ledger
  (`a_ledger_from_a_newer_relais_is_refused`), so every separate step forces
  another upgrade.
- **The activations table carries the column anyway.**
- **Nothing needs a backfill.** The table has never held a row, so "no
  default for existing rows" touches nothing. The column stays nullable only
  because SQLite's `ADD COLUMN` cannot be `NOT NULL` without a default. The
  writer always sets it.

The build therefore:
- removes the `holds-until:` comment on `ROUTER_LABEL_POLICY_VERSION`;
- makes `record_router_exploration_stop` dedup on (counts,
  `label_policy_version`);
- has `router_exploration_stops()` read the column.

t06's "not owed yet" stays true as a measure; this is a choice to pay it
early, alongside a step that is needed anyway.

**Accepted gaps:**
- **The early trigger is partly blind.** Its "revert miss rolls back at
  once" is dead until t10–t12 ship, because t02 found the revert channel
  blind. Only `/relais-flag` and the count and bound triggers can fire.
- **t05 is still blocked.** Closing this ticket does not unblock the
  decision-record ticket, which still waits on t03.
- **The adjustment's shape is open.** `adjustment_json` and
  `adjustment_version` leave it to t03.
