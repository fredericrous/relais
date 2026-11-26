# Full reviews: relais drops the aval fallbacks

Panel run 2026-11-27 by the t22 relais-slice agent; every seat on opus.

## backend (rounds 1, 2, two deltas)
- R1 approve-with-changes (38k, 85 s): adds=contract (blocking, accepted);
  every `"aval"` key string survives a pure `default_bin` (high, accepted);
  no phase produced the legacy-grant report (accepted); other hosts need
  `greentopo` (accepted); "pass" rows not observable (accepted).
- R2 approve-with-changes (52k, 40 s): doctor exits 3 with a FAIL line, not
  2 (accepted); branch doctor sweep needs a scratch `RELAIS_STATE_DIR`
  (accepted); picture wording (accepted).
- R2 final approve (56k, 19 s); low notes: name the ledger query for the
  week's count; pod note wording.
- Delta 1 approve-with-changes (62k, 29 s): the probe seam must keep
  `which`'s path-name branch and `is_file` (high, accepted).
- Delta 2 approve (64k, 12 s); low notes, carried into implementation: the
  broken-install row holds on the spawn path (`confirmed_absent`), the probe
  reports a dangling link as not available; run the declared-bin row from
  the repository root.

## lang:rust (rounds 1, 2)
- R1 approve-with-changes (50k, 137 s): one `TOPO_KEY` const (accepted);
  `INIT_TEMPLATE` writes `aval` (accepted); legacy-grant report (accepted);
  assert the serialized key (accepted).
- R2 approve-with-changes (72k, 40 s): doctor exit 3 (accepted); suite row
  restored (accepted); the probe test needs a PATH seam, never
  `env::set_var` (high, accepted).

## tui (rounds 1, 2)
- R1 approve-with-changes (34k, 83 s): the trust step was not runnable
  (high, accepted); refusal text names the key with an example (accepted);
  final strings stated (accepted); no `aval` in output (accepted).
- R2 approve-with-changes (48k, 36 s): doctor exit 3 (high, accepted);
  legacy-exact rule and order (accepted).
- R2 final approve-with-changes (54k, 26 s): `relais plan` needs a valid
  `--task`, else clap's exit 2 passes the row (high, accepted).

## unix (rounds 1, 2)
- R1 approve-with-changes (39k, 81 s): pin the refusal's exit and streams
  (accepted, corrected by other seats); broken-install row (accepted);
  parse, not grep, for the gate (accepted).
- R2 approve-with-changes (43k, 17 s): suite row restored (accepted).
- R2 final approve (51k, 24 s); low, carried: doctor's exit 3 is not
  specific to the refusal; grep its output for "is no longer read".

## architect (rounds 1, 2; seated when adds=contract was added)
- R1 approve-with-changes (54k, 102 s): `docs/SPEC.md` still describes the
  fallback (high, accepted); `Integrations::entries()` instead of copied
  triples (accepted); the `default_bin` test was a tautology (accepted).
- R2 approve (58k, 21 s); low: runner is `greengate run` (applied).

## po (rounds 1, 2; seated when adds=contract was added)
- R1 approve-with-changes (27k, 31 s): gate over repositories with no
  checkout (high, accepted); hosts named (accepted); a post-release count
  (accepted, without a rollback phase: a rollback moves the hash again);
  a real-repository row (low, later applied).
- R2 approve-with-changes (33k, 19 s): Forgejo orgs in the gate (applied);
  the real-repository row (applied).

codex: not run (programme ticket agent; not installed check skipped).
