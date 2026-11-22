+++
type = "task"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais"]
due = "2026-11-23"
+++
# Confirm v0.77.0 shipped and is installed

## Question

The entry plan's Phases 1–3 merged in relais#533 and its release branch
`chore/release-v0.77.0` was landing when this programme was charted. Did
the release PR merge, did the `v0.77.0` tag's Release workflow conclude
`success` with its assets, and does `relais --version` on this machine read
0.77.0 (the plugin reinstall is the person's step)?

## Look at

- `gh pr list --repo fredericrous/relais --state merged --search "release: v0.77.0"`,
  `gh run list --repo fredericrous/relais --workflow Release --limit 3`
  and the run's `conclusion`, `gh release view v0.77.0`, `relais --version`.

## Answer looks like

The merge sha, the Release run's conclusion, the asset list, and the
installed version; if any is missing, the step that supplies it, and the
stop-row check waits on it.

## Resolution

2026-11-23 00:45 CET (2026-11-22 23:44Z). **Shipped and installed;
closed.** The stop-row check (t06) did not wait on anything and is
already closed.

- **Release PR.** relais#537 `chore(release): v0.77.0` merged
  2026-11-22 22:03Z as `50736d58`. The `v0.77.0` tag points at that
  commit.
- **Release workflow.** Run 45075566068 on `v0.77.0`: `completed`,
  conclusion `success`.
- **Assets.** The release was published 22:15Z with 7 assets:
  `relais-0.77.0-{aarch64-apple-darwin,x86_64-apple-darwin,aarch64-unknown-linux-gnu,x86_64-unknown-linux-gnu,x86_64-unknown-linux-musl}.tar.gz`,
  `relais-0.77.0-x86_64-pc-windows-msvc.zip` and `SHA256SUMS`.
- **Installed binary.** `relais --version` reads `relais 0.77.0`. The
  only `relais` on `PATH` is `/usr/local/bin/relais`, a link to
  `Cellar/relais/0.77.0`, made 22:17Z.
- **Plugin.** The Claude Code plugin is still at 0.72.0. Its hooks call
  `relais` from `PATH`, so they already run 0.77.0. Reinstalling the
  plugin is the person's step and nothing in this programme waits on it.
