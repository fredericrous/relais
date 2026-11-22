+++
type = "research"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais"]
+++
# How a missed failure is detected for a drawn task

## Question

The session-router plan defines a missed failure as a later correction that
edits the same files within 24 h, or a revert, on a task the router drew
down. The ledger records `corrected` on the task itself, `files_json`,
`parent_task_id` and the run-level revert detection (`learn::revert`). What
query, over which tables, yields a per-stratum missed-failure count and the
denominator the Clopper–Pearson upper bound needs, and what is undetectable
(an `answered` question has no files; a correction after the next task
starts never reaches the earlier task)?

## Look at

- `docs/plans/2026-10-07-session-router.md` lines 170–180 (the measure)
  and §5 gate 4.
- `crates/relais/src/ledger/router.rs` (`files_json`, `completed_at_end`,
  `outcome_source`, the rank rule), `crates/relais/src/learn/revert.rs`,
  `claude-plugin/hooks/router.ts` (`outcomeOf`, correction detection,
  `startTask`).
- `docs/router-protocol.md` (the outcome enums and `completed_at_end`).

## Answer looks like

The SQL (or the Rust accessor to add) producing, per stratum, drawn tasks,
drawn tasks with a missed failure, and the same for undrawn; a list of the
cases it cannot see, each with why; and the figure for the two ready review
strata on a `.backup` copy.

## Resolution

Measured 2026-11-22 on a `.backup` copy of the live ledger (4,579
`router_tasks`, latest 2026-11-22T22:28Z). Strata and arms as in t01:
subagent tasks of kind `review` joined to their `relation = 'subagent'`
decision; drawn = explored, applied, not held out; undrawn = not explored,
propensity set, not held out. Run in-session (needs the live ledger copy
and transcripts), not through relais.

### What a review's miss is

A review edits nothing (1,601 of 1,603 strata tasks have `files_json =
'[]'`) and every strong outcome it holds is its parent's
(`outcome_source = parent`, written by `task_outcome` only over an own
`unknown`/`answered`). So the files to watch are the **parent's**, and the
window starts at the **parent's** end: a completed review R with parent P
(same session, `P.task_id = R.parent_task_id`, `P.agent_id = ''`) counts a
missed failure when, in `(P.ended_at, P.ended_at + 24 h]`:

- (a) a main-session `correction` decision is recorded on a task B (any
  session) whose `files_json` shares a hash with P's; or
- (b) a `revert` reassess row's `files_json` shares a hash with P's; or
- a `flag` reassess row names P's files (none exist: `flag` has 0 rows).

A correction inside P after R ended makes P `corrected`, which reaches R
through `task_outcome` (rank 2), so R is then not completed: gate 2 counts
it, gate 4 does not. For a main task the same rule applies with P = the
task itself.

### The query

