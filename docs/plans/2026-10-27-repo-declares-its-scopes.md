---
canonical: github:fredericrous/aval/docs/plans/2026-10-27-repo-declares-its-scopes.md
phases: [4]
status: active
branch: feat/scoped-decisions
---
# A repository declares the scopes it is in

Pointer. The plan and its review panel live in aval (aval#46, released 1.11.0);
the ADR is decisions ADR-0046 (decisions#102). This repository carries relais's
part of Phase 4: the reviewer's decisions block names the scope that answered,
a cross-scope contradiction keeps its head ids and `by_scope` in the blocked
message, the `holds-until:` on default-scope verdicts goes (its condition is
met: aval answers at the repository's declared `in_scopes`), and SPEC §7 says so.
