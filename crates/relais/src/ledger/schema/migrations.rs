/// Additive migrations, in order. Existing steps are never edited; a new
/// step appends. `schema_migrations` records what applied.
pub(in crate::ledger) const MIGRATIONS: &[(&str, &str)] = &[
    (
        "v1",
        r#"
    CREATE TABLE runs (
        id TEXT PRIMARY KEY,
        repo_path TEXT NOT NULL,
        status TEXT NOT NULL,
        root_session TEXT,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    CREATE TABLE contract_revisions (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        run_id TEXT NOT NULL,
        hash TEXT NOT NULL,
        contract_json TEXT NOT NULL,
        base_ref TEXT NOT NULL,
        base_sha TEXT,
        created_at TEXT NOT NULL,
        superseded_by INTEGER
    );
    CREATE TABLE attempts (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        run_id TEXT NOT NULL,
        revision_id INTEGER NOT NULL,
        attempt_index INTEGER NOT NULL,
        tier TEXT NOT NULL,
        phase TEXT NOT NULL,
        state TEXT NOT NULL,
        started_at TEXT NOT NULL,
        ended_at TEXT,
        worktree TEXT,
        candidate_sha TEXT
    );
    CREATE TABLE transitions (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        run_id TEXT NOT NULL,
        attempt_id INTEGER,
        from_state TEXT,
        to_state TEXT NOT NULL,
        reason TEXT NOT NULL,
        detail_json TEXT,
        at TEXT NOT NULL
    );
    CREATE TABLE evidence (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        run_id TEXT NOT NULL,
        attempt_id INTEGER,
        kind TEXT NOT NULL,
        path TEXT NOT NULL,
        sha256 TEXT,
        at TEXT NOT NULL
    );
    CREATE TABLE usage_events (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        event_id TEXT NOT NULL UNIQUE,
        run_id TEXT NOT NULL,
        attempt_id INTEGER,
        parent_event_id TEXT,
        model TEXT,
        input_tokens INTEGER,
        output_tokens INTEGER,
        cache_read_tokens INTEGER,
        cache_write_tokens INTEGER,
        cost_micros INTEGER NOT NULL,
        cost_kind TEXT NOT NULL,
        completeness TEXT NOT NULL,
        inclusive INTEGER NOT NULL DEFAULT 0,
        at TEXT NOT NULL
    );
    CREATE INDEX idx_usage_run ON usage_events(run_id);
    CREATE TABLE dispatches (
        dispatch_id TEXT PRIMARY KEY,
        run_id TEXT NOT NULL,
        attempt_id INTEGER,
        intent_json TEXT NOT NULL,
        pid INTEGER,
        session_id TEXT,
        state TEXT NOT NULL,
        reserved_micros INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    CREATE TABLE outcomes (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        run_id TEXT NOT NULL,
        kind TEXT NOT NULL,
        detail_json TEXT,
        at TEXT NOT NULL
    );
    CREATE TABLE receipts (
        run_id TEXT PRIMARY KEY,
        receipt_json TEXT NOT NULL,
        hash TEXT NOT NULL,
        at TEXT NOT NULL
    );
    CREATE TABLE features (
        dispatch_id TEXT PRIMARY KEY,
        feature_json TEXT NOT NULL,
        at TEXT NOT NULL
    );
    CREATE TABLE predictions (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        run_id TEXT NOT NULL,
        artifact_id TEXT,
        input_hash TEXT,
        result_json TEXT,
        at TEXT NOT NULL
    );
    "#,
    ),
    (
        // Work packages (SPEC §19) are runs of their own, linked to the root
        // run so cost, attempts and identity aggregate over the tree.
        "v2",
        r#"
    ALTER TABLE runs ADD COLUMN parent_run TEXT;
    ALTER TABLE runs ADD COLUMN package_id TEXT;
    CREATE INDEX idx_runs_parent ON runs(parent_run);
    "#,
    ),
    (
        // Unknown usage is unknown, never zero (SPEC §11). `cost_micros`
        // was NOT NULL, so a dispatch whose harness reported no cost was
        // stored as 0 and summed into a figure that read as money. SQLite
        // cannot drop a NOT NULL constraint in place: the table is rebuilt
        // with the column nullable, rows preserved, and every row whose
        // completeness already said `unknown` gets the NULL its zero stood
        // for. `SUM` skips NULL, so a run's cost becomes the lower bound
        // its completeness label always claimed it was.
        "v3",
        r#"
    CREATE TABLE usage_events_v3 (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        event_id TEXT NOT NULL UNIQUE,
        run_id TEXT NOT NULL,
        attempt_id INTEGER,
        parent_event_id TEXT,
        model TEXT,
        input_tokens INTEGER,
        output_tokens INTEGER,
        cache_read_tokens INTEGER,
        cache_write_tokens INTEGER,
        cost_micros INTEGER,
        cost_kind TEXT NOT NULL,
        completeness TEXT NOT NULL,
        inclusive INTEGER NOT NULL DEFAULT 0,
        at TEXT NOT NULL
    );
    INSERT INTO usage_events_v3
        (id, event_id, run_id, attempt_id, parent_event_id, model,
         input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
         cost_micros, cost_kind, completeness, inclusive, at)
    SELECT id, event_id, run_id, attempt_id, parent_event_id, model,
           input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
           CASE WHEN completeness = '"unknown"' THEN NULL ELSE cost_micros END,
           cost_kind, completeness, inclusive, at
    FROM usage_events;
    DROP TABLE usage_events;
    ALTER TABLE usage_events_v3 RENAME TO usage_events;
    CREATE INDEX idx_usage_run ON usage_events(run_id);
    "#,
    ),
    (
        // A task is the identity a run's cost is attributed to (SPEC's
        // task spine): stable across a contract revision or a re-run, so
        // cost sums per task rather than per run. Every existing run
        // predates the spine, so each root run's tree is backfilled one
        // `task-legacy-<root run id>` task, `origin = 'backfilled'`,
        // propagated to every descendant by a recursive walk of
        // `parent_run` — the same tree `run_cost` already walks.
        "v4",
        r#"
    CREATE TABLE tasks (
        task_id TEXT PRIMARY KEY,
        repo_key TEXT NOT NULL,
        first_run TEXT NOT NULL,
        origin TEXT NOT NULL,
        created_at TEXT NOT NULL
    );
    ALTER TABLE runs ADD COLUMN task_id TEXT;
    CREATE INDEX idx_runs_task ON runs(task_id);
    -- `repo_key` is a hash of the run's repository identity everywhere a
    -- run writes one. A migration cannot compute it: the identity comes
    -- from git, and a legacy row carries only the path the run was
    -- launched in, which may have been a worktree that no longer exists.
    -- So a backfilled task says so — `legacy:<path>` is unmistakably not
    -- a key, and grouping by `repo_key` puts each legacy task in its own
    -- bucket instead of silently merging it with a repository's real one.
    INSERT INTO tasks (task_id, repo_key, first_run, origin, created_at)
        SELECT 'task-legacy-' || id, 'legacy:' || repo_path, id, 'backfilled', created_at
          FROM runs WHERE parent_run IS NULL;
    WITH RECURSIVE tree(id, root) AS (
        SELECT id, id FROM runs WHERE parent_run IS NULL
        UNION ALL
        SELECT runs.id, tree.root FROM runs JOIN tree ON runs.parent_run = tree.id
    )
    UPDATE runs SET task_id = 'task-legacy-' || (SELECT root FROM tree WHERE tree.id = runs.id)
     WHERE id IN (SELECT id FROM tree);
    "#,
    ),
    (
        "v5",
        r#"
    -- Typed outcomes (SPEC §20) are read per task and per candidate, not
    -- only per run: `outcomes.task_id` lets `latest_outcome` answer "is
    -- this task's accepted change still standing" without a join through
    -- `runs`, and `outcomes.candidate_sha` names the exact candidate a
    -- row is about even if the run later grows more attempts.
    --
    -- Its own step, not an edit to v4: v4 shipped in 2f37f80 and every
    -- ledger opened since has it recorded, so `apply_step` would skip
    -- the additions and the columns would exist only where the ledger
    -- had never been opened — which is every test and no real machine.
    ALTER TABLE outcomes ADD COLUMN task_id TEXT;
    ALTER TABLE outcomes ADD COLUMN candidate_sha TEXT;
    CREATE INDEX idx_outcomes_task ON outcomes(task_id);
    "#,
    ),
    (
        // A run that reaches `needs_review`, `needs_decision` or
        // `interrupted` is waiting for a person to answer it (SPEC's
        // decision spine: `State::awaits_a_person`). Before this, that
        // wait had no record of its own — only the run's transition
        // history, which never says WHO answered it or WHEN. One open
        // row per such run, closed by `relais decide`; `record_transition`
        // opens the row for every run from here on, in the same
        // transaction as the transition that raises it, so a run can
        // never be waiting without one.
        //
        // The three state names below are `State::awaits_a_person`'s
        // definition, spelled out in SQL because a migration cannot call
        // into this binary's Rust — `lifecycle`'s own test and this
        // migration's ledger-side test each assert they still agree.
        "v6",
        r#"
    CREATE TABLE decisions (
        run TEXT PRIMARY KEY,
        task TEXT NOT NULL,
        raised_at TEXT NOT NULL,
        raised_state TEXT NOT NULL,
        raised_reason TEXT NOT NULL,
        resolved_at TEXT,
        resolution TEXT,
        actor TEXT,
        note TEXT,
        successor_run TEXT
    );
    CREATE INDEX idx_decisions_unresolved ON decisions(resolved_at);
    INSERT INTO decisions (run, task, raised_at, raised_state, raised_reason)
    SELECT runs.id,
           COALESCE(runs.task_id, 'task-legacy-' || runs.id),
           COALESCE(
               (SELECT t.at FROM transitions t WHERE t.run_id = runs.id ORDER BY t.id DESC LIMIT 1),
               runs.updated_at
           ),
           COALESCE(
               (SELECT t.to_state FROM transitions t WHERE t.run_id = runs.id ORDER BY t.id DESC LIMIT 1),
               runs.status
           ),
           COALESCE(
               (SELECT t.reason FROM transitions t WHERE t.run_id = runs.id ORDER BY t.id DESC LIMIT 1),
               'worker_dispatched'
           )
      FROM runs
     WHERE COALESCE(
               (SELECT t.to_state FROM transitions t WHERE t.run_id = runs.id ORDER BY t.id DESC LIMIT 1),
               runs.status
           ) IN ('needs_review', 'needs_decision', 'interrupted');
    "#,
    ),
    (
        "v7",
        r#"
    -- v6 backfilled each open decision from the run's LAST transition.
    -- For a run that started waiting and then had its worktree retired,
    -- that is the retirement row: a same-state transition whose detail
    -- carries a reference and a patch and no gaps. So the row pointed at
    -- the wrong moment, and `decide --answer approve` — which reads the
    -- transition `raised_at` names — would find no gaps on a run that
    -- has them. The raising transition is the FIRST one landing on a
    -- state that awaits a person, which for a backfilled row is the only
    -- wait it has ever had.
    --
    -- Its own step, not a correction of v6: v6 is recorded on every
    -- ledger opened since it merged, and `apply_step` skips a version
    -- already applied.
    UPDATE decisions
       SET raised_at = COALESCE(
               (SELECT t.at FROM transitions t
                 WHERE t.run_id = decisions.run
                   AND t.to_state IN ('needs_review', 'needs_decision', 'interrupted')
                 ORDER BY t.id ASC LIMIT 1),
               decisions.raised_at
           ),
           raised_state = COALESCE(
               (SELECT t.to_state FROM transitions t
                 WHERE t.run_id = decisions.run
                   AND t.to_state IN ('needs_review', 'needs_decision', 'interrupted')
                 ORDER BY t.id ASC LIMIT 1),
               decisions.raised_state
           ),
           raised_reason = COALESCE(
               (SELECT t.reason FROM transitions t
                 WHERE t.run_id = decisions.run
                   AND t.to_state IN ('needs_review', 'needs_decision', 'interrupted')
                 ORDER BY t.id ASC LIMIT 1),
               decisions.raised_reason
           )
     WHERE resolved_at IS NULL;
    "#,
    ),
    (
        // A usage event said only what it cost, never what produced it.
        // Five columns say so: `phase` names which of the run's phases
        // (worker attempt, review, planning, integration) spent the
        // money — the same typed `UsagePhase` an attempt's own `phase`
        // column already carries, so the two cannot spell the same thing
        // differently; `duration_ms` is the dispatch's own elapsed wall
        // time, not derived from `attempts.started_at`/`ended_at`, which
        // are the ATTEMPT's span and do not exist at all for a reviewer
        // or planner dispatch; `requested_model`/`requested_effort`
        // record what the route asked for, beside `model`, which already
        // records what the harness reported running, so a substitution
        // is visible in the row rather than only at the moment it
        // happened; `harness` names the identity the run probed.
        //
        // All nullable: a row written before this step reads them as
        // absent, never as a default that would misreport an attempt
        // this binary never measured.
        "v8",
        r#"
    ALTER TABLE usage_events ADD COLUMN phase TEXT;
    ALTER TABLE usage_events ADD COLUMN duration_ms INTEGER;
    ALTER TABLE usage_events ADD COLUMN requested_model TEXT;
    ALTER TABLE usage_events ADD COLUMN requested_effort TEXT;
    ALTER TABLE usage_events ADD COLUMN harness TEXT;
    "#,
    ),
    (
        // A mandatory criterion asking for a human sign-off had no way
        // to be answered: `verify::acceptance_gaps` raised it forever,
        // because nothing recorded that a person ever gave one. This
        // table is that record — one row per (run, criterion), written
        // only by `relais decide --answer approve --criterion <id>` — so
        // `acceptance_gaps`/`settle_acceptance` can read the id back and
        // stop treating an answered criterion as unmet.
        "v9",
        r#"
    CREATE TABLE human_signoffs (
        run_id TEXT NOT NULL,
        criterion_id TEXT NOT NULL,
        actor TEXT NOT NULL,
        note TEXT,
        recorded_at TEXT NOT NULL,
        PRIMARY KEY (run_id, criterion_id)
    );
    "#,
    ),
    (
        // `record_outcome` always had a receipt-derived candidate to
        // write, because a run reaching `accepted` had always gone
        // through verification, which always stores one. A run accepted
        // through a person's approval (`relais decide --answer approve`)
        // on a contract interrupted before verification never wrote a
        // receipt, so `relais feedback` about it has no candidate to
        // name. `candidate_sha` was always a nullable column — `ALTER
        // TABLE ADD COLUMN` cannot declare NOT NULL without a default —
        // but nothing before this step ever inserted NULL there, so the
        // guarantee was never exercised. This step rebuilds the table to
        // make that guarantee explicit and tested, rather than relying on
        // an accident of v5's DDL. Its own step, not an edit to v5's: v5
        // shipped and every ledger opened since already ran it.
        "v10",
        r#"
    CREATE TABLE outcomes_v10 (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        run_id TEXT NOT NULL,
        task_id TEXT,
        candidate_sha TEXT,
        kind TEXT NOT NULL,
        detail_json TEXT,
        at TEXT NOT NULL
    );
    INSERT INTO outcomes_v10 (id, run_id, task_id, candidate_sha, kind, detail_json, at)
        SELECT id, run_id, task_id, candidate_sha, kind, detail_json, at FROM outcomes;
    DROP TABLE outcomes;
    ALTER TABLE outcomes_v10 RENAME TO outcomes;
    CREATE INDEX idx_outcomes_task ON outcomes(task_id);
    "#,
    ),
    (
        // An evidence row said only its kind and its path — free text, no
        // way to name what produced it or what it is about. These four
        // columns let a row say that: `tool` and `external_id` are where
        // it came from, `subject` and `criterion_id` are what it attests
        // to. All nullable, so every row this ledger has ever written
        // reads back unchanged, and `relais evidence attach` is the only
        // thing that ever fills them all in.
        "v11",
        r#"
    ALTER TABLE evidence ADD COLUMN tool TEXT;
    ALTER TABLE evidence ADD COLUMN external_id TEXT;
    ALTER TABLE evidence ADD COLUMN subject TEXT;
    ALTER TABLE evidence ADD COLUMN criterion_id TEXT;
    "#,
    ),
    (
        // A coordinator restart adopts every dispatch `live_dispatches`
        // still calls `launched` (SPEC §23), but that query named only
        // `dispatch_id`, `run_id` and `pid` — so adoption filled the
        // caps a restart cannot re-derive with placeholders: session
        // "unknown", no parent, depth 0. The session and reservation
        // were already columns; these four were not. All four are
        // nullable, so a row from before this step reads back with them
        // NULL, never guessed — `live_dispatches` reports that row as
        // unrecorded rather than inventing an answer for it.
        "v12",
        r#"
    ALTER TABLE dispatches ADD COLUMN source TEXT;
    ALTER TABLE dispatches ADD COLUMN agent_id TEXT;
    ALTER TABLE dispatches ADD COLUMN parent_dispatch TEXT;
    ALTER TABLE dispatches ADD COLUMN depth INTEGER;
    "#,
    ),
    (
        // The router decides among a deterministic recipe, a risk floor,
        // a learned artifact or the conservative baseline (SPEC §6), but
        // nothing recorded which one a dispatch actually got — a dataset
        // could not tell a route the learner chose from one it never got
        // the evidence to choose. Nullable, so a row from before this
        // step reads back `None`: "nobody recorded how this was routed",
        // never coerced into `ConservativeBaseline`, which is a positive
        // claim about a decision that was never captured.
        "v13",
        r#"
    ALTER TABLE dispatches ADD COLUMN routed_by TEXT;
    "#,
    ),
    (
        // A controlled comparison needs somewhere to live: one row per
        // arm of one comparison (SPEC §17), a run's purpose distinguishing
        // ordinary work from a replay or a live trial arm, and a trial id
        // on the dispatch that belongs to one. Storage and readers only —
        // nothing here assigns a trial, gives a run a purpose or a
        // dispatch a trial id; that is a later package. `runs.purpose`
        // and `dispatches.trial_id` are nullable, so every existing row
        // reads back as ordinary work with no trial id, never guessed
        // into either.
        "v14",
        r#"
    CREATE TABLE trials (
        trial_id TEXT PRIMARY KEY,
        task_id TEXT NOT NULL,
        source_run_id TEXT NOT NULL,
        incumbent_recipe_id TEXT NOT NULL,
        arm_recipe_id TEXT NOT NULL,
        arm_index INTEGER NOT NULL,
        assignment_probability REAL NOT NULL,
        seed INTEGER NOT NULL,
        base_sha TEXT NOT NULL,
        contract_hash TEXT NOT NULL,
        verification_profile_hash TEXT NOT NULL,
        workspace_isolation TEXT NOT NULL,
        outcome TEXT,
        accepted_without_escalation INTEGER,
        cost_micros INTEGER,
        cost_completeness TEXT,
        duration_ms INTEGER,
        created_at TEXT NOT NULL,
        -- When the arm concluded, which `created_at` cannot answer: an
        -- arm that never settles and one that settled instantly are the
        -- same row without this. NULL until `settle_trial` fills it, and
        -- `settle_trial` refuses a second settlement, so it is written
        -- exactly once.
        settled_at TEXT
    );
    ALTER TABLE runs ADD COLUMN purpose TEXT;
    ALTER TABLE dispatches ADD COLUMN trial_id TEXT;
    "#,
    ),
    (
        // The orchestrating Claude Code session's own spend (SPEC §11):
        // one row per API message, imported from that session's own
        // transcript rather than reported by a worker. `message_id` is
        // the transcript's own dedup key and is UNIQUE, so importing the
        // same session twice inserts nothing the second time
        // (`INSERT OR IGNORE`). `cost_micros` is nullable — a model
        // absent from the price table is unknown, never zero (SPEC §11)
        // — and `pricing_version` travels with it so a stored cost can
        // always be traced to the table it came from, even after that
        // table changes.
        "v15",
        r#"
    CREATE TABLE orchestration_usage (
        message_id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL,
        transcript TEXT NOT NULL,
        model TEXT NOT NULL,
        speed TEXT NOT NULL,
        input_tokens INTEGER NOT NULL,
        output_tokens INTEGER NOT NULL,
        cache_read_tokens INTEGER NOT NULL,
        cache_write_5m_tokens INTEGER NOT NULL,
        cache_write_1h_tokens INTEGER NOT NULL,
        cost_micros INTEGER,
        pricing_version TEXT,
        completeness TEXT NOT NULL,
        at TEXT NOT NULL
    );
    CREATE INDEX idx_orchestration_usage_session ON orchestration_usage(session_id);
    CREATE INDEX idx_orchestration_usage_at ON orchestration_usage(at);
    "#,
    ),
    (
        // A live trial draws among several arms, and what the draw was
        // over is part of the evidence: `arms_json` is the JSON list of
        // every arm's recipe id, control first, exactly as assigned.
        // NULL on every existing row: a replay is one arm, not a draw.
        "v16",
        r#"
    ALTER TABLE trials ADD COLUMN arms_json TEXT;
    "#,
    ),
    (
        // A replay trial named its SOURCE run only, so which run the arm
        // itself executed as had to be guessed by recency. `arm_run_id`
        // records it. NULL on every row written before this step: those
        // arms' runs are not known, and nothing reads them by guesswork.
        "v17",
        r#"
    ALTER TABLE trials ADD COLUMN arm_run_id TEXT;
    "#,
    ),
    (
        // The transcript messages a native worker's attempt was booked
        // for (SPEC §11). A native worker is a subagent of the
        // orchestrating session, so `usage import` meets the same
        // messages; this table is how each is counted once, whichever
        // reads the transcript first.
        "v18",
        r#"
    CREATE TABLE native_usage_messages (
        message_id TEXT PRIMARY KEY,
        dispatch_id TEXT NOT NULL,
        run_id TEXT NOT NULL,
        recorded_at TEXT NOT NULL
    );
    "#,
    ),
    (
        // Every trust grant `relais trust grant` wrote (SPEC §5): who
        // granted which key for which repository, from where, and the
        // exact steps it authorized. machine.toml holds the grant itself;
        // this is the audit trail, and what the next question for the
        // same repository is marked against, so a person re-asked after
        // an edit sees what changed rather than the whole list again.
        "v19",
        r#"
    CREATE TABLE trust_grants (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        grant_key TEXT NOT NULL,
        repo_key TEXT NOT NULL,
        repo TEXT NOT NULL,
        reviewed_by TEXT NOT NULL,
        source TEXT NOT NULL,
        steps_json TEXT NOT NULL,
        granted_at TEXT NOT NULL
    );
    CREATE INDEX idx_trust_grants_repo ON trust_grants(repo_key, id);
    "#,
    ),
    (
        // The session router's own records (SPEC §30), written by
        // `relais native router-observe`. Keys are the wire contract's
        // (docs/router-protocol.md); an absent turn or agent id is stored
        // as '' so a key holding one is still UNIQUE (SQLite treats NULLs
        // as distinct). No cost column anywhere: a cost is computed from
        // the token columns and `[pricing]` when read, never stored. The
        // usage rows are the router's own accounting and are never added
        // to `orchestration_usage` totals (plan decision log, S0).
        "v20",
        r#"
    CREATE TABLE router_decisions (
        session TEXT NOT NULL,
        task_id TEXT NOT NULL,
        turn_id TEXT NOT NULL,
        agent_id TEXT NOT NULL,
        relation TEXT NOT NULL,
        class_kind TEXT,
        difficulty INTEGER,
        scope TEXT,
        uncertainty TEXT,
        verifiable INTEGER,
        confidence REAL,
        tier TEXT NOT NULL,
        model TEXT NOT NULL,
        effort TEXT,
        reason TEXT NOT NULL,
        mode_effective TEXT NOT NULL,
        holdout INTEGER NOT NULL,
        applied INTEGER NOT NULL,
        explored INTEGER NOT NULL,
        propensity REAL,
        draw REAL,
        would_pass_gate INTEGER NOT NULL,
        at TEXT NOT NULL,
        recorded_at TEXT NOT NULL,
        PRIMARY KEY (session, task_id, turn_id, agent_id)
    );
    CREATE TABLE router_usage (
        session TEXT NOT NULL,
        task_id TEXT NOT NULL,
        turn_id TEXT NOT NULL,
        step INTEGER NOT NULL,
        agent_id TEXT NOT NULL,
        source TEXT NOT NULL,
        model TEXT NOT NULL,
        input_tokens INTEGER NOT NULL,
        output_tokens INTEGER NOT NULL,
        cache_read_input_tokens INTEGER NOT NULL,
        cache_creation_input_tokens INTEGER NOT NULL,
        at TEXT NOT NULL,
        recorded_at TEXT NOT NULL,
        PRIMARY KEY (session, turn_id, step, agent_id, source)
    );
    CREATE INDEX idx_router_usage_task ON router_usage(session, task_id, agent_id);
    CREATE TABLE router_reassess (
        session TEXT NOT NULL,
        task_id TEXT NOT NULL,
        agent_id TEXT NOT NULL,
        event TEXT NOT NULL,
        tier_from TEXT NOT NULL,
        tier_to TEXT NOT NULL,
        effort_to TEXT,
        escalating INTEGER NOT NULL,
        files_json TEXT NOT NULL,
        at TEXT NOT NULL,
        recorded_at TEXT NOT NULL,
        PRIMARY KEY (session, task_id, agent_id, event, at)
    );
    CREATE TABLE router_tasks (
        session TEXT NOT NULL,
        task_id TEXT NOT NULL,
        agent_id TEXT NOT NULL,
        started_at TEXT NOT NULL,
        ended_at TEXT,
        class_kind TEXT,
        difficulty INTEGER,
        scope TEXT,
        uncertainty TEXT,
        verifiable INTEGER,
        confidence REAL,
        outcome TEXT NOT NULL,
        outcome_rank INTEGER NOT NULL,
        completed_at_end TEXT,
        inferred_json TEXT NOT NULL,
        files_json TEXT NOT NULL,
        escalations INTEGER NOT NULL,
        exhausted INTEGER NOT NULL,
        turns INTEGER NOT NULL,
        explicit_quote TEXT,
        recorded_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        PRIMARY KEY (session, task_id, agent_id)
    );
    CREATE INDEX idx_router_tasks_started ON router_tasks(started_at);
    CREATE TABLE router_provenance (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        kind TEXT NOT NULL,
        granted_by TEXT,
        epsilon_max REAL,
        source TEXT NOT NULL,
        at TEXT NOT NULL
    );
    CREATE INDEX idx_router_provenance_kind ON router_provenance(kind, id);
    CREATE VIEW router_task_tokens AS
        SELECT session, task_id, agent_id,
               SUM(input_tokens + output_tokens + cache_read_input_tokens
                   + cache_creation_input_tokens) AS total_tokens
          FROM router_usage
         GROUP BY session, task_id, agent_id;
    "#,
    ),
    (
        // Whether the verified candidate patch survives intact in the
        // commit a person salvaged (SPEC §17 labels): NULL is unknown
        // (every salvage recorded before this step, or one whose
        // repository or patch was gone), 1 contained, 0 rewritten.
        "v21",
        r#"
    ALTER TABLE decisions ADD COLUMN candidate_contained INTEGER;
    "#,
    ),
    (
        // The learning loop's record (SPEC §17): one row per cycle, inserted
        // when it starts with `ended_at` NULL, closed when it finishes, and
        // closed as `interrupted` by the next cycle when it never did. A
        // person's `relais rollback` writes a finished row of its own
        // (`source = 'person'`). Additive: a binary that predates it never
        // reads the table, and the older ledger it opens has one more table.
        "v22",
        r#"
    CREATE TABLE learning_cycles (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        started_at TEXT NOT NULL,
        ended_at TEXT,
        label_policy_version INTEGER,
        dataset_fingerprint TEXT,
        examples_by_class_json TEXT,
        examples_by_source_json TEXT,
        cohorts_json TEXT,
        artifact_id TEXT,
        evaluate_json TEXT,
        action TEXT NOT NULL DEFAULT 'none',
        source TEXT NOT NULL DEFAULT 'cycle',
        error TEXT,
        note TEXT
    );
    CREATE INDEX idx_learning_cycles_started ON learning_cycles(started_at);
    "#,
    ),
    (
        // Evidence for the session router (learning-loop Phase 3): where a
        // task's outcome came from (`own`: its task record; `parent`: the
        // task_outcome record its parent's end sent, only over `unknown`),
        // the checks the task ran with their kinds, and the capability
        // table's version on each decision. Additive; every task recorded
        // before this step took its outcome from its own record.
        "v23",
        r#"
    ALTER TABLE router_tasks ADD COLUMN outcome_source TEXT;
    ALTER TABLE router_tasks ADD COLUMN checks_json TEXT;
    UPDATE router_tasks SET outcome_source = 'own';
    ALTER TABLE router_decisions ADD COLUMN table_version INTEGER;
    "#,
    ),
    (
        // The exploration stop rule's record (learning-loop Phase 4): one
        // row each time `router-state` finds drawn tasks doing worse than
        // undrawn ones in the same strata and serves epsilon 0, with the
        // counts it saw. Additive: a binary that predates it never reads
        // the table.
        "v24",
        r#"
    CREATE TABLE router_exploration_stops (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        at TEXT NOT NULL,
        drawn INTEGER NOT NULL,
        drawn_bad INTEGER NOT NULL,
        undrawn INTEGER NOT NULL,
        undrawn_bad INTEGER NOT NULL
    );
    "#,
    ),
    (
        // The session router's classifier calls kept for training
        // (learning-loop Phase 5, relais ADR-0002): everything about a
        // call EXCEPT its text, which lives in a local 0600 day file. The
        // text is represented by its SHA-256 only, never stored. `answer_json`
        // is the parsed answer without the person's quoted words. Additive:
        // a binary that predates it never reads the table.
        "v25",
        r#"
    CREATE TABLE router_classifications (
        session TEXT NOT NULL,
        call_id TEXT NOT NULL,
        purpose TEXT NOT NULL,
        model TEXT NOT NULL,
        prompt_version TEXT NOT NULL,
        input_sha256 TEXT NOT NULL,
        answer_json TEXT,
        parse_ok INTEGER NOT NULL,
        latency_ms INTEGER,
        at TEXT NOT NULL,
        recorded_at TEXT NOT NULL,
        PRIMARY KEY (session, call_id)
    );
    CREATE INDEX idx_router_classifications_at ON router_classifications(at);
    "#,
    ),
    (
        // The plan-review panel's outcomes (plan-review-learns-model
        // Phase 2), written by `relais native review-outcome`: one row per
        // reviewer launch, keyed by its agent id, labelled `clean`, `miss`
        // or `unknown`. Not router outcomes, so not in `router_tasks`.
        // Additive: a binary that predates it never reads the table.
        "v26",
        r#"
    CREATE TABLE review_outcomes (
        agent_id TEXT PRIMARY KEY,
        plan_path TEXT NOT NULL,
        role TEXT NOT NULL,
        class TEXT NOT NULL,
        round INTEGER NOT NULL,
        body_sha TEXT NOT NULL,
        model TEXT,
        tokens INTEGER,
        seconds REAL,
        verdict TEXT,
        label TEXT NOT NULL,
        label_reason TEXT,
        router_task_id TEXT,
        recorded_at INTEGER NOT NULL,
        labelled_at TEXT NOT NULL
    );
    "#,
    ),
    (
        // Who decided, typed (orchestrator-decides plan, Phase 1): the
        // free-text `actor` becomes one of three values, the original
        // spelling is kept in `actor_raw`, and each answer records how it
        // reached relais (`recorded_via`; every existing row is
        // `legacy`), the person's words when the orchestrator typed for
        // them (`person_quote`), and the advisor's stored advice (columns
        // only; nothing writes them yet). `apply_actor_step` refuses, by
        // name, a spelling the CASE below does not map BEFORE this runs
        // (the CASE has no ELSE on purpose: an unmapped spelling must
        // never become a silent NULL), and backs the ledger up first.
        "v27",
        r#"
    ALTER TABLE decisions ADD COLUMN actor_raw TEXT;
    ALTER TABLE decisions ADD COLUMN person_quote TEXT;
    ALTER TABLE decisions ADD COLUMN recorded_via TEXT;
    ALTER TABLE decisions ADD COLUMN advice_p_approve REAL;
    ALTER TABLE decisions ADD COLUMN advice_model TEXT;
    ALTER TABLE decisions ADD COLUMN advice_revision TEXT;
    ALTER TABLE decisions ADD COLUMN advice_null_reason TEXT;
    ALTER TABLE human_signoffs ADD COLUMN actor_raw TEXT;
    ALTER TABLE human_signoffs ADD COLUMN person_quote TEXT;
    ALTER TABLE human_signoffs ADD COLUMN recorded_via TEXT;
    CREATE TABLE decide_nonces (
        session TEXT PRIMARY KEY,
        nonce_hash TEXT NOT NULL,
        created_at TEXT NOT NULL
    );
    UPDATE decisions SET actor_raw = actor, recorded_via = 'legacy';
    UPDATE human_signoffs SET actor_raw = actor, recorded_via = 'legacy';
    UPDATE decisions SET actor = CASE actor
        WHEN 'claude' THEN 'orchestrator'
        WHEN 'claude-orchestrator' THEN 'orchestrator'
        WHEN 'claude-session' THEN 'orchestrator'
        WHEN 'claude (for the person)' THEN 'person-via-orchestrator'
        WHEN 'Frédéric Rousseau (via Claude Code)' THEN 'person-via-orchestrator'
        WHEN 'fredericrous' THEN 'person'
        WHEN 'person' THEN 'person'
        WHEN 'orchestrator' THEN 'orchestrator'
        WHEN 'person-via-orchestrator' THEN 'person-via-orchestrator'
    END
    WHERE actor IS NOT NULL;
    UPDATE human_signoffs SET actor = CASE actor
        WHEN 'claude' THEN 'orchestrator'
        WHEN 'claude-orchestrator' THEN 'orchestrator'
        WHEN 'claude-session' THEN 'orchestrator'
        WHEN 'claude (for the person)' THEN 'person-via-orchestrator'
        WHEN 'Frédéric Rousseau (via Claude Code)' THEN 'person-via-orchestrator'
        WHEN 'fredericrous' THEN 'person'
        WHEN 'person' THEN 'person'
        WHEN 'orchestrator' THEN 'orchestrator'
        WHEN 'person-via-orchestrator' THEN 'person-via-orchestrator'
    END;
    "#,
    ),
    (
        // The process that drives a run, recorded when it takes the run:
        // its pid, that process's start time (a reused pid has another),
        // and when it took the run. Additive and nullable: a row written
        // before this step has no driver on record, never a guessed one.
        "v28",
        r#"
    ALTER TABLE runs ADD COLUMN driver_pid INTEGER;
    ALTER TABLE runs ADD COLUMN driver_started TEXT;
    ALTER TABLE runs ADD COLUMN driver_since TEXT;
    "#,
    ),
    (
        // A task's type (docs/plans/2026-10-12-task-types.md): the label,
        // how it was given (`declared` by the contract, `classified` for
        // history) and the taxonomy version it was given under, plus the
        // design a trial's arm was handed. Additive and nullable: a task
        // that predates the step has no type, never a guessed one.
        "v29",
        r#"
    ALTER TABLE tasks ADD COLUMN task_type TEXT;
    ALTER TABLE tasks ADD COLUMN task_type_source TEXT;
    ALTER TABLE tasks ADD COLUMN taxonomy_version INTEGER;
    ALTER TABLE trials ADD COLUMN design_sha256 TEXT;
    "#,
    ),
    (
        // A repository is trusted once (ADR-0007). `repo_declarations` is
        // the command declaration (steps and integrations, never models)
        // each repository last ran under, one row per `repo_key`, so a
        // change to the commands is said once and not on every run.
        // `runs.trust_status` is which grant admitted a run
        // (`repository` or `legacy_exact`), what `relais doctor` counts to
        // say whether a legacy grant is still in use. Additive: a run
        // written before the step has no status, never a guessed one. The
        // step is preceded by a copy of the ledger file (`TRUST_STEP`).
        "v30",
        r#"
    CREATE TABLE repo_declarations (
        repo_key TEXT PRIMARY KEY,
        declaration_hash TEXT NOT NULL,
        declaration_json TEXT NOT NULL,
        seen_at TEXT NOT NULL
    );
    ALTER TABLE runs ADD COLUMN trust_status TEXT;
    "#,
    ),
    (
        // Which trials authorised a typed research recipe's promotion
        // (docs/plans/2026-10-12-task-types.md, Phase 3). Outside the
        // recipe content on purpose: the recipe id is a hash of that
        // content, and it must equal the measured candidate's. One row per
        // promoted recipe and task type. Additive: a promotion made before
        // the step left no row, and no row is ever guessed for it.
        "v31",
        r#"
    CREATE TABLE recipe_promotions (
        promotion_id INTEGER PRIMARY KEY AUTOINCREMENT,
        recipe_id TEXT NOT NULL,
        recipe_name TEXT NOT NULL,
        revision INTEGER NOT NULL,
        task_type TEXT NOT NULL,
        gate TEXT NOT NULL,
        trial_ids_json TEXT NOT NULL,
        promoted_at TEXT NOT NULL
    );
    CREATE INDEX idx_recipe_promotions_recipe ON recipe_promotions(recipe_id);
    "#,
    ),
    (
        // A person's verdict on a review-rejected replay trial (plan
        // task-types D9): `ground_truth` is `pass` or `fail`, with the
        // evidence in `ground_truth_note` and the ledger-clock time in
        // `ground_truth_at`. Additive and nullable: a trial written before
        // the step is unjudged, never guessed. Like v29 and v31 it takes no
        // backup copy: no existing value is rewritten.
        "v32",
        r#"
    ALTER TABLE trials ADD COLUMN ground_truth TEXT;
    ALTER TABLE trials ADD COLUMN ground_truth_note TEXT;
    ALTER TABLE trials ADD COLUMN ground_truth_at TEXT;
    "#,
    ),
    (
        // The candidate a `kind: verify` run applies, resolved once at
        // validation (docs/plans/2026-10-14-runs-survive-a-loaded-machine.md
        // A1): one row per consuming run. The row is also the pin: while
        // the consuming run is not terminal, the source run's candidate
        // ref and patch file are kept. Expand-only: a new table, nothing
        // read by an older binary, no existing value rewritten.
        "v33",
        r#"
    CREATE TABLE candidate_inputs (
        run_id TEXT PRIMARY KEY,
        source_run TEXT NOT NULL,
        candidate_index INTEGER NOT NULL,
        candidate_commit TEXT NOT NULL,
        patch_sha256 TEXT NOT NULL,
        source_base_sha TEXT NOT NULL,
        target_base_sha TEXT NOT NULL,
        resolved_at TEXT NOT NULL
    );
    CREATE INDEX idx_candidate_inputs_source
        ON candidate_inputs(source_run, candidate_index);
    "#,
    ),
    (
        // The successor intent (plan A5): the transactional outbox row a
        // `revise` answer writes in the same transaction as the answer.
        // `decision_id` (the decided run) is UNIQUE, so a repeated answer
        // finds the same intent. The row also pins the candidate it
        // resolved (`pin_run`, `pin_index`) until the run it launched
        // ends. Expand-only: a new table, nothing an older binary reads.
        "v34",
        r#"
    CREATE TABLE successor_intents (
        intent_id TEXT PRIMARY KEY,
        decision_id TEXT NOT NULL UNIQUE,
        predecessor TEXT NOT NULL,
        contract_digest TEXT NOT NULL,
        contract_json TEXT NOT NULL,
        scope_delta_json TEXT NOT NULL,
        candidate_json TEXT,
        pin_run TEXT,
        pin_index INTEGER,
        state TEXT NOT NULL,
        run_id TEXT,
        created_at TEXT NOT NULL,
        claimed_at TEXT
    );
    CREATE INDEX idx_successor_intents_run ON successor_intents(run_id);
    "#,
    ),
    (
        // Why the last launch of a successor intent was refused before it
        // had a run (a preflight said no) and when. Additive and nullable.
        "v35",
        r#"
    ALTER TABLE successor_intents ADD COLUMN refusal TEXT;
    ALTER TABLE successor_intents ADD COLUMN refused_at TEXT;
    "#,
    ),
    (
        // How many launches of an intent were refused, so the wait before
        // the next offer can grow. Additive, defaulted.
        "v36",
        r#"
    ALTER TABLE successor_intents ADD COLUMN refusal_count INTEGER NOT NULL DEFAULT 0;
    "#,
    ),
    (
        // Where a run's accepted change landed, found once, so revert
        // detection starts from it. Additive; nothing reads it but detection.
        "v37",
        r#"
    CREATE TABLE landings (
        run_id TEXT PRIMARY KEY REFERENCES runs(id),
        landing_sha TEXT NOT NULL,
        landed_at TEXT NOT NULL,
        found_at TEXT NOT NULL
    );
    "#,
    ),
    (
        // How far a landing search got without finding the landing, so the
        // next pass resumes there. Additive; nothing reads it but detection.
        "v38",
        r#"
    CREATE TABLE landing_searches (
        run_id TEXT PRIMARY KEY REFERENCES runs(id),
        searched_to TEXT NOT NULL,
        searched_at TEXT NOT NULL
    );
    "#,
    ),
    (
        // Heavy leases that survive the process that took them (plan B2,
        // SPEC §23). `group_bindings` is one row per lease tied to a
        // process group, written when the group is bound and removed when
        // the lease is given back, so a coordinator that restarts finds the
        // holds it had. `run_epochs` counts how often a run's runner has
        // started, so a resumed run never reuses the id of a lease that
        // settled before. Expand-only: two new tables, nothing an older
        // binary reads, no existing value rewritten.
        "v39",
        r#"
    CREATE TABLE group_bindings (
        dispatch_id TEXT PRIMARY KEY,
        run_id TEXT NOT NULL,
        session_id TEXT NOT NULL,
        runner INTEGER NOT NULL,
        runner_started TEXT,
        pgid INTEGER NOT NULL,
        leader_started TEXT,
        tokens INTEGER NOT NULL,
        bound_at TEXT NOT NULL
    );
    CREATE TABLE run_epochs (
        run_id TEXT PRIMARY KEY,
        epoch INTEGER NOT NULL
    );
    "#,
    ),
    (
        // The disk each check label grew its tree by the last time it ran
        // (plan B3, SPEC §23): the estimate the coordinator reserves for
        // the next lease of the same label. Expand-only: one new table,
        // nothing an older binary reads, no existing value rewritten.
        "v40",
        r#"
    CREATE TABLE check_disk_growth (
        label TEXT PRIMARY KEY,
        bytes INTEGER NOT NULL,
        recorded_at TEXT NOT NULL
    );
    "#,
    ),
    (
        // What a receipt needs, saved before a run stops for a review that
        // did not fit or could not be had (plan review-over-budget, SPEC
        // §10): the receipt the candidate would have earned, bound to the
        // candidate, base, contract and policy hashes it was verified under.
        // `relais decide <run> approve` seals the person's receipt from it.
        // Expand-only: one new table, nothing an older binary reads, no
        // existing value rewritten.
        "v41",
        r#"
    CREATE TABLE verification_snapshots (
        run_id TEXT PRIMARY KEY REFERENCES runs(id),
        candidate_sha TEXT NOT NULL,
        base_sha TEXT NOT NULL,
        contract_hash TEXT NOT NULL,
        policy_hash TEXT NOT NULL,
        stop_reason TEXT NOT NULL,
        receipt_json TEXT NOT NULL,
        saved_at TEXT NOT NULL
    );
    "#,
    ),
    (
        // A review in parts (plan review-over-budget, Changes step 5): the
        // successor intent a `review-parts` answer writes names how the
        // successor reviews (`review`, NULL for every earlier intent), and
        // the parts manifest records each part of that review, keyed by
        // run, candidate hash and part index, with the hashes of its
        // immutable patch and result artifacts and its status. Expand-only:
        // one nullable column and one new table; no existing value is
        // rewritten, and an older binary reads neither.
        "v42",
        r#"
    ALTER TABLE successor_intents ADD COLUMN review TEXT;
    CREATE TABLE review_parts (
        run_id TEXT NOT NULL REFERENCES runs(id),
        candidate_sha TEXT NOT NULL,
        part_index INTEGER NOT NULL,
        part_count INTEGER NOT NULL,
        patch_path TEXT NOT NULL,
        patch_sha256 TEXT NOT NULL,
        dispatch_id TEXT,
        result_path TEXT,
        result_sha256 TEXT,
        status TEXT NOT NULL,
        detail TEXT,
        updated_at TEXT NOT NULL,
        PRIMARY KEY (run_id, candidate_sha, part_index)
    );
    "#,
    ),
    (
        // The holdout definitions a run resolved at preflight (ADR-0045
        // phase 3c): a successor intent judges by its predecessor's rows
        // instead of resolving machine.toml and the store again.
        // Expand-only: one new table, nothing an older binary reads, no
        // existing value rewritten.
        "v43",
        r#"
    CREATE TABLE holdout_definitions (
        run_id TEXT NOT NULL REFERENCES runs(id),
        name TEXT NOT NULL,
        definition_json TEXT NOT NULL,
        digest TEXT NOT NULL,
        PRIMARY KEY (run_id, name)
    );
    "#,
    ),
    (
        // What holdouts catch and leak (ADR-0045 phase 4): one row per
        // holdout a verification ran, with whether the profile's checks
        // were green on that candidate, and one row per tool call of a
        // worker that named a holdout location (the marker's name only,
        // never the matched text). Expand-only: two new tables, nothing an
        // older binary reads, no existing value rewritten.
        "v44",
        r#"
    CREATE TABLE holdout_outcomes (
        run_id TEXT NOT NULL REFERENCES runs(id),
        label_kind TEXT NOT NULL,
        attempt_index INTEGER NOT NULL,
        name TEXT NOT NULL,
        outcome TEXT NOT NULL,
        profile_green INTEGER NOT NULL,
        PRIMARY KEY (run_id, label_kind, attempt_index, name)
    );
    CREATE TABLE holdout_exposures (
        run_id TEXT NOT NULL REFERENCES runs(id),
        dispatch_id TEXT NOT NULL,
        tool TEXT NOT NULL,
        marker TEXT NOT NULL,
        PRIMARY KEY (run_id, dispatch_id, tool, marker)
    );
    "#,
    ),
];
