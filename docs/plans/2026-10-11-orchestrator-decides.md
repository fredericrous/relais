---
status: active
branch: feat/orchestrator-decides
repos: [relais, homelab]
adrs:
  - relais ADR-0004 (who answers a decision, and the optional decision advisor)
---

# Decisions the orchestrator answers, recorded, advised by Laya

## Review panel

👉 **Decide:** none — approve if a typed, recorded orchestrator answer with Laya advice (once trained) is how decisions get settled.
📍 relais, homelab · plan reviewed, nothing built · next: relais PR 1 (Phases 1–2), then the Phase 3 sweep. Panel: architect, backend, go, python, rust, typescript, platform, po, tui, unix.
**Changed by review:** advice computed once when the row opens, never on a read path; orchestrator cannot sign off human criteria; advisor ships with 4c, not PR 1.
**Verdicts:** round 1: 9 approve-with-changes, 1 approve; round 2 and two deltas backend approve-with-changes; 1 blocker went to the person (D8: tamper-evidence); 5 findings carried into implementation.
📄 Full reviews: [2026-10-11-orchestrator-decides.reviews.md](2026-10-11-orchestrator-decides.reviews.md)

## Context

A relais run that cannot settle itself stops `needs_decision`, `needs_review` or `interrupted`, preserves its candidate and opens a row in `decisions` (`State::awaits_a_person`, `crates/relais/src/lifecycle.rs:430`). Only `relais decide <run> --answer … --actor …` (`decide_command`, `crates/relais/src/main.rs:6824`) closes that row and moves the run to a terminal state. In practice the root Claude Code session, the orchestrator, is the one that settles most of these, but nothing leads it to record what it settled:

- the plugin registers `run`, `status`, `onboard`, `trust`, `replay`, and no `decide` tool;
- the outcome message ends "This is yours to decide" (`runner/mod.rs:2393`) and never names `relais decide`;
- the skill says "make it explicitly" (`claude-plugin/skills/relais/SKILL.md:113`), not "record it".

The live ledger (2026-10-11): **29 decisions open**, the oldest from 2026-09-30 (10 `needs_decision`, 9 of them `scope_exceeded`; 13 `needs_review`; 6 `interrupted`), so `relais status` lies: it lists runs as waiting that nobody is waiting on. Of 158 answered, 49 were answered by the orchestrator, under seven actor spellings (`claude`, `claude-orchestrator`, `claude (for the person)`, `fredericrous`, `person`, …), so the ledger cannot tell who decided. Answering the stale rows does not by itself feed learning (answered runs land in Cancelled/AcceptedByPerson, `learn/dataset.rs:162-165`); only the contained salvages the Phase 3 sweep finds become labels.

The person decided (2026-10-11): **the orchestrator approves**, and **Laya advises the approval only — it never gates it**. Laya is the 421M ModernBERT decision model the learning-loop plan trains on the homelab (`2026-10-10-learning-loop.md` Phases 6–7). Inference runs on this Mac if it can (2019 Intel i7-9750H, 32 GB, x86_64), else as a homelab service.

**Intended outcome:** every decision the orchestrator makes is recorded through one tool, with an actor that says who decided; the outcome message names that step; the 29 stale rows are answered from what actually landed; and once a Laya "approve?" model exists, its calibrated advice is computed once when the decision opens, shown with the outcome and stored beside the answer, so its agreement with the person can be measured.

## Phase 1: one actor vocabulary (Rust) — done