```sql
-- strata review tasks with their arm
CREATE TEMP VIEW s AS
SELECT t.session, t.task_id, t.outcome, t.parent_task_id,
  CASE WHEN t.difficulty = 3 AND t.scope = 'module' THEN 'd3 module'
       WHEN t.difficulty IN (4,5) AND t.scope = 'cross-cutting' THEN 'd4-5 cc' END AS stratum,
  (d.explored AND d.applied AND NOT d.holdout) AS drawn,
  (NOT d.explored AND d.propensity IS NOT NULL AND NOT d.holdout) AS undrawn
FROM router_tasks t
JOIN router_decisions d ON d.session = t.session AND d.task_id = t.task_id
 AND d.relation = 'subagent'
WHERE t.class_kind = 'review';

-- completed ones, with the parent's end and files
CREATE TEMP VIEW c AS
SELECT s.*, p.ended_at AS p_end, p.files_json AS p_files
FROM s LEFT JOIN router_tasks p
  ON p.session = s.session AND p.task_id = s.parent_task_id AND p.agent_id = ''
WHERE stratum IS NOT NULL AND (drawn OR undrawn)
  AND s.outcome IN ('completed_verified', 'completed_accepted');

-- stop: hashes of paths outside every repository (see "cannot see", 1)
CREATE TEMP TABLE stop(h TEXT PRIMARY KEY);

CREATE TEMP VIEW m AS
SELECT c.*,
  (parent_task_id IS NOT NULL AND p_files IS NOT NULL AND p_files != '[]') AS linked,
  EXISTS (SELECT 1 FROM router_decisions d
          JOIN router_tasks b ON b.session = d.session AND b.task_id = d.task_id AND b.agent_id = ''
          WHERE d.relation = 'correction' AND d.agent_id = ''
            AND julianday(d.at) >  julianday(c.p_end)
            AND julianday(d.at) <= julianday(c.p_end) + 1
            AND EXISTS (SELECT 1 FROM json_each(b.files_json) bf
                        JOIN json_each(c.p_files) pf ON bf.value = pf.value
                        WHERE bf.value NOT IN (SELECT h FROM stop))) AS miss_corr,
  EXISTS (SELECT 1 FROM router_reassess r
          WHERE r.event = 'revert'
            AND julianday(r.at) >  julianday(c.p_end)
            AND julianday(r.at) <= julianday(c.p_end) + 1
            AND EXISTS (SELECT 1 FROM json_each(r.files_json) rf
                        JOIN json_each(c.p_files) pf ON rf.value = pf.value
                        WHERE rf.value NOT IN (SELECT h FROM stop))) AS miss_rev
FROM c;

SELECT stratum, drawn,
  count(*) AS completed, sum(NOT linked) AS unlinked, sum(linked) AS measured,
  sum(linked AND (miss_corr OR miss_rev)) AS missed,
  count(DISTINCT CASE WHEN linked THEN session || parent_task_id END) AS parents,
  count(DISTINCT CASE WHEN linked AND (miss_corr OR miss_rev)
                      THEN session || parent_task_id END) AS missed_parents
FROM m GROUP BY 1, 2;
```

The Rust accessor to add is this query as
`Ledger::router_missed_failures(stop: &[String]) -> Vec<MissRow>` (one row
per completed exploration task: stratum keys, arm, `linked`, `missed`,
`parent` key), with the counting and the bound in `router::stats`
(`clopper_pearson_upper` exists). The denominator is **measured**
(linked) tasks, never all completed ones: an unlinked review has no
files to check and would count as clean.

### The figures

With the stop list (668 hashes: the absolute paths of every file in the
memory directory and `~/.claude/CLAUDE.md`; 153 of them occur in task
records):

| Stratum | Arm | Completed | Unlinked | Measured | Missed | Bound (per review) | Parents | Missed parents | Bound (per parent) |
|---|---|---|---|---|---|---|---|---|---|
| d3 module | drawn | 27 | 3 | 24 | 0 | 0.143 | 19 | 0 | 0.177 |
| d3 module | undrawn | 288 | 36 | 252 | 0 | 0.015 | 62 | 0 | 0.058 |
| d4-5 cc | drawn | 23 | 9 | 14 | 0 | 0.232 | 11 | 0 | 0.285 |
| d4-5 cc | undrawn | 243 | 66 | 177 | 0 | 0.021 | 56 | 0 | 0.064 |

Bounds are Clopper–Pearson 95% upper at 0 misses. Gate 4 fails on both
strata on sample size, as t01 found, now on the smaller measured
denominators: 24 and 14 linked drawn reviews (t01 had 27 and 23).

**Without the stop list** the same query finds 45 misses (24 undrawn d3,
20 undrawn d4-5, 1 drawn d3) on 5 parents. Four of the five overlap only
on one hash, `1ed8db4facf41d44`, the absolute path of the memory index
`MEMORY.md`, which 84 main tasks in 55 sessions edited. The fifth
overlaps on another memory file. All 45 are false.

**Misses cluster on parents.** A parent spawns 4 to 11 reviews; one parent
miss marks all of them. A per-review binomial bound counts them as
independent and overstates n by 1.3–4.1× here. For a review stratum, gate
4's "≥ 36 completed drawn" should read 36 distinct parents, with the bound
on parents. The same holds for attribution: a review that found the defect
the parent then shipped is charged the parent's miss. With t01's parent
note, that goes to t03.

