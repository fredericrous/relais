---
id: ADR-0002
status: accepted
decisions:
  - key: learning.classifier-capture
    choice: relais keeps the text of each session-router classifier call on this machine only, in 0600 day files deleted after 90 days with known secret shapes redacted, keeps only the text's hash in the ledger, can be switched off, and uses the text to train a local model; nothing leaves the machine except by an explicit export
    first: true
    reason: A local model that replaces the haiku classifier call can only be trained and measured on the requests that call saw, and the ledger deliberately holds none of their text; a copy kept under the person's own account, short-lived, redacted and switchable keeps that evidence without making the ledger, the report or a backup a place where prompts sit
---
# 0002 — relais keeps classifier inputs locally to train on

## Context

The session router (ADR-0001) sends each prompt the person types, and each
subagent the model spawns, to a small classifier model (haiku). The reply
sets the task's relation, kind, difficulty, scope, uncertainty and
confidence, and the capability table turns those into a tier. The ledger
records the decision and the classifier's token usage, never what was asked:
`router_tasks` keeps an `explicit_quote` of at most 300 bytes and nothing
else of the person's words.

The learning-loop plan (`docs/plans/2026-10-10-learning-loop.md`, Phase 5
to Phase 7) wants to replace that call with a small local model fine-tuned on
the classifier's own answers. That needs the input of each call beside its
answer; and the answer on a continuation, a correction or an explicit
accept, which the task row loses today, needs to be kept as well. The
person decided on 2026-10-10 (plan decision D2) that the inputs are kept
locally.

A prompt is the person's own words. It can hold a pasted key, a bearer
token or a private key block. Every session runs with permissions bypassed,
so no prompt asks before a file is written.

## Decision

- **The text stays local.** It is written to `<state dir>/classifier/
  <YYYY-MM-DD>.jsonl`, under a directory forced to 0700 (also when it
  already exists) and files created 0600, opened `O_APPEND`, one `write_all`
  per line. It is the only copy: it is not in the ledger, in `relais report`,
  in a log, or in a dataset.
- **The ledger holds a hash.** `router_classifications` (ledger schema 25,
  additive) has the call's id, purpose (`prompt` or `spawn`), model, prompt
  version (the SHA-256 of the classifier's system prompt), the SHA-256 of
  the text, the parsed answer with every field, whether it parsed, the
  latency and the time. It never has the text, nor the words the classifier
  quoted from it.
- **90 days.** A day file older than 90 days is deleted by any write of one
  and by every `relais learn cycle`.
- **Redaction before writing.** Known secret shapes are replaced and counted
  per line: provider API keys (`sk-`, `ghp_`, `xox*-` and kin), bearer
  tokens, private key blocks, and base64 or hex runs of 40 characters or
  more. This lowers the chance a secret is kept; it does not make a prompt
  safe to share, because a pattern list cannot know every secret or every
  private sentence.
- **An off switch.** `[session_routing] keep_classifier_inputs = false` in
  machine.toml, or `RELAIS_SESSION_ROUTING=off`, or routing turned off,
  captures nothing: no file and no row. A machine.toml that cannot be read
  captures nothing.
- **Nothing leaves except by an export.** `relais router export-classifier
  --out <file|->` writes the parsed calls with a time split. It is the one
  way the text leaves the state directory; the person runs it. It writes a
  file 0600, through a temporary file in the same directory, then renames
  it.
- **The use.** Training a local classifier and measuring it against the
  haiku answers (plan Phases 6 and 7). A local model is adopted by a later
  plan with its own gates and record.

## The stated limit

Redaction is a list of shapes, so an unusual secret, or a person's private
detail, is kept for up to 90 days in a file only the owner account can read.
The files are not encrypted, and backup tools that copy the state directory
copy them. A person who does not want this turns the switch off.

## Consequences

- `docs/router-protocol.md` has a `classification` record; the plugin sends
  one for every classifier call, fail-open. An older relais refuses the
  record kind (bad payload, exit 2), so relais is upgraded before the plugin.
- `relais report` shows the number of parsed calls kept, the count the first
  training run waits on.
- SPEC §30 describes the capture, the day files and the export.
