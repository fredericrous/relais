//! Two `relais doctor` findings about the state directory: whether the
//! automatic prune of ended runs is keeping up, and which ledger backups
//! sit beside the ledger. Both only read; nothing here deletes a file.

use super::{Finding, Level};
use crate::prune::{self, AutoPruneSetting};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// An ended run older than the prune age plus this is a prune that is not
/// working, whatever the stamp says.
pub const PURGE_GRACE: Duration = Duration::from_secs(3 * 86_400);

const DAY: Duration = Duration::from_secs(86_400);
const MAX_BACKUP_LINES: usize = 10;
const RELAIS_BACKUP_NAMES: [&str; 2] = ["ledger.sqlite.bak-v27", "ledger.sqlite.bak-v30"];
const SAFE_TO_DELETE: &str = "safe to delete once you will not roll back";
const NOT_RELAIS: &str = "not written by relais";

/// The facts the purge-health finding is made of, all read by the caller.
pub struct PurgeFacts {
    /// What `RELAIS_AUTO_PRUNE_DAYS` and the default make of the prune age
    /// (`--no-prune` is per call, so doctor never sees it).
    pub setting: AutoPruneSetting,
    /// Whether the variable is set, for naming the source of the age.
    pub env_set: bool,
    /// How old `runs-prune.stamp` is; `None` when it is missing.
    pub stamp_age: Option<Duration>,
    pub state_bytes: u64,
    /// Ended, prunable runs older than the age plus [`PURGE_GRACE`].
    pub overdue_runs: usize,
    pub overdue_bytes: u64,
    /// Ended `accepted` runs older than the age plus [`PURGE_GRACE`] kept
    /// only because the ledger holds no receipt for them.
    pub receiptless_runs: usize,
    pub receiptless_bytes: u64,
}

/// The age at which a run is overdue for the purge, when the prune is on.
pub fn overdue_after(setting: &AutoPruneSetting) -> Option<Duration> {
    match setting {
        AutoPruneSetting::Days(days) => {
            Some(DAY * u32::try_from(*days).unwrap_or(u32::MAX) + PURGE_GRACE)
        }
        AutoPruneSetting::Disabled | AutoPruneSetting::Off | AutoPruneSetting::Invalid(_) => None,
    }
}

/// How old `runs-prune.stamp` is, `None` when it is missing or unreadable.
pub fn stamp_age(state_dir: &Path, now: SystemTime) -> Option<Duration> {
    let modified = std::fs::metadata(state_dir.join(prune::STAMP_FILE))
        .and_then(|meta| meta.modified())
        .ok()?;
    Some(now.duration_since(modified).unwrap_or(Duration::ZERO))
}

pub fn purge_finding(facts: &PurgeFacts) -> Finding {
    let mut lines = Vec::new();
    let mut level = Level::Ok;
    if facts.overdue_runs > 0 {
        level = Level::Warn;
        lines.push(format!(
            "{} ended runs ({}) are older than the prune age plus 3 days: the automatic prune is not removing them",
            facts.overdue_runs,
            human_bytes(facts.overdue_bytes)
        ));
    }
    if facts.receiptless_runs > 0 {
        lines.push(format!(
            "{} ended accepted runs ({}) are older than the prune age plus 3 days and have no receipt in the ledger: they are never pruned",
            facts.receiptless_runs,
            human_bytes(facts.receiptless_bytes)
        ));
    }
    let source = if facts.env_set {
        prune::AUTO_PRUNE_DAYS_ENV
    } else {
        "default"
    };
    lines.push(match &facts.setting {
        AutoPruneSetting::Days(days) => {
            format!("prune age: {days}d ({source}; --no-prune is per call)")
        }
        AutoPruneSetting::Off => format!(
            "prune age: off ({}=0; --no-prune is per call)",
            prune::AUTO_PRUNE_DAYS_ENV
        ),
        AutoPruneSetting::Invalid(text) => {
            level = Level::Warn;
            format!(
                "prune age: invalid {}={text:?}; the prune is skipped until it is a number of days",
                prune::AUTO_PRUNE_DAYS_ENV
            )
        }
        AutoPruneSetting::Disabled => "prune age: disabled for this call (--no-prune)".to_string(),
    });
    lines.push(match facts.stamp_age {
        Some(age) => format!("{} last ran {} ago", prune::STAMP_FILE, human_age(age)),
        None => format!("{} is missing: the prune has not run", prune::STAMP_FILE),
    });
    lines.push(format!(
        "state directory: {}",
        human_bytes(facts.state_bytes)
    ));
    Finding {
        component: "purge",
        level,
        detail: lines.join("\n"),
    }
}