- `relais decide --actor` takes one of three values, `orchestrator`, `person`, `person-via-orchestrator` (the person said it, the orchestrator typed it), instead of a free string. A typed `Actor` `ValueEnum` in `ledger`, stored as its string; each variant and the `--actor` help carry a doc comment (help today: "Who is recording this decision", `main.rs:347`). No free-text name: the machine has one person. `--actor` is required (no default). An old spelling is refused with a message naming its replacement (`--actor claude` → "`claude` is now `orchestrator`"). `--actor person-via-orchestrator` is refused without `--person-quote <text>`, stored in a new `person_quote` column (same schema step); `--actor person` is refused when stdin is not a TTY (the person answers from their own terminal), so a model shelling out past the Bash reminder cannot write `person`. Implementation checks whether a `!` prompt command has a TTY; if not, the refusal says to use a terminal or the tool. Each answer also records `recorded_via` (`plugin` | `cli`, same schema step). The plugin proves its calls with a per-session nonce relais gives it at `session.start`, passed to `relais decide` inside a JSON frame on its stdin (`fx.process.run` takes `stdin`; it has no extra-descriptor option, `register.ts:49-50`), so the value never appears in argv, an env var or a file. relais keeps only its SHA-256, per session, in the ledger, and deletes it at `session.end`; a stale or wrong nonce records `recorded_via = cli`. A `person-via-orchestrator` answer counts as the person's (for `--criterion` and for the 4c gate and `export-decisions`) only when `recorded_via = plugin`; `person` counts only from a TTY. This is tamper-evidence, not prevention: a model that deliberately evades it (for example `script -q /dev/null`) is still possible, and its row shows `recorded_via = cli` (person, 2026-10-11, D8).
- The same type goes to `DecisionAnswer.actor` (`ledger/mod.rs:2130`; `resolve_decision`, `:3209`, writes its string), `SalvageAnswer` (`main.rs:6955`, salvage path `ledger/mod.rs:3293`) and `human_signoffs.actor` (table `ledger/mod.rs:1164`, `record_human_signoff` `:3474`).
- Human criteria: relais refuses `--criterion` unless the actor is `person` (from a TTY) or `person-via-orchestrator` with `recorded_via = plugin`, since `main.rs:6888-6916` reseals the receipt claiming a human sign-off. An orchestrator never signs off a human criterion.
- Migration, one schema step that adds `actor_raw`, `person_quote`, `recorded_via` and the 4b advice columns (`advice_p_approve`, `advice_model`, `advice_revision`, `advice_null_reason`) together. Existing rows get `recorded_via = 'legacy'` and no quote; legacy rows are never labels for the advisor and never count toward the 4c gate:
  - It first queries `SELECT DISTINCT actor` from `decisions` and `human_signoffs` and refuses, naming the spelling, if any is unmapped. The map covers every spelling in the live ledger: `claude`, `claude-orchestrator` → `orchestrator`; `claude (for the person)`, `Frédéric Rousseau (via Claude Code)` → `person-via-orchestrator`; `fredericrous`, `person` → `person`. The SQL `CASE` has no silent `ELSE`.
  - The original spelling is kept in `actor_raw`; reads parse leniently.
  - The migration code backs up with `VACUUM INTO '<state>/ledger.sqlite.bak-v<schema>'` then renames (a file copy can miss WAL pages; `ledger/mod.rs:1741`), and applies in one transaction.
  - Rollback: an older binary refuses the bumped schema (`SchemaAhead`, `ledger/mod.rs:1781-1790`); revert = install the previous tag, restore the backup, replay Phase 3 from the row → answer table this plan records. Answers recorded after the backup are otherwise lost.
