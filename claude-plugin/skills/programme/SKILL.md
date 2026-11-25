---
name: programme
description: Chart or resume a programme, which is work too big for one session, as a map of decision tickets in the repository. Use whenever a new project or initiative starts; when the person says it is big, too big for one session, or to plan it as a programme; and when they ask what is next on a programme, or to resume one.
---

# programme

A programme is work that needs more than one session, or that holds
decisions nobody can state as a question yet. It is charted as a **map**,
`docs/programmes/<slug>/map.md`, in the repository that owns the outcome.
Beside the map is one **ticket** file per decision. Any session resumes the
programme from the map and `relais programme frontier`, not from pasted
context.

Triage first:

- **Small**: describable in one sentence. Write the change, no plan.
- **Medium**: fits one session, and every decision can be stated now. Use
  plan mode.
- **Big**: more than one session, or a decision that cannot be stated yet.
  Use this skill.

## The files

`map.md` has these sections, in this order:

```markdown
## Destination
<what reaching the end looks like: a spec, a decision, or a change in place>
## Notes
<domain, skills every session reads, standing preferences>
## Decisions so far
- [<ticket name>](<id>.md): <one-line gist>
## Handoff
<optional: where the last session left off; replaced at each boundary, never appended>
## Not yet specified
<the fog: questions you can see coming but cannot state yet>
## Out of scope
- [<ticket name>](<id>.md): <why it is past the destination>
```

A ticket, `<id>.md` (the id is the file stem, for example `t03-storage`):

```markdown
+++
type = "research"        # research | prototype | grilling | task
mode = "afk"             # afk (agent alone) | hitl (with the person)
status = "open"          # open | closed | out-of-scope
blocked_by = ["t01-auth-model"]
repos = ["relais", "amont"]   # the repositories it touches; default: the map's
+++
# Which object store holds the lake
## Question
<the decision this ticket resolves>
## Look at
<afk: the files, runs or pages to read first>
## Answer looks like
<afk: the shape of a finished answer, so a run knows when it is done>
## Resolution
<when closed: the answer, and links to assets, ADRs, PRs and runs>
```

A `task` ticket may also hold a follow-up measure that a plan schedules for
later: it carries a due date, and `relais programme frontier` lists dated
tickets first, soonest first, marking the overdue ones.

```markdown
+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
due = "2026-11-19"       # task tickets only; a quoted YYYY-MM-DD
+++
# Recount the settle timeouts a week after the release
## Question
<the measure to take, and what its result decides>
```

Text the person reads names a ticket by its `# ` name, never by its bare
id.

## Ticket types

| Type | Mode | Resolve it with |
|---|---|---|
| research | afk | a relais `inspect` run or an Explore agent; several may run at once |
| prototype | hitl | a cheap artifact to react to: a mockup, a stub, a rough take |
| grilling | hitl | AskUserQuestion, two to four options, recommendation first; the person answers, never you |
| task | afk or hitl | a relais run, or an exact checklist for the person; it only unblocks a decision |

A ticket resolves a decision, not a deliverable. The build happens
afterwards, as ordinary plans. When the answer is a lasting decision for
every repository, record it as an ADR, and link the ADR from the
Resolution.

## Chart a new programme

1. **Name the destination.** Grill the person until the Destination is one
   or two lines they agree with. Done when it is written in `map.md`.
2. **Map the frontier, breadth first.** List the open decisions across the
   whole space before going deep on any one. If no fog shows up AND the
   work fits one session, stop and say "medium tier: use plan mode".
   Multi-session work with every decision clear still gets a map. Done when
   each decision is a ticket or a line under Not yet specified.
3. **Write the files.** Make a question a ticket when you can state it
   precisely now, even if it is blocked; otherwise it is fog. An afk
   ticket requires `## Look at` and `## Answer looks like`; `check` warns
   on an open afk ticket without them. Done when
   `relais programme check docs/programmes/<slug>` exits 0 and
   `relais programme frontier docs/programmes/<slug>` lists at least one
   ticket.
4. **Fan out research.** Start one run per research ticket. Wait for every
   run, then write each Resolution, `status = "closed"`, and the
   Decisions-so-far line yourself; the runs write nothing in the programme
   directory. Done when `check` exits 0.
