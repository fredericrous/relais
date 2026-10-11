//! The `[learning]` promotion envelope in machine.toml (SPEC §17, relais
//! ADR-0003): what the person granted once, so the learning cycle may
//! promote a task artifact that passed every gate.
//!
//! The writer is the session-routing envelope's: [`crate::trust::edit_machine`]
//! takes the lock, validates, keeps comments and the file's mode, and
//! renames atomically. Mechanism only; who may call it (the installer, and
//! `relais native learning-envelope` for the person's own command) is the
//! caller's rule.

use std::path::Path;

use crate::policy::{AutoPromote, AutoPromoteOff};
use crate::trust::GrantError;

/// `text` with `[learning]`'s `remove` key gone and `key = { … }` set from
/// `fields`, everything else as it was.
fn replace_learning_key(
    text: &str,
    remove: &str,
    key: &str,
    fields: Vec<(&str, toml_edit::Value)>,
) -> Result<String, GrantError> {
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| GrantError::Unwritable(e.to_string()))?;
    let table = doc
        .entry("learning")
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .ok_or_else(|| GrantError::Unwritable("`learning` is not a table".into()))?;
    table.remove(remove);
    let mut inline = toml_edit::InlineTable::new();
    for (name, value) in fields {
        inline.insert(name, value);
    }
    table.insert(
        key,
        toml_edit::Item::Value(toml_edit::Value::InlineTable(inline)),
    );
    Ok(doc.to_string())
}

/// `text` with `[learning] auto_promote = { … }` set and any `off`
/// tombstone removed, everything else as it was.
pub fn set_auto_promote(text: &str, grant: &AutoPromote) -> Result<String, GrantError> {
    replace_learning_key(
        text,
        "off",
        "auto_promote",
        vec![
            ("granted_at", grant.granted_at.clone().into()),
            ("by", grant.by.clone().into()),
            ("source", grant.source.clone().into()),
        ],
    )
}

/// `text` without `[learning] auto_promote` and with the `off` tombstone
/// set, everything else as it was.
pub fn set_off(text: &str, off: &AutoPromoteOff) -> Result<String, GrantError> {
    replace_learning_key(
        text,
        "auto_promote",
        "off",
        vec![
            ("at", off.at.clone().into()),
            ("by", off.by.clone().into()),
            ("source", off.source.clone().into()),
        ],
    )
}

/// Write the grant to machine.toml (locked, atomic, comments and mode kept).
pub fn write_auto_promote(machine_path: &Path, grant: &AutoPromote) -> Result<(), GrantError> {
    crate::trust::edit_machine(machine_path, |text, _| {
        set_auto_promote(text, grant).map(Some)
    })?;
    Ok(())
}

/// Record `grant` only when machine.toml has neither a grant nor an `off`
/// tombstone, under the same lock as the check: a grant the person
/// recorded is never replaced, and promotion they turned off stays off.
/// Whether it was written.
pub fn write_auto_promote_if_absent(
    machine_path: &Path,
    grant: &AutoPromote,
) -> Result<bool, GrantError> {
    crate::trust::edit_machine(machine_path, |text, current| {
        if current.learning.auto_promote.is_some() || current.learning.off.is_some() {
            return Ok(None);
        }
        set_auto_promote(text, grant).map(Some)
    })
}

/// Turn automatic promotion off (the same locked writer): the grant
/// removed, the tombstone recorded. Whether a grant was removed.
pub fn write_off(machine_path: &Path, off: &AutoPromoteOff) -> Result<bool, GrantError> {
    let mut had_grant = false;
    crate::trust::edit_machine(machine_path, |text, current| {
        had_grant = current.learning.auto_promote.is_some();
        set_off(text, off).map(Some)
    })?;
    Ok(had_grant)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::MachineSettings;

    fn grant() -> AutoPromote {
        AutoPromote {
            granted_at: "2026-10-10T00:00:00Z".into(),
            by: "me".into(),
            source: "install".into(),
        }
    }

    fn off() -> AutoPromoteOff {
        AutoPromoteOff {
            at: "2026-10-10T01:00:00Z".into(),
            by: "me".into(),
            source: "plugin-ask".into(),
        }
    }

    const BEFORE: &str = "# mine\nschema_version = 1\n\n# kept\n[learning]\n";

    #[test]
    fn the_grant_is_set_and_everything_else_kept() {
        let after = set_auto_promote(BEFORE, &grant()).unwrap();
        assert!(after.starts_with(BEFORE), "{after}");
        let machine = MachineSettings::from_toml_str(&after).unwrap();
        assert_eq!(machine.learning.auto_promote, Some(grant()));
    }

    #[test]
    fn off_replaces_the_grant_and_a_grant_replaces_off() {
        let with = set_auto_promote(BEFORE, &grant()).unwrap();
        let turned_off = set_off(&with, &off()).unwrap();
        let machine = MachineSettings::from_toml_str(&turned_off).unwrap();
        assert_eq!(machine.learning.auto_promote, None);
        assert_eq!(machine.learning.off, Some(off()));
        let again = set_auto_promote(&turned_off, &grant()).unwrap();
        let machine = MachineSettings::from_toml_str(&again).unwrap();
        assert_eq!(machine.learning.auto_promote, Some(grant()));
        assert_eq!(machine.learning.off, None);
    }

    #[test]
    fn the_install_grant_never_replaces_a_grant_or_a_tombstone() {
        let dir = crate::test_support::short_temp_dir("learning-envelope");
        let path = dir.join("machine.toml");
        assert!(write_auto_promote_if_absent(&path, &grant()).unwrap());
        assert!(
            !write_auto_promote_if_absent(&path, &grant()).unwrap(),
            "a grant already there is left alone"
        );
        assert!(write_off(&path, &off()).unwrap());
        assert!(
            !write_auto_promote_if_absent(&path, &grant()).unwrap(),
            "promotion the person turned off stays off"
        );
        let text = std::fs::read_to_string(&path).unwrap();
        let machine = MachineSettings::from_toml_str(&text).unwrap();
        assert_eq!(machine.learning.off, Some(off()));
        assert_eq!(machine.learning.auto_promote, None);
    }
}