/// One `ledger*` file beside the ledger that is not the ledger itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerBackup {
    pub path: PathBuf,
    pub bytes: u64,
    pub age: Duration,
}

impl LedgerBackup {
    fn written_by_relais(&self) -> bool {
        self.path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| RELAIS_BACKUP_NAMES.contains(&name))
    }
}

/// Every `ledger*` file in `state_dir` other than `ledger.sqlite` and its
/// `-wal` and `-shm`, biggest first. A missing or unreadable directory
/// lists nothing; an entry that cannot be read is left out.
pub fn ledger_backups(state_dir: &Path, now: SystemTime) -> Vec<LedgerBackup> {
    let Ok(entries) = std::fs::read_dir(state_dir) else {
        return Vec::new();
    };
    let mut found: Vec<LedgerBackup> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let ledger_own = ["ledger.sqlite", "ledger.sqlite-wal", "ledger.sqlite-shm"];
            if !name.starts_with("ledger") || ledger_own.contains(&name.as_str()) {
                return None;
            }
            let meta = entry.metadata().ok().filter(std::fs::Metadata::is_file)?;
            let age = meta
                .modified()
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .unwrap_or(Duration::ZERO);
            Some(LedgerBackup {
                path: entry.path(),
                bytes: meta.len(),
                age,
            })
        })
        .collect();
    found.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
    found
}

/// The `backups` note: nothing when there is no backup. It never fails and
/// deletes nothing; `rm` lines are text for the person to run.
pub fn backups_finding(backups: &[LedgerBackup]) -> Option<Finding> {
    if backups.is_empty() {
        return None;
    }
    let total: u64 = backups.iter().map(|backup| backup.bytes).sum();
    let mut lines = vec![format!(
        "{} ledger backup files, {} in all; relais deletes none",
        backups.len(),
        human_bytes(total)
    )];
    let shown = &backups[..backups.len().min(MAX_BACKUP_LINES)];
    for backup in shown {
        let label = if backup.written_by_relais() {
            SAFE_TO_DELETE
        } else {
            NOT_RELAIS
        };
        lines.push(format!(
            "{:>9}  {:>4}  {label}  {}",
            human_bytes(backup.bytes),
            format!("{}d", backup.age.as_secs() / DAY.as_secs()),
            backup.path.display()
        ));
    }
    if backups.len() > shown.len() {
        let rest: u64 = backups[shown.len()..].iter().map(|b| b.bytes).sum();
        lines.push(format!(
            "... and {} more ({})",
            backups.len() - shown.len(),
            human_bytes(rest)
        ));
    }
    for backup in shown.iter().filter(|backup| backup.written_by_relais()) {
        lines.push(format!("rm {}", backup.path.display()));
    }
    Some(Finding {
        component: "backups",
        level: Level::Ok,
        detail: lines.join("\n"),
    })
}

fn human_age(age: Duration) -> String {
    match age.as_secs() {
        secs if secs >= 86_400 => format!("{}d", secs / 86_400),
        secs if secs >= 3_600 => format!("{}h", secs / 3_600),
        secs => format!("{}m", secs / 60),
    }
}