### What it cannot see

1. **Paths outside the session cwd collide across sessions.** `fileHash`
   hashes `repoRelative(file, cwd)`, which leaves a path outside the cwd
   absolute. Memory files, `~/.claude/CLAUDE.md` and any shared absolute
   path match across unrelated sessions (above: 45 false misses). The
   query masks them with a stop list. The fix is in the plugin: drop
   paths outside a repository before hashing.
2. **No repository key.** Two repositories' `Cargo.toml` or `AGENTS.md`
   hash the same. A cross-session match is unsafe until the plugin hashes
   a repository id (the git common dir) with the path. Today a same-session
   match is the only trustworthy one.
3. **No recorded revert can ever match.** None of the 93 `revert` rows
   shares a hash with any task's files, its own task included. In a
   checked session (943bdf5e, cwd `~/Developer/Perso`, edits in
   `relais-wt-replay-floors/…`):
   - `revertedPaths` read `git restore $P` and hashed the literal `$P`
     (`09235618e3da7da7`);
   - it resolves paths against the session cwd, not the `cd <worktree>`
     the command ran, so even an expanded path would hash
     `docs/plans/…` against the edit's `relais-wt-replay-floors/docs/plans/…`;
   - 10 rows hash the pathspec `.` and 2 are `git revert` (`[]`), both
     recorded as unresolved by design (`holds-until: R2`).
   That command was also a save-and-restore around a tool call, not an
   undo of failed work. The smallest fixes: resolve paths against the
   command's effective directory (`cd`, `git -C`); skip arguments that
   still hold `$`; for `.` and `git revert`, resolve at R2 as the comment
   plans.
4. **A correction as a new task.** The classifier reads `correction`
   only against an active task. A complaint after a `/clear`, in a new
   session, or after the task closed with no task open is a `new_task`
   and never a correction. Of 12,966 main-session classified turns, 26 are
   corrections and 840 start new tasks.
5. **Correction rows carry no files.** `router_reassess` `correction` rows
   have `files_json = '[]'`, so the overlap uses all of B's files, those
   edited before the correction included. That over-counts.
6. **Plan versus code, double count.** The plan says a correction inside
   open task B on A's files counts "as A's miss only, never also as B's
   `corrected`". The hook marks B `corrected` regardless
   (`router.ts` `isCorrection` → `task.corrected = true`), so gate 2 counts
   B too.
7. **Unlinked reviews.** Reviews recorded before the parent link
   (`parent_link_recorded = 0`), and parents that edited nothing, have no
   files to check. That covers 3 of 27 and 9 of 23 completed drawn
   reviews.
8. **An `answered` review never enters the measure.** It holds no files
   and is not completed. Neither is a question's `answered`, so for a
   question stratum gate 4 has no signal at all.
9. **`/relais-flag` never used.** It is wired, with 0 rows. The "no flag
   miss" half of gate 4 is vacuous until someone uses it.
10. **`learn::revert` cannot join.** It detects reverts of accepted relais
    *runs* from git history, but `router_tasks` carries no run id. A
    task that ended `completed_verified` through a relais run cannot
    inherit that run's detected revert. The smallest addition is the run
    id on the task record.
11. **Window edge.** A correction more than 24 h after the parent ends is
    not seen. Today, a task that ends while the next one starts gets its
    late correction attributed to the next task (item 6).

### Readout

The miss measure is computable today as the SQL above. On the two ready
strata it reads 0 misses, but gate 4 still cannot pass on them:
- the drawn samples are too small (24 and 14 measured, 19 and 11 parents);
- the revert channel is blind (item 3);
- cross-session matches need the stop list and a repository key
  (items 1–2).

Plugin fixes to ship before any stratum activates on gate 4, in order:
1. resolve revert paths;
2. drop out-of-repository paths;
3. key hashes by repository;
4. record correction files.

Questions for t03:
- Should gate 4 for review strata count parents?
- Is the parent's miss a review's miss at all?
