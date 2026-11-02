//! The store holdouts the person confirmed (SPEC §4): one row per store URL,
//! repository and holdout name, holding the confirmed fingerprint, the store
//! commit it was reviewed at and a retained snapshot of the reviewed
//! declaration and supplied contents. The snapshot lives here, not in the
//! fetch cache, so evicting a cached commit never loses what was reviewed.

use rusqlite::{params, OptionalExtension};

use crate::holdout::HoldoutConfirmation;

use super::{Ledger, Result};

/// What [`Ledger::confirm_holdout`] writes.
#[derive(Debug, Clone, Copy)]
pub struct ConfirmationRecord<'a> {
    pub store: &'a str,
    pub repo: &'a str,
    pub name: &'a str,
    pub fingerprint: &'a str,
    pub store_sha: &'a str,
    pub snapshot_json: &'a str,
    /// Who asked the person: `cli` or `plugin`.
    pub confirmed_via: &'a str,
}

impl Ledger {
    /// Record the confirmation of one holdout, replacing the previous one
    /// for the same store, repository and name. One statement, so the
    /// fingerprint and its snapshot are never apart.
    pub fn confirm_holdout(&self, record: &ConfirmationRecord<'_>) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO holdout_confirmations
                (store, repo, name, fingerprint, store_sha, snapshot_json, confirmed_at,
                 confirmed_via)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                record.store,
                record.repo,
                record.name,
                record.fingerprint,
                record.store_sha,
                record.snapshot_json,
                self.now(),
                record.confirmed_via
            ],
        )?;
        Ok(())
    }

    /// The confirmation of a holdout, or `None` when it was never confirmed.
    pub fn holdout_confirmation(
        &self,
        store: &str,
        repo: &str,
        name: &str,
    ) -> Result<Option<HoldoutConfirmation>> {
        Ok(self
            .conn
            .query_row(
                "SELECT fingerprint, store_sha, snapshot_json, confirmed_at
                   FROM holdout_confirmations
                  WHERE store = ?1 AND repo = ?2 AND name = ?3",
                params![store, repo, name],
                |row| {
                    Ok(HoldoutConfirmation {
                        fingerprint: row.get(0)?,
                        store_sha: row.get(1)?,
                        snapshot_json: row.get(2)?,
                        confirmed_at: row.get(3)?,
                    })
                },
            )
            .optional()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record<'a>(
        store: &'a str,
        repo: &'a str,
        name: &'a str,
        fp: &'a str,
    ) -> ConfirmationRecord<'a> {
        ConfirmationRecord {
            store,
            repo,
            name,
            fingerprint: fp,
            store_sha: "abc",
            snapshot_json: "{}",
            confirmed_via: "cli",
        }
    }

    #[test]
    fn a_confirmation_is_keyed_by_store_repository_and_name() {
        let dir = crate::test_support::short_temp_dir("ho-confirm");
        let ledger = Ledger::open(&dir.to_path_buf().join("ledger.sqlite")).expect("ledger");
        ledger
            .confirm_holdout(&record("ssh://store-a", "alpha", "report", "v1:aaa"))
            .expect("confirm");
        let held = ledger
            .holdout_confirmation("ssh://store-a", "alpha", "report")
            .expect("read")
            .expect("confirmed");
        assert_eq!(held.fingerprint, "v1:aaa");
        for (store, repo, name) in [
            ("ssh://store-a", "beta", "report"),
            ("ssh://store-b", "alpha", "report"),
            ("ssh://store-a", "alpha", "other"),
        ] {
            assert_eq!(
                ledger
                    .holdout_confirmation(store, repo, name)
                    .expect("read"),
                None,
                "{store} {repo} {name}"
            );
        }
        ledger
            .confirm_holdout(&record("ssh://store-a", "beta", "report", "v1:bbb"))
            .expect("confirm the same name in another repository");
        ledger
            .confirm_holdout(&record("ssh://store-a", "alpha", "report", "v1:ccc"))
            .expect("a new fingerprint replaces the old");
        let fingerprint = |repo: &str| {
            ledger
                .holdout_confirmation("ssh://store-a", repo, "report")
                .expect("read")
                .expect("confirmed")
                .fingerprint
        };
        assert_eq!(fingerprint("alpha"), "v1:ccc");
        assert_eq!(fingerprint("beta"), "v1:bbb");
    }

    #[test]
    fn a_confirmation_records_who_asked_the_person() {
        let dir = crate::test_support::short_temp_dir("ho-via");
        let ledger = Ledger::open(&dir.to_path_buf().join("ledger.sqlite")).expect("ledger");
        ledger
            .confirm_holdout(&ConfirmationRecord {
                confirmed_via: "plugin",
                ..record("ssh://store-a", "alpha", "report", "v1:aaa")
            })
            .expect("confirm");
        let via: String = ledger
            .conn
            .query_row(
                "SELECT confirmed_via FROM holdout_confirmations WHERE name = 'report'",
                [],
                |row| row.get(0),
            )
            .expect("read");
        assert_eq!(via, "plugin");
    }
}