5. **Commit** through the `worktree-task` skill. Charting resolves nothing
   but research.

## Work a ticket, or resume a programme

0. **Find the map.** Run `relais programme list` (from `~/Developer/...`
   or any repository beside the one that holds it) and match on the
   destination or the programme's name. If nothing matches, ask the
   person; never chart a second map for the same work. The path may be in
   a linked worktree (a programme with unpushed work there), or absent on
   disk when the last column is a ref (origin alone holds it); `frontier`
   on that path reads the same source. Done when one map path is chosen.
1. **Read** `map.md`: when `list`'s last column is a ref, with
   `git show <ref>:docs/programmes/<slug>/map.md` in the repository, since
   the file may be absent on disk. Then run `relais programme frontier <dir>`. Take the
   ticket the person named, or the first frontier ticket. Done when one
   ticket id is chosen.
2. **Claim** the ticket before any work. Run
   `relais programme claim <slug> <id> --dir <map dir>` (the map's directory
   `docs/programmes/<slug>`, since the current directory may be another
   repository's worktree), and run it again at each commit on the ticket: it
   refreshes the claim, which the session's end deletes. Read its exit status:
   - 0: the ticket is yours; go on.
   - 3: another live session holds it. Run
     `relais programme next --from <repo>:<slug> --claim`, which reserves one
     unheld ready ticket of this programme, never of another, for a handoff
     and prints `<repo> <slug> <id>`. Then
     run `relais programme claim <slug> <id> --dir <map dir>` on what it
     names, and only after it exits 0 start any work: the reservation is a
     handoff, not active work. If that claim exits 3 too, another session
     took the reservation over and it is theirs: do not release it. Select
     again, at most 3 selections in all, then park.
   - 22 (from `next`): nothing in this programme is unheld and due. Park:
     end the answer with `safe to /clear; parked: next due <date>`, with
     the first line `next` printed (`parked: …`), and no `next:` prompt.
     When `next` also printed `ready elsewhere: <repo>:<slug> (<n> ready)`
     lines, name those programmes for the person just above the parked
     line, and start none of them: starting another programme is the
     person's call.
   - 1 from `next` with `stayed held for` on stderr: the programme's lock
     was busy. Run the `next:` command it printed once more; if it is still
     busy, write that command for the person and park with
     `safe to /clear; parked: lock busy`.
   - any other non-zero exit: report the stderr line and stop. Print no
     clear line, and do not switch ticket or programme on a configuration or
     I/O failure.

   Done when the claim exits 0.
3. **Start** the `worktree-task` skill on branch `prog/<slug>/<id>` in the
   map's repository. The branch tells every other session that the ticket
   is in flight, and the skill's board check shows who else works nearby.
   The `prog/<slug>/<id>` branch is created in the map's repository even
   when the ticket's `repos` name others; work in another repository goes
   on that repository's own branch, named in the Resolution. To give a
   ticket up unresolved, delete its `prog/` branch locally and on origin.
   An in-flight ticket whose branch has not moved for days is worth asking
   the person about.
4. **Resolve** the ticket by its type (the table above). Then:
   - write its `## Resolution` and set `status = "closed"`;
   - add its line to Decisions so far;
   - turn fog that the answer made precise into new tickets, and remove it
     from Not yet specified;
   - rule out tickets past the destination: `status = "out-of-scope"`, a
     line under Out of scope, and their open dependents ruled out too or
     their `blocked_by` changed;
   - file this session's follow-ups as `task` tickets (step 5).
5. **Write the handoff**, without being asked, in the ticket's closing
   commit, before the merge; never in memory or a file outside the
   repository. When the `worktree-task` skill is installed, follow its
   Handoff section. Otherwise:
   - file the follow-ups you noticed as `task` tickets in this map, and
     the inbox, when it exists, of every repository this session touched:
     `inbox="$(git rev-parse --path-format=absolute --git-common-dir)/greenharness/inbox.md"`.
     A line reads `<date> <session8> <text>`, where `session8` is the
     first 8 characters of `CLAUDE_CODE_SESSION_ID`. Take the file first
     (`mv "$inbox" "${inbox%.md}.<session8>.filing"`). File this session's lines
     and those dated two or more days ago, append the others back with one
     `>>` each, then delete the taken file;
   - replace the map's `## Handoff` section: date, what merged, the live
     state, what is next by ticket name, accepted gaps, and any steps left
     to the person.
6. **Done** when `relais programme check <dir>` exits 0 and the pull
   request has merged. Then, once nothing of this session is in flight (no
   uncommitted or unpushed work, no run or background task still going, no
   question unanswered, no worktree left to remove), run
   `relais programme next --from <repo>:<slug> --claim` and read its exit as
   step 2 does, also when the `worktree-task` skill is absent. Print a
   `next:` prompt only on exit 0, naming the ticket it reserved, as this
   line, last, with nothing after it; the prompt is one line of at most
   2,000 characters with no backtick:

   ```
   safe to /clear; next: `resume <repo> <slug> <id>: <gist>`
   ```

   On exit 22 or a busy lock, end with the parked line of step 2 instead.

A single-ticket session works one ticket, except research, which may be
fanned out. Steps 4–6 are its close; a driver closes its tickets as the
next section says.

## Drive a programme

Once the person approves a programme, that approval covers its tickets: no
ticket plan is approved again, and no `next:` prompt waits for the person
(ADR-0080). One **driver** session runs the map: it claims, starts one agent
per ready ticket, closes what they report, and clears itself between waves.
The panel and the implementation review still run on every ticket; their
recorded verdicts are the audit record.

### The loop

0. **Check the source.** Run `relais programme list --json`. The map's
   `source` must be a ref, not `working-tree`: a checkout with local
   programme work ignores the closures on origin. Otherwise stop with one
   line, `cannot drive <map dir>: local programme work in <checkout>; land
   or push it, then drive again`.
1. **Read the frontier.** Run `relais programme frontier <dir> --json`,
   without `--no-fetch`, and parse stdout only (`[]` is nothing ready).
   Keep the open tickets with every `blocked_by` closed, no `prog/` branch
   and no live claim. Skip a ticket whose `due` is after today (compare the
   date; `overdue: false` does not tell today from later), and one the
   map's `## Handoff` lists as held.
2. **Claim what can start now.** Capacity is 3 minus this driver's
   unfinished ticket agents, capped by the free slots in
   `greengate slots --json`; an agent waiting on a review still holds its
   slot. Claim only that many, each with
   `relais programme claim <slug> <id> --dir <dir>`. One session may hold
   several claims. Read each exit status, and its stderr, since exits 1 and
   3 each carry more than one cause:
   - 0: start its agent.
   - 3 with `is held by` on stderr: another live session holds it; skip it.
   - 1 with `stayed held for` on stderr: the lock timed out; retry once,
     then skip it this round.
   - 2: no session id, or the ticket is not open; stop the loop and report
     it.
   - any other exit, or a 1 or 3 without those words: report the stderr
     line and stop.

   Never start a ticket whose claim did not exit 0. With capacity 0 and
   ready tickets left, wait (recheck every 2 minutes, at most 30 minutes,
   printing `waiting: <n> ready, 0 slots free`), then report: no capacity
   is not the end of the programme.
3. **Start the agents** in one message: one background `general-purpose`
   agent per claimed ticket, never a fork (a fork carries the driver's
   whole context and inherits its plan mode). Its brief is the ticket file,
   the map's Destination, the ticket protocol below, and the driver's
   session id for `relais programme claim`. Never enter plan mode while a
   drive runs: a running agent inherits it and stops.
4. **Report and refill.** Print one status line, never a question, at each
   change: `<ticket name>: started | closed <PR> | held: <reason>`. When an
   agent reports, close it as its type says, run step 1 again and start
   what became ready. A ticket never waits for a slower sibling: each
   closure frees its own dependents.
5. **Resume.** A ticket whose agent died keeps its `prog/` branch and reads
   in flight; relaunch an agent on that branch. A restarted driver takes
   over only open tickets whose `prog/` branch has no live claim, no commit
   in the last 8 hours, and no `held` line in the Handoff, and claims each
   before relaunching.
6. **Wave boundary.** When nothing is in flight (every agent started has
   reported, no PR of the driver's is open) and work is left, do not start
   the next wave in this context:
   - reconcile the reports (below) and write the map's `## Handoff` (the
     held tickets and why) in one PR, in a
     `worktree-task` worktree, never the main checkout;
   - merge it with `merge-when-green`, run step 1 again to name the next
     wave;
   - when the session started less than 2 minutes ago, wait until it is
     2 minutes old: the clear-relay does not fire twice within 120 s;
   - end the answer with the line, last:

     ```
     safe to /clear; next: `resume <repo> <slug> drive: <the next wave's ticket ids>`
     ```

   The clear-relay clears and submits it; the fresh session starts this
   loop at step 0 and claims the wave again. Take the boundary every time,
   whatever the context size. When agents always overlap and no boundary
   comes, stop starting agents once the context passes 200k tokens: the
   ones running report, and that makes the boundary. Never hand off with an
   agent of the driver still running.
7. **Stop** when nothing is running and nothing ready is left, after the
   reconcile. Write the Handoff in one PR, then send **one** summary: the
   decisions agents took, the held tickets and why, the steps only the
   person can run, the tickets waiting on a due date, and the programmes
   `relais programme next --from <repo>:<slug>` lists under
   `ready elsewhere:`, which the driver does not start: starting one is the
   person's call. Each line names the
   ticket by its `# ` name, its file (`docs/programmes/<slug>/<id>.md`),
   and the exact command or edit the person runs. End with exactly one
   closing line: work left for the person → `safe to /clear; next:`; only
   tickets not yet due → `safe to /clear; parked: next due <date>`; the
   map done → none.

### The ticket protocol

What each agent does, by the ticket's type:

- **research**: an inspect run; the agent returns the Resolution text. The
  driver lands it with `status = "closed"` and the ticket's Decisions line
  in its own PR (a `worktree-task` worktree, `merge-when-green`), then runs
  step 1, so the dependents start in this drive.
- **a code or config change**: the agent runs the whole `worktree-task`
  loop itself, in its own transcript, and messages the driver only at the
  end:
  1. starts `worktree-task` on `prog/<slug>/<id>` and writes the plan as
     `docs/plans/<date>-<slug>.md`, without plan mode;
  2. runs the `plan-review` skill on that file. A `rework` that survives
     round 2 holds the ticket;
  3. lands the plan commit, implements, verifies, and writes the ticket's
     Resolution with `status = "closed"` and its own `(<id>.md)` line under
     Decisions so far, so the reviewed tree is the pushed tree;
  4. runs F4b itself: `greenbelay tree-sha --block`, then
     `implementation-review` and the declared profiles. The push check reads
     them from the session's side transcripts. A rework that survives its
     delta holds the ticket;
  5. pushes with `amont land` and runs `merge-when-green`. When a sibling
     merged first and the rebase changes the tree, it runs step 4 again on
     the new tree before the push;
  6. reports once: `DONE <PR>` or `HELD <reason>`, the fog its answer made
     precise, its follow-ups, and anything now outside the Destination.
- **grilling**: the agent takes its recommended option and returns the
  decision and why; the driver closes it as research.
- **prototype**: the agent picks its recommended approach, then builds the
  artifact or runs the experiment the ticket asks for: a recommendation does
  not answer an empirical question. A code change goes through the code
  protocol, an evidence-only result through research's. A prerequisite only
  the person can give holds it.
- A grilling or prototype Resolution reads
  `decided-by: agent (ADR-0080); reopen: set status = "open" in docs/programmes/<slug>/<id>.md`.
- **Held, not failed**: a ticket is held when it needs a secret, `sudo` or
  a physical step, when a rework survives its delta, or when a UI change has
  no `evidence:` preview. Its dependents wait; the rest of the map runs.

### Who writes the map

A ticket agent writes only its ticket file and its own Decisions line; a
sibling's line in conflict is a rebase (protocol step 5). The driver writes
every other section. It **reconciles** each report in its own worktree:
new tickets for the follow-ups, clarified fog removed from Not yet
specified, tickets past the Destination ruled out with their dependents
updated. A new ticket within the Destination runs in this drive, at most 5
per drive; the rest are filed and listed in the summary. Then
`relais programme check <dir>` exits 0, and step 1 runs again. An empty
frontier does not end the drive while fog or follow-ups are unfiled:
fog that stays unresolved is held or ruled out of scope, with its reason.
