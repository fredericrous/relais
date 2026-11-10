+++
type = "research"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais", "amont"]
+++
# How much check and review time relais spends that a later gate repeats

## Question

Per accepted relais run, how many hours of checks and how much review spend
does relais pay that amont's pre-push gate, the worktree-task verification
loop or implementation-review then repeat on the same tree or diff?

## Look at

- relais ledger `~/.local/state/relais/ledger.sqlite`: `check_timings`
  (phase `base`, `attemptN`, `cached`, `run_ms`), `usage_events` (reviewer
  cost by `phase`), `runs` (status, repo).
- amont's journal of pre-push gate runs and stamps for the same trees.
- Since 2026-10-10, per repository.

## Answer looks like

Per repository: baseline hours, verification hours, reviewer dollars, and the
share of each repeated later on the same tree by amont or F4b. The "before"
figure the destination measures against, with the query that produced it so
it can be rerun after.

## Resolution

Measured 2026-11-10 from the relais ledger, since 2026-10-10, worktrees
folded into their repository (`<repo>-wt-*` → `<repo>`).

| Repository | Baseline h (run) | Baseline cache hits | Verification h | Review $ | Reviewed runs |
|---|---|---|---|---|---|
| relais | 3.8 | 239/467 | 10.8 | 84.14 | 162 |
| amont | 9.1 | 0/16 | 3.6 | 5.41 | 7 |
| amont-agent | 2.7 | 13/39 | 4.5 | 97.26 | 77 |
| sre-agent | 1.5 | 0/41 | 2.2 | 32.91 | 38 |
| happier | 1.8 | 0/9 | 0.0 | 41.10 | 33 |
| adr-lint | 0.8 | 0/21 | 0.8 | 75.98 | 36 |
| grh | 0.3 | 5/23 | 0.5 | 22.06 | 20 |
| homelab | 0.4 | 0/15 | 0.4 | 16.32 | 14 |
| others (4) | 0.7 | 0/36 | 0.4 | 31.85 | 27 |

Fleet: about 21 h of baseline, 23 h of verification. Review is $440.58 of
$1,151.48 of all relais spend (38%). Only relais itself hits the baseline
cache; every other repository's base moves between runs.

**Repeated later:** not joinable from available records. The relais ledger
does not record the tree amont's pre-push gate judged, and F4b's
implementation-review has no cost ledger. Structurally, every check
repeats: relais commits with an empty `core.hooksPath`, so amont never
stamps a relais candidate, and the profiles run plain commands (`make
check`) rather than `amont run --stamp`. So amont's pre-push gate reruns
them, and every planned branch gets implementation-review at F4b on the same
diff.

The query (rerun after the build with the same date cut):

```sql
-- sqlite3 -readonly ~/.local/state/relais/ledger.sqlite
with r2 as (select id, case when instr(replace(repo_path,'/Users/fredericrous/Developer/Perso/',''),'-wt-')>0
  then substr(replace(repo_path,'/Users/fredericrous/Developer/Perso/',''),1,instr(replace(repo_path,'/Users/fredericrous/Developer/Perso/',''),'-wt-')-1)
  else replace(repo_path,'/Users/fredericrous/Developer/Perso/','') end repo from runs),
c as (select r2.repo,
 sum(case when ct.phase='base' and ct.cached=0 then ct.run_ms else 0 end)/3600000.0 base_h,
 sum(case when ct.phase='base' then ct.cached else 0 end) hits, sum(ct.phase='base') base_n,
 sum(case when ct.phase glob 'attempt[0-9]' then ct.run_ms else 0 end)/3600000.0 verif_h
 from check_timings ct join r2 on r2.id=ct.run_id where ct.queued_at>='2026-10-10' group by 1),
u as (select r2.repo, sum(coalesce(cost_micros,0))/1e6 review_usd, count(distinct u.run_id) reviewed_runs
 from usage_events u join r2 on r2.id=u.run_id where u.phase in ('review','report_review') and u.at>='2026-10-10' group by 1)
select c.repo, base_h, hits, base_n, verif_h, review_usd, reviewed_runs from c left join u using(repo);
```