- `relais decide --json` prints `{run, resolution, actor, from_state, to_state, advice}` on success (today: one `println!` at `main.rs:6949`, and salvage's at `~7015`); a refusal under `--json` prints `{error, exit}` to stdout with the same exit codes (`main.rs:879-889`: 0 accepted, 2 invalid input, 10 unknown run, 1 operational). `advice` is the stored row advice (null until 4b ships).
- `docs/SPEC.md` §9 (`:303`) documents the three actors, `recorded_via` and the nonce, the criterion rule and the `--json` shapes and exit codes in the same commit.
- Learning: `AcceptanceRoute::PersonApproved` (`learn/dataset.rs:422`) is split by actor. An orchestrator `approve` becomes its own route `OrchestratorApproved`, excluded from labels until measured. An orchestrator `salvaged` keeps the existing strict containment test (`PersonSalvageContained`, `dataset.rs:61-64,211,244,391`), because that evidence is mechanical (the verified patch reverse-applies), renamed/recorded with its actor. `LABEL_POLICY_VERSION` bumps to 3.
- Tests: each actor round-trips; `--actor claude` is refused naming `orchestrator`; an unknown `--actor` is refused with the three values listed; `decide --answer approve --criterion X --actor orchestrator` exits InvalidInput and the receipt is unchanged; the migration against a copy of the live ledger leaves 0 unmapped rows and `actor_raw` equal to the originals, and an unmapped fixture spelling makes it refuse; dataset fixtures for orchestrator-approved (no label) and orchestrator-salvaged (contained → label); a `decide --json` integration test on a scratch ledger writes the fixture the TS test consumes. The free-string fixtures ("a person", "a reviewer", `ledger/mod.rs:5454-5755,6747,8384`; `dataset.rs:1124,1171`) are rewritten to the type.

## Phase 2: `mcp__relais__decide` and the outcome message (TypeScript + Rust text) — done

- `claude-plugin/hooks/register.ts`: a `DECIDE_TOOL = 'mcp__relais__decide'` constant beside the others (`:32-36`), `$.tool.register` in `session.start` beside `status` (`:172`), and an `on('tool.call', {tool: DECIDE_TOOL})` handler like `status`'s (`:279`). Input `{run, answer, note?, successor?, candidate?, criterion?, on_behalf_of_person?: boolean, person_quote?: string}`; missing `run`/`answer` → `{deny}`. The plugin sets `--actor orchestrator`, or `person-via-orchestrator` when `on_behalf_of_person` is true; the model never types an actor string. It calls `relais decide … --json` through `fx.process.run`, parses the JSON (never stderr), returns `{result}`, and passes relais's `{error, exit}` refusal through verbatim as the result — relais stays the only judge of what may be answered.
- `on_behalf_of_person` cannot be self-asserted: the plugin accepts it only when a person prompt arrived in this session after the outcome was delivered (seen through `onPersonPrompt` in `register.ts`), and only with a `person_quote` that is a substring of that prompt; the plugin passes it as `--person-quote`. Otherwise the tool refuses with a message saying so.
- `claude-plugin/hooks/guards.ts`: a Bash `relais decide …` from the model is denied with a message pointing at the tool (beside the trust guard, `:72`). It is a reminder, not a boundary (as `guards.ts:58-61` documents for the others): it matches only when `relais` is the command word (start of the command, or after `;`, `&&`, `||`, `|`, `$(`), so `grep -rn "relais decide" docs/` is allowed. The guarantee that an answer carries a typed actor is relais's required `--actor`. The person's own `! relais decide` in the prompt is not a model tool call and is unaffected.
- No answer is withheld from the orchestrator: `approve` included (person, 2026-10-11). `approve` is still refused by relais while verification gaps remain (`main.rs:6918`); that refusal is policy, not the actor's.
- Each decide writes one pane line through `note` (`runs.ts:73`): run, answer, actor, and the stored advice when there was one (Phase 4b).
- Outcome text: the plugin composes it (`runs.ts` `onDone`, headline + facts + `NEXT_STEP[code]`); the runner supplies only `detail`. For the `AWAITS_A_PERSON` outcomes (`runs.ts:39`), append one closing line: decide, then record it with `mcp__relais__decide` (approve / revise / reject / salvaged / abandon / decided); when the request or approved plan does not answer it, ask the person and record it `on_behalf_of_person` with their words as `person_quote`. The runner's "This is yours to decide, not a reviewer's" (`runner/mod.rs:2393` and the other `Terminal::NeedsDecision` details) becomes "not a reviewer's to decide", since the reader is the orchestrator. `freshen`/`answeredText` (`runs.ts:112`, `:159`) are unchanged: an answered outcome still says "Nothing is waiting on you".
- Skill (`claude-plugin/skills/relais/SKILL.md` steps 4 and Rules, Done when): "a decision is not made until it is recorded with `mcp__relais__decide`"; when the orchestrator may decide itself (the request or approved plan answers it) and when it asks the person.
- Tests (`claude-plugin/tests/`, a new `decide.test.ts` in the `consent.test.ts` style, faking the `relais decide` argv and using the Rust-produced `--json` fixture): the tool sets the actor; `on_behalf_of_person` with a quote from a later person prompt maps to `person-via-orchestrator`, and without a prompt or with a quote not in it is refused; a relais `{error, exit}` comes back as a tool error with relais's text; the outcome message names the tool and lists all six answers; the closing line is absent for terminal and answered outcomes; a decision recorded through the tool makes the next `freshen` rewrite. `guards.test.ts`: `relais decide`, `cd x && relais decide`, `$(relais decide …)` denied; `grep -rn "relais decide" docs/` and `echo relais decide` allowed. `claude-plugin/tests/outcome.test.ts:307,377,389,407,469` updated (they assert "This is yours to decide").

## Phase 3: answer the 29 open rows (live, agent-run)

- The list comes from `Ledger::open_decisions` (`ledger/mod.rs:3169`, already used by the report). For each open row: read the run (`relais explain`), its contract objective and candidate sha, and look for what landed (`git log --all` on the run's repo for the objective or the candidate's patch, the strict salvage test D1 of the learning-loop plan: does the verified patch reverse-apply on the landed tree).
- Answer each with `mcp__relais__decide`, actor `orchestrator`, a note naming the evidence: `salvaged --candidate <sha>` when the patch landed intact; `revise --successor <run>` when a later run carried the work; `abandon` otherwise. No `approve` in the sweep: an approval months after the fact has no candidate anyone checked.
- The result (row → answer → evidence) is recorded in this plan; it is also the replay table for the Phase 1 rollback. `relais status` afterwards shows no run waiting from before 2026-10-11.

## Phase 4: Laya advises the approval (data-gated)

### 4a: can this Mac run Laya? (spike, before anything is built)

- PyTorch stopped shipping macOS x86_64 wheels after 2.2.2 and ONNX Runtime after 1.23.2. So the local path is: export the fine-tuned model to ONNX on wk-3 (laya-lab) with `attn_implementation="eager"`, `reference_compile=False`, checked by `onnx.checker`; quantize int8 (~420 MB); run it on the Mac with `onnxruntime==1.23.2` (the last x86_64 wheel, CPU, AVX2), pinned and hash-locked.
- Wheels verified first: `uv pip compile --python-platform x86_64-apple-darwin --only-binary :all:`; `requires-python` pinned to a version with an onnxruntime 1.23.2 macOS x86_64 wheel.
- Measure on this Mac, end to end and cold, with the load checked first (`uptime`, no stray >90% CPU process; memory: Gatekeeper holds a fresh script's first exec 0.7–6.4 s): 30 full `laya-advise < req.json` process runs at 512 and 1,024 tokens, p50/p95; page cache dropped, and the first run after reboot measured separately; peak RSS. Use the zero-shot pinned Laya checkpoint for the spike (accuracy does not matter here, only cost).
- **Gate:** p95 ≤ 8 s at 1,024 tokens (under the 10 s advisor deadline), RSS ≤ 3 GB, and int8 vs fp32 calibrated `p_approve` within 0.02 over the same 50 inputs → local. Otherwise → homelab service (4b').
- The numbers are recorded here either way; the RSS figure sets the 4b' requests/limits.

### 4b: the advisor contract (relais, Rust)

- relais never links a model (SPEC §16: an optional experiment must not become a runtime requirement). An **advisor** is an external command named in `machine.toml` `[decision.advisor] command = [...]`, absent by default. relais writes one JSON request on its stdin (`{run, raised_state, raised_reason, objective, detail, changed_paths, diff_stats, verification_summary}`; no file contents) and reads `{p_approve, model, revision, latency_ms}`.
- Called once, never on a read path: the runner calls the advisor when the decision row opens, after the transition commits, outside the write transaction, off the run's critical path, and stores the result on the row (`advice_p_approve`, `advice_model`, `advice_revision`, or `advice_null_reason`). Neither `relais native status --run` nor `relais decide` calls it: `freshen` calls native status with `FRESHEN_TIMEOUT_MS = 1_000` (`claude-plugin/hooks/runs.ts:36`, `:122`), and a 10 s advisor there would make every outcome fail open stale. Absent, failed or timed out → advice null with the reason, never a refusal (fail-open).
- Process contract: not `procs::run_with_timeout` as is (it merges stdout/stderr and adds a 5 s drain grace, `procs.rs:500-524`), but a variant: stdout separate and capped at 64 KiB, one JSON object parsed; stderr forwarded to relais stderr with a prefix; one total deadline of 10 s; the advisor spawned in its own process group and the whole group killed on deadline (uv→python, sh→curl grandchildren); stdin written on its own thread; `BrokenPipe` on stdin → `advice null (advisor closed stdin)`, never an error.
- `native status` `current.decision` and the `done` line carry the stored advice; plugin `onDone` prints it in the outcome message ("Laya: p(approve)=0.83, rev …") so the orchestrator sees it before deciding; `decide --json` echoes it, and the decide pane line shows it. It is labelled advisory; nothing branches on it.
- Tests: stub advisors that sleep 15 s, close stdin (`sh -c 'exec 0<&-; sleep 15'`), flood 10 MB to stdout, and write to stderr; each ends in ≤ ~10 s, the run exits 0, no process is left (`pgrep`). With the 15 s stub, `native status --run` p95 stays < 1 s and freshen still rewrites.

### 4b': homelab fallback (if 4a fails)

- The same Python package as the local advisor, packaged like `services/bge-reranker-v2-m3-int8/Dockerfile` (model-only image + python runtime), no Go. A `laya-advise` Deployment in `llms` on wk-3 CPU behind the internal gateway, mTLS client cert; the advisor command is a `curl` with the cert.
- One replica, no PDB (the advisor fails open), requests/limits from the 4a RSS measurement, a readiness probe; image pull time noted (~420 MB over the ~100 Mb/s uplink ≈ 35 s).

### 4c: train the "approve?" model (homelab, data-gated)

- Starts when the ledger holds ≥ 200 decisions answered by `person` or quote-backed `person-via-orchestrator` on `needs_decision`/`needs_review`, counted separately (today ≈ 100 across all states, ~30 approvals; the gate is checked by `relais report`). Until then the advisor is unconfigured and Phases 1–3 stand alone.
- `relais dataset export-decisions` (0600, `--out -`) emits (request JSON as in 4b, label = approve vs. not) from `person` (TTY) and `person-via-orchestrator` with `recorded_via = plugin` answers only (never `legacy` or `cli`), temporal split; laya-lab (learning-loop Phase 6 infrastructure, same window rules) fine-tunes one yes/no question, fits a temperature on the calibration split, and exports ONNX int8 alongside `report.json`.
- **Bars** before the advisor is configured, evaluated only when the temporal test split holds ≥ 30 approvals and ≥ 30 non-approvals: ECE (5 bins) ≤ 0.10 and Brier reported; AUROC lower 95% bound (DeLong, or stratified bootstrap) ≥ 0.70; reported precision at p ≥ 0.8. The temperature is fitted only with ≥ 30 calibration rows, else one shared temperature (as learning-loop.md:97). Below the sample floor the report says `not measurable (n=k)` and waits; below a bar, it says which and the run repeats at 400.
- `laya-advise` lives in homelab `services/laya-lab/`, beside the training code and under its ruff/pyright/pytest CI gate (learning-loop.md:94), not relais `scripts/` (relais `make check` runs no Python gate, relais Makefile:21,30). pytest covers the stdin/stdout contract: a malformed request and a missing model each exit non-zero, and stdout carries only JSON.
- Local (4a passed): installed once on the Mac with `uv sync --frozen`; the advisor command is `.venv/bin/python -I -m laya_advise` (never a syncing `uv run`). Homelab: 4b'.

## Delivery

- **relais PR 1:** Phases 1 and 2 (one commit per phase), including the one schema step that already adds the advice columns, and ADR-0004 recording both decisions. ADR-0004: who answers a decision (the orchestrator may answer any of them, actors typed; it never signs off a human criterion), and the advisor is an optional external command, advisory only, fail-open, consistent with SPEC §16, not amending it. It cites `learning.classifier-capture` (ADR-0002) by key and states the refinement: the decision request (no file contents: objective, reason, detail, changed paths, diff stats, verification summary) leaves the Mac only on the homelab path, only to the person's own homelab over mTLS, only when the person configures that advisor command (opt-in, absent by default), per the person's 2026-10-11 decision D3; on the local path nothing leaves. `relais report` gains person answers per week (`person` and quote-backed `person-via-orchestrator` separately) and the projected date the 4c gate is reached. `make check` before push, implementation review, merge on green, release through `tag-release`. The person's step after release: `relais install --claude --user --write`, `relais coordinator stop`, restart sessions.
- **Live:** Phase 3 sweep with the released plugin; Phase 4a spike (no code shipped).
- **Later, data-gated:** Phase 4b and 4c together (it serves nothing until a model exists): the advisor call, process contract and plugin display, plus `export-decisions`, in a second relais PR, and the laya-lab change with `laya-advise` in homelab, like learning-loop D5. If at day 30 after release `relais report` projects the 4c gate beyond 6 months, 4c may not start and the person is told once.

## Measure (after release)

- `person-via-orchestrator` rows with `recorded_via = cli`: `relais report` counts them, and any above 0 is said once.
- Open decisions older than 24 h: target 0 a week after release; > 5 means the outcome text or tool does not work.
- Share of answers since release with a typed actor: target 100%.
- Person answers per week (`person`, quote-backed `person-via-orchestrator`), against the 4c gate projection.
- Tool refusals that are not relais policy refusals: any in week 1 → revisit the guard.
- After 4b ships: advice coverage, timeouts, failures, and agreement with person answers in `relais report`.

## Verification

- `make check` in the worktree exits 0: fmt clean, clippy `-D warnings` clean, cargo test green (MSRV, audit included), `make plugin` green.
- Migration on a `VACUUM INTO` copy of the live ledger, in a scratch `RELAIS_STATE_DIR`: 0 unmapped rows, `actor_raw` equal to the originals, the backup file present; an older binary on the migrated copy refuses with `SchemaAhead`.
- `relais decide <run> --answer approve --criterion X --actor orchestrator` on a scratch ledger exits 2 (InvalidInput), the receipt unchanged; `--actor claude` exits 2 naming `orchestrator`; `--actor person` with stdin not a TTY exits 2; `--actor person-via-orchestrator` without `--person-quote` exits 2; a CLI `--actor person-via-orchestrator --person-quote x --criterion X` (no plugin nonce) exits 2 with the receipt unchanged, and a CLI-recorded `person-via-orchestrator` row is absent from `export-decisions`; a plugin call with the session's nonce writes `recorded_via = plugin`, and one with a stale or wrong nonce writes `cli`; after the migration, `recorded_via` has 0 nulls and `sqlite3` on the ledger finds no plaintext nonce; `decide --json` refusals print `{error, exit}` with the matching exit code.
- Live, after release: a small relais run that ends `needs_decision` in a scratch repo; the session receives the new outcome text, calls `mcp__relais__decide --answer revise`; `relais status` shows it cancelled, the row's `actor = orchestrator`; a second one answered "for the person" after a real person prompt, with a quote, shows `person-via-orchestrator` and the quote in the note; the same without a prompt is refused by the tool; `approve` on a run with an open gap is refused with relais's text.
- 4b PR, with stub advisors in a scratch `RELAIS_CONFIG_DIR`: `echo '{"p_approve":0.5,…}'` → the advice appears in the outcome message, on the row and in `decide --json`; the sleep-15 s, closed-stdin, 10 MB-flood and stderr stubs each end in ≤ ~10 s, the run exits 0, `pgrep` finds no advisor process; with the 15 s stub, `native status --run` p95 < 1 s over 30 calls and freshen still rewrites.
- Ledger work only against a scratch `RELAIS_STATE_DIR` until release (memory: a branch binary migrates the real ledger).

## Verification record (2026-10-11, before push)

- `make check` on the rebased tree (onto `1d8ef93`) → exit 0: fmt, clippy `-D warnings`, 1,324 lib tests plus the integration suites, MSRV, audit, plugin 240/240. Observed.
- Migration on a `VACUUM INTO` copy of the live ledger (scratch `RELAIS_STATE_DIR`, branch binary `relais report`): expected 25 → 27 applied steps, every actor mapped, 0 nulls, a backup → observed 27 steps; actors `orchestrator` 43, `person` 91, `person-via-orchestrator` 13, 32 open rows NULL; `recorded_via` all `legacy`, 0 nulls; `ledger.sqlite.bak-v27` written. The installed 0.12.1 on the migrated copy → refuses: "27 applied migration(s), this relais knows 25 — a newer relais wrote this ledger".
- Same copy, `relais decide`: `--answer approve --criterion X --actor orchestrator --json` → `{"error": "`--criterion` signs off a human criterion and an orchestrator never does …", "exit": 2}`, rc 2. `--actor claude` → `` `claude` is now `orchestrator` ``, rc 2. `--actor person` with stdin `/dev/null` → refused naming the terminal, rc 2. `person-via-orchestrator --person-quote … --proof-stdin` with the minted nonce → `{"actor":"person-via-orchestrator", …, "to_state":"cancelled"}`, row `recorded_via = plugin` with the quote. `orchestrator --proof-stdin` with the ended nonce → accepted, row `recorded_via = cli`.
- Same copy, the remaining refusals: `--actor person-via-orchestrator` without `--person-quote` → `{"error": "… requires `--person-quote <text>` …", "exit": 2}`; `--answer approve --criterion X --actor person-via-orchestrator --person-quote …` with no proof → `{"error": "`--criterion` is refused for `--actor person-via-orchestrator` without the plugin's proof …", "exit": 2}`; `--actor a-stranger` → `unknown actor `a-stranger` — one of: orchestrator, person, person-via-orchestrator`, exit 2. After all three: 0 sign-off rows for the run, its decision still open. "Receipt unchanged" after a criterion refusal is asserted by `decide_actor.rs` (`an_orchestrator_never_signs_off_a_criterion_and_the_receipt_is_untouched`, `a_stale_nonce_cannot_sign_off_and_the_receipt_is_untouched`), both green.
- `relais native decide-nonce --session s` → 64 hex characters; the ledger holds `decide_nonces(session, 64-char hash)` and no file of `ledger.sqlite*` contains the plaintext (byte search).
- `relais report` on the copy → `advisor data: 1 of 200 answers counted as the person's (0.2/week over 30 days: person 0, via the orchestrator 1); the gate opens in about 853 weeks`.
- Plugin (`claude plugin test`): the decide tool's argv, proof only on stdin, the quote gate (not delivered here; written before the outcome; under 12 characters; not whole words; accepted), relais's `{error, exit}` passed through verbatim, the Bash reminder's denied and allowed forms, the outcome closing line present for `needs_decision` and absent for `accepted` and answered outcomes. Observed in the 240/240 run.
- Not observable before release: the tool inside a live Claude Code session (needs the released plugin installed) — that is Phase 3's first step, and the live-check row of Verification above.

## Not in scope

- Laya gating any answer (the person chose advisory only).
- Laya's other jobs (classifier, model choice): learning-loop Phases 6–8.
- Changing which runs reach `needs_decision`.
- `relais feedback --actor` (`main.rs:293`) stays a free string.

## Decision log

- **D1** The orchestrator may answer every decision, `approve` included; relais's own gap refusal still applies (person, 2026-10-11).
- **D2** Laya advises only; it never gates (person, 2026-10-11).
- **D3** Laya inference on this Mac if the 4a gate passes, else a homelab service (person, 2026-10-11).
- **D4** The advisor trains only on `person` and quote-backed `person-via-orchestrator` answers.
- **D5** The orchestrator never signs off a human-sign-off criterion; relais refuses `--criterion` for actor `orchestrator`, and for `person-via-orchestrator` unless `recorded_via = plugin`.
- **D6** Advice is computed once at raise time and stored on the decision row; no read path calls the advisor.
- **D7** 4b ships with 4c in a later relais PR, not in PR 1; PR 1 only adds the advice columns in its one schema step.
- **D8** Forged person answers: tamper-evidence through a plugin nonce (sent on stdin, stored hashed) and `recorded_via`, not prevention (person, 2026-10-11).
- **D9** The actor step is ledger schema **27**, not 26: `main` took 26 for `review_outcomes` (#201) while this branch was open. The first commit's subject still says "v26"; the code, SPEC, CHANGELOG and backup name (`ledger.sqlite.bak-v27`) say 27.
- **D10** `claude-session` (a spelling another session wrote on 2026-10-11, after the plan) maps to `orchestrator`; the migration's refusal of an unmapped spelling is what caught it, on the live-ledger copy.
- **D11** The decide tool's plugin tests live in `tests/guards.test.ts`, not a new file: measured three times, a fifteenth test file made 13–19 unrelated tests time out at their 5 s budget on this machine (load 130–215), and the same tests in fourteen files passed 240/240.
- **D12** `record_salvage_contained` takes an `Answerer {actor, person_quote, recorded_via}` rather than eight arguments (clippy `too_many_arguments`; the lint is kept).
- **D13** Phase 1a went through relais first: run `run-65d8bdbaebfc4-12c71` was interrupted when the coordinator restarted mid-run (answered `abandon`); run `run-65d8c0c602ccb-e32c` wrote the change and ran out of its 45-minute wall clock repairing an unrelated plugin timing flake under load 135. Its candidate was applied and finished here; Phases 1b, 1c and 2 were done in the session. The salvage of `e32c` was recorded with a malformed `--candidate` (an `sqlite3` error string, from a wrong column name); relais refuses a second answer, so the row stays and says `candidate contained: unknown`. What reads it: the dataset's containment backfill (`learn/salvage.rs:284`) asks git for that candidate, fails, and leaves it unknown, which `salvaged_labelling` excludes; `relais feedback` on that one task would name the malformed candidate. No label is affected.
- **D14** Medium review finding 10 (backend round 2) is implemented: the plugin keeps `lastPersonPrompt` before the `relais` filter, records `outcomeDeliveredAt[run]` when the outcome is submitted, requires a quote of at least 12 characters in whole words, and refuses an outcome delivered in another session.

## Implementation review

- **approve-with-changes**, round 1 (60k tokens, 45 s).
- Fixed: the plugin fallback no longer claims "Nothing was recorded" (a timeout may follow a recorded answer); the missing refusals are now observed in the record; D13 names what reads the malformed salvage candidate.
- Deliberate: the chained `.catch` on `on('tool.call', …)` is the plugin API's fallback handler, the same pattern as the onboard and trust tools (`register.ts:242-252`), not a misplaced try/catch.
- Noted: the PR does not claim `export-decisions` (Phase 4c).
- Delta (tree `dbc7f28a`): **approve** (64k tokens, 9 s); all four findings resolved, none new.
- Next: merge, release, reinstall the plugin, then Phase 3 (the live check and the sweep of the open rows).

<!-- panel: repos=relais,homelab reviewers=architect,backend,lang:go,lang:python,lang:rust,lang:typescript,platform,po,tui,unix body-sha=fdb0e4b341aa -->