/// Decimal units, one decimal above bytes: `330.0 MB`.
fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "kB", "MB", "GB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("relais-state-health-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir); // a leftover of an earlier run
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn facts() -> PurgeFacts {
        PurgeFacts {
            setting: AutoPruneSetting::Days(30),
            env_set: false,
            stamp_age: Some(Duration::from_secs(3_600)),
            state_bytes: 1_100_000_000,
            overdue_runs: 0,
            overdue_bytes: 0,
            receiptless_runs: 0,
            receiptless_bytes: 0,
        }
    }

    #[test]
    fn purge_is_quiet_without_overdue_runs_and_reports_the_stamp() {
        let finding = purge_finding(&facts());
        assert_eq!(finding.level, Level::Ok);
        assert!(finding.detail.contains("prune age: 30d (default"));
        assert!(finding.detail.contains("last ran 1h ago"));
        assert!(finding.detail.contains("1.1 GB"));
    }

    #[test]
    fn purge_warns_on_overdue_runs_even_with_a_fresh_stamp() {
        let finding = purge_finding(&PurgeFacts {
            overdue_runs: 2,
            overdue_bytes: 5_000_000,
            ..facts()
        });
        assert_eq!(finding.level, Level::Warn);
        assert!(finding.detail.contains("2 ended runs (5.0 MB)"));
    }

    #[test]
    fn purge_counts_accepted_runs_kept_for_a_missing_receipt() {
        let finding = purge_finding(&PurgeFacts {
            receiptless_runs: 3,
            receiptless_bytes: 2_000_000,
            ..facts()
        });
        assert!(finding.detail.contains("3 ended accepted runs (2.0 MB)"));
        assert!(finding.detail.contains("never pruned"));
        assert!(!purge_finding(&facts()).detail.contains("no receipt"));
    }

    #[test]
    fn purge_reports_a_missing_stamp_and_an_invalid_variable() {
        let finding = purge_finding(&PurgeFacts {
            setting: AutoPruneSetting::Invalid("soon".into()),
            env_set: true,
            stamp_age: None,
            ..facts()
        });
        assert_eq!(finding.level, Level::Warn);
        assert!(finding.detail.contains("runs-prune.stamp is missing"));
        assert!(finding
            .detail
            .contains("invalid RELAIS_AUTO_PRUNE_DAYS=\"soon\""));
    }

    #[test]
    fn overdue_is_the_age_plus_three_days() {
        assert_eq!(
            overdue_after(&AutoPruneSetting::Days(30)),
            Some(Duration::from_secs(33 * 86_400))
        );
        assert_eq!(overdue_after(&AutoPruneSetting::Off), None);
    }

    #[test]
    fn no_backup_prints_no_note() {
        let dir = scratch("none");
        std::fs::write(dir.join("ledger.sqlite"), b"x").unwrap();
        std::fs::write(dir.join("ledger.sqlite-wal"), b"x").unwrap();
        std::fs::write(dir.join("ledger.sqlite-shm"), b"x").unwrap();
        assert!(backups_finding(&ledger_backups(&dir, SystemTime::now())).is_none());
    }

    #[test]
    fn backups_are_sorted_marked_and_deleted_by_nobody() {
        let dir = scratch("marked");
        std::fs::write(dir.join("ledger.sqlite"), b"x").unwrap();
        std::fs::write(dir.join("ledger.sqlite.bak-v27"), vec![0; 3000]).unwrap();
        std::fs::write(dir.join("ledger.sqlite.hand-made"), vec![0; 9000]).unwrap();
        let backups = ledger_backups(&dir, SystemTime::now());
        let finding = backups_finding(&backups).unwrap();
        let lines: Vec<&str> = finding.detail.lines().collect();
        assert!(lines[1].contains(NOT_RELAIS) && lines[1].ends_with("ledger.sqlite.hand-made"));
        assert!(lines[2].contains(SAFE_TO_DELETE) && lines[2].ends_with("ledger.sqlite.bak-v27"));
        assert_eq!(
            lines[3],
            format!("rm {}", dir.join("ledger.sqlite.bak-v27").display())
        );
        assert!(!finding.detail.contains('\u{1b}'));
        assert!(dir.join("ledger.sqlite.bak-v27").exists());
        assert!(dir.join("ledger.sqlite.hand-made").exists());
    }

    #[test]
    fn backups_cap_at_ten_lines_and_stay_under_80_columns_before_the_path() {
        let dir = scratch("cap");
        for n in 0..13 {
            std::fs::write(dir.join(format!("ledger.copy{n}")), vec![0; 100 + n]).unwrap();
        }
        let finding = backups_finding(&ledger_backups(&dir, SystemTime::now())).unwrap();
        let lines: Vec<&str> = finding.detail.lines().collect();
        assert_eq!(lines.len(), 1 + 10 + 1);
        assert!(lines[11].starts_with("... and 3 more ("));
        for line in &lines[1..11] {
            let before_path = line.rsplit_once("  ").unwrap().0;
            assert!(before_path.len() + 16 < 80, "{line}");
        }
    }
}
