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
2. **Start** the `worktree-task` skill on branch `prog/<slug>/<id>` in the
   map's repository. The branch tells every other session that the ticket
   is in flight, and the skill's board check shows who else works nearby.
   The `prog/<slug>/<id>` branch is created in the map's repository even
   when the ticket's `repos` name others; work in another repository goes
   on that repository's own branch, named in the Resolution. To give a
   ticket up unresolved, delete its `prog/` branch locally and on origin.
   An in-flight ticket whose branch has not moved for days is worth asking
   the person about.
3. **Resolve** the ticket by its type (the table above). Then:
   - write its `## Resolution` and set `status = "closed"`;
   - add its line to Decisions so far;
   - turn fog that the answer made precise into new tickets, and remove it
     from Not yet specified;
   - rule out tickets past the destination: `status = "out-of-scope"`, a
     line under Out of scope, and their open dependents ruled out too or
     their `blocked_by` changed.
4. **Done** when `relais programme check <dir>` exits 0 and the pull
   request has merged.

One ticket per session, except research, which may be fanned out.
