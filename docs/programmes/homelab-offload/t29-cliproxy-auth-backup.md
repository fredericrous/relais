+++
type = "research"
mode = "afk"
status = "closed"
blocked_by = ["t27-cliproxy-on-homelab"]
repos = ["homelab"]
+++
# How the CLIProxyAPI auth volume is backed up and restored

## Question

The proxy rewrites its OAuth files on every refresh, so the RWO PVC holding
them changes on its own. Which backup that the homelab already runs covers
it? How often does it run, and how is the volume restored without leaving
two copies of the same account live at once (t28)?

## Look at

- t27's Resolution (the PVC, `rook-ceph-block`, one replica).
- Homelab backup tooling: grep `kubernetes/homelab` for Velero, VolSync,
  snapshot or backup CronJobs, and the restore drills in the homelab memory
  notes (the Happier relay restore drill).

## Answer looks like

The backup chosen, with its schedule and retention and the manifest that
shows how another PVC is backed up the same way. A restore runbook of at
most five steps. The answer to "can a restore revive a token another proxy
has already refreshed?"

## Resolution

Explore research on Haiku, 2026-11-07. The schedules and the snapshot class
were checked against homelab `origin/main`.

- **Backup:** the existing Velero schedules, with CSI snapshots
  (`ceph-block-snapshot`, labelled for Velero) moved to the `cluster-garage`
  store. Daily at 02:00 kept 7 days, weekly kept 28, monthly kept 365
  (`kubernetes/homelab/backup/configs/schedules.yaml`). Namespaces are
  opted in by name, so the deploy plan adds `cliproxy` to all three lists.
  Secrets are excluded, which is fine: the auth files live on the PVC.
- **Off-site:** unverified. A 2026-09-25 note says the NAS sync copies
  Velero metadata only, not the volume data. Check it in the deploy plan.
- **A restored token may be dead.** Refresh tokens rotate (RFC 9700 §4.14;
  unverified for Anthropic and OpenAI), so any snapshot may hold one the
  provider has already revoked. Restore therefore assumes a fresh sign-in,
  not replay. The volume is backed up for the config and access keys, and a
  more frequent schedule would not help.
- **Restore runbook** (`docs/operations/backup-restore.md`):
  1. Scale the proxy to 0, and confirm that no other proxy holds the
     accounts (t28).
  2. Run `bootstrap recover homelab velero --namespace cliproxy --dry-run`,
     then with `--force`.
  3. Delete the restored token files, then sign each account in again
     through the panel.
  4. Scale the proxy to 1, check each account, and make one test call.
  5. Record which accounts were signed in again.
- **Open:** no restore drill exists for this volume yet. The deploy plan
  runs one.
