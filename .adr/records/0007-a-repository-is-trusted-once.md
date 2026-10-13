---
id: ADR-0007
status: accepted
decisions:
  - key: trust.scope
    choice: a repository is trusted once, by its identity (origin URL, else git common directory), in machine.toml's `[trust_repos]`; a change to the commands relais would run is reported once on stderr and recorded on the run, never re-asked; legacy grants bound to one exact policy keep admitting that policy
    first: true
    reason: A content-bound grant made every relais.toml edit and every learner or replay candidate a new grant, though a candidate may only change routing; the repository is written by its committers, so re-approving its contents protected nothing, and the person chose once per repository with a warning (2026-10-13)
---
# 0007 — a repository is trusted once; a change to its commands warns

## Context

relais runs a repository's verification commands, setup steps and integrations, so it asks the person before it does. Until v0.18.0 the grant was bound to the whole policy's content (`grant_key(authority_hash, identity)`, the whole `RepoPolicy`, recipes included). Every edit to `relais.toml` and every candidate recipe was a new policy, so screening the task-types replays alone needed about six grants, and every later candidate one more, although `validate_candidate` already requires a candidate's commands to be the incumbent's. The person (2026-10-13): "we have too many trust stuff. one is already a lot. what do the industry do? would warning be enough".

Other tools trust a location once: VS Code Workspace Trust (the folder), `git safe.directory` (the path), mise (the config path; content hashing only in its paranoid mode). direnv re-asks on every content change, and that is its most common complaint.

## Decision

A repository is trusted once, by `policy::repo_key` of its identity, in `[trust_repos."<repo_key>"]`, written only by `relais trust grant --reviewed-by` (the person, in a shell or through the plugin's own question). Any policy of that repository runs under it, candidates and live-trial arms included. When the commands relais would run change (verification setup and commands, integrations; never models, tiers or recipes), the first `run` that sees the change prints it on stderr and records `commands_changed`; it never blocks. Legacy `[trust."<key>"]` grants keep admitting their exact policy, and `relais doctor` counts how often they still do.

## Threat model

- The repository's contents are written by its committers. A malicious `relais.toml` arrives through a commit or a pull request the person merged, which a content hash never stopped: it asked the person to approve the same change twice.
- A grant keyed by the origin URL also covers any other clone with the same origin on this machine (a fork's branch in a scratch directory).
- What still blocks: a repository never granted, such as a fresh clone of someone else's project.
- A session in bypass-permissions mode is outside this model, as before (SPEC §5).

## Consequences

- One grant per repository; the warning replaces the re-ask, once per change.
- A machine.toml with `[trust_repos]` cannot be read by v0.18.0 (`deny_unknown_fields`). Rollback: remove `[trust_repos]`, restore the ledger from the `ledger.sqlite.bak-v30` copy the migration writes (runs recorded after the upgrade are lost), re-grant with `relais trust grant --key`.
- Removing legacy grants is the contract step of `migration.expand-migrate-contract`, in a later major, once `relais doctor` reports none matched.
