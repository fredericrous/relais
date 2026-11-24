+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
due = "2026-12-02"
+++
# A week of relays adopts every handoff

## Question

f12 made a session a `/clear` relay starts adopt its predecessor's
handoff (`relais programme adopt`, run by the relais plugin). Once the
plugin is reinstalled with that change, does every relayed handoff become
its successor's `work` within seconds, so no handoff lapses while its
successor is alive? The count decides whether f12 is done or the adoption
needs a second trigger, such as a SessionStart hook in the Rust binary.

## Look at

- `~/.local/state/relais/claims/log.jsonl`: each `create` of a `handoff`
  claim, the `takeover` that follows (`kind = "work"`, by another session)
  and its delay, and each `expire` of a handoff.
- The plugin's `relais.programmeEvents` store: `adopt` events by result
  (`ok`, `older`, `error`).
- The session transcripts under `~/.claude/projects/`, to tell whether a
  handoff's successor was alive when the handoff expired.

## Answer looks like

Over the 7 days after the reinstall: N relayed handoffs, the number
taken over within 10 s of the successor's start, the number that expired
while their successor was alive (target 0), and the `adopt` error count.
Then one of: done, or the next change with its cause.
