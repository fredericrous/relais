//! Two `relais doctor` findings about the state directory: whether the
//! automatic prune of ended runs is keeping up, and which ledger backups
//! sit beside the ledger. Both only read; nothing here deletes a file.

use super::{Finding, Level};
use crate::prune::{self, AutoPruneSetting, SettingSource};
use crate::units::human_bytes;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// An ended run older than the prune age plus this is a prune that is not
/// working, whatever the stamp says.
pub const PURGE_GRACE: Duration = Duration::from_secs(3 * 86_400);

const DAY: Duration = Duration::from_secs(86_400);
const MAX_BACKUP_LINES: usize = 10;
const RELAIS_BACKUP_NAMES: [&str; 2] = ["ledger.sqlite.bak-v27", "ledger.sqlite.bak-v30"];
const SAFE_TO_DELETE: &str = "safe to delete later";
const NOT_RELAIS: &str = "not from relais";
/// The doctor indents a finding by 16 columns; a line fits 80 with it.
const LINE_BUDGET: usize = 80 - 16;
/// How much of an operator-supplied or system text one line keeps.
const TEXT_BUDGET: usize = 50;
const ELLIPSIS: &str = "...";

/// `text` cut to `max` characters with an ellipsis at the end.
fn cut_end(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text
        .chars()
        .take(max.saturating_sub(ELLIPSIS.len()))
        .collect();
    format!("{head}{ELLIPSIS}")
}

/// `name` cut to `max` characters with an ellipsis in the middle, so the
/// extension stays.
fn cut_middle(name: &str, max: usize) -> String {
    let length = name.chars().count();
    if length <= max {
        return name.to_string();
    }
    let extension = name.rfind('.').map_or(0, |dot| name[dot..].chars().count());
    let tail = extension.min(max / 2).max(max / 4);
    let head = max.saturating_sub(ELLIPSIS.len() + tail);
    let start: String = name.chars().take(head).collect();
    let end: String = name.chars().skip(length - tail).collect();
    format!("{start}{ELLIPSIS}{end}")
}

/// How `runs-prune.stamp` stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StampState {
    Missing,
    /// Last written this long ago.
    Written(Duration),
    /// Dated this far ahead of now (a clock set back).
    InTheFuture(Duration),
}

impl StampState {
    /// Whether the automatic prune would run again: missing, in the
    /// future, or older than [`PURGE_GRACE`].
    fn is_stale(self) -> bool {
        match self {
            Self::Missing | Self::InTheFuture(_) => true,
            Self::Written(age) => age > PURGE_GRACE,
        }
    }
}

/// What the doctor learned about the run directories, or why it could not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunsHealth {
    /// The prune is off or invalid: nothing was judged.
    NotChecked,
    /// The ledger or `runs/` could not be read: nothing is known.
    Unmeasured {
        operation: String,
        error: String,
    },
    Measured(RunCounts),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RunCounts {
    /// Ended runs the prune would delete, older than the prune age.
    pub old_ended_runs: usize,
    /// Of those, older than the age plus [`PURGE_GRACE`].
    pub overdue_runs: usize,
    pub overdue_bytes: u64,
    /// Ended `accepted` runs older than the age plus [`PURGE_GRACE`] kept
    /// only because the ledger holds no receipt for them.
    pub receiptless_runs: usize,
    pub receiptless_bytes: u64,
}

/// The facts the purge-health finding is made of, all read by the caller.
pub struct PurgeFacts {
    /// What `RELAIS_AUTO_PRUNE_DAYS` and the default make of the prune age.
    pub setting: AutoPruneSetting,
    pub source: SettingSource,
    pub stamp: StampState,
    /// The state directory without the live checkouts under `verify/`.
    pub state_bytes: u64,
    pub runs: RunsHealth,
}

/// The prune age plus [`PURGE_GRACE`], when the prune is on.
pub fn overdue_after(setting: &AutoPruneSetting) -> Option<Duration> {
    prune_age(setting).map(|age| age + PURGE_GRACE)
}

/// The age the automatic prune deletes at, when it is on.
pub fn prune_age(setting: &AutoPruneSetting) -> Option<Duration> {
    match setting {
        AutoPruneSetting::Days(days) => Some(DAY * u32::try_from(*days).unwrap_or(u32::MAX)),
        AutoPruneSetting::Off | AutoPruneSetting::Invalid(_) => None,
    }
}

/// How `runs-prune.stamp` stands. A stamp that cannot be read is missing.
pub fn stamp_state(state_dir: &Path, now: SystemTime) -> StampState {
    let Some(modified) = std::fs::metadata(state_dir.join(prune::STAMP_FILE))
        .and_then(|meta| meta.modified())
        .ok()
    else {
        return StampState::Missing;
    };
    match now.duration_since(modified) {
        Ok(age) => StampState::Written(age),
        Err(ahead) => StampState::InTheFuture(ahead.duration()),
    }
}

const NEXT_STEP: &str = "next: run `relais resume --prune-runs --write`\n  or read resume.log for `relais: prune` errors";

pub fn purge_finding(facts: &PurgeFacts) -> Finding {
    let mut lines = Vec::new();
    let mut level = Level::Ok;
    let mut needs_next_step = false;
    match &facts.runs {
        RunsHealth::NotChecked => {}
        RunsHealth::Unmeasured { operation, error } => {
            level = Level::Warn;
            lines.push("the purge could not be measured".to_string());
            let one_line = format!("  {operation} failed: {error}");
            if one_line.chars().count() <= LINE_BUDGET {
                lines.push(one_line);
            } else {
                lines.push(format!("  {} failed:", cut_end(operation, TEXT_BUDGET)));
                lines.push(format!("  {}", cut_end(error, TEXT_BUDGET)));
            }
        }
        RunsHealth::Measured(counts) => {
            if counts.overdue_runs > 0 {
                level = Level::Warn;
                needs_next_step = true;
                lines.push(format!(
                    "{} ended runs ({}) are past the prune age plus 3 days",
                    counts.overdue_runs,
                    human_bytes(counts.overdue_bytes)
                ));
                lines.push("  the automatic prune is not removing them".to_string());
            }
            if counts.old_ended_runs > 0 && facts.stamp.is_stale() {
                level = Level::Warn;
                needs_next_step = true;
                let why = match facts.stamp {
                    StampState::Missing => format!("{} is missing", prune::STAMP_FILE),
                    StampState::InTheFuture(_) => {
                        format!("{} is dated in the future", prune::STAMP_FILE)
                    }
                    StampState::Written(_) => {
                        format!("{} has not been written for over 3 days", prune::STAMP_FILE)
                    }
                };
                lines.push(format!(
                    "{} ended runs are old enough to prune",
                    counts.old_ended_runs
                ));
                lines.push("  the automatic prune is not running".to_string());
                lines.push(format!("  {why}"));
            }
            if counts.receiptless_runs > 0 {
                lines.push(format!(
                    "{} ended accepted runs ({}) lack a receipt",
                    counts.receiptless_runs,
                    human_bytes(counts.receiptless_bytes)
                ));
                lines.push("  past the prune age, so never pruned".to_string());
                lines.push("  `relais resume --prune-runs` lists them".to_string());
            }
        }
    }
    if needs_next_step {
        lines.push(NEXT_STEP.to_string());
    }
    let source = match facts.source {
        SettingSource::Environment => prune::AUTO_PRUNE_DAYS_ENV,
        SettingSource::Default => "default",
    };
    lines.push(match &facts.setting {
        AutoPruneSetting::Days(days) => format!("prune age: {days}d ({source})"),
        AutoPruneSetting::Off => format!("prune age: off ({}=0)", prune::AUTO_PRUNE_DAYS_ENV),
        AutoPruneSetting::Invalid(text) => {
            level = Level::Warn;
            let one_line = format!("prune age: invalid {}={text:?}", prune::AUTO_PRUNE_DAYS_ENV);
            let shown = if one_line.chars().count() <= LINE_BUDGET {
                one_line
            } else {
                format!(
                    "prune age: invalid {}\n  value: {:?}",
                    prune::AUTO_PRUNE_DAYS_ENV,
                    cut_end(text, TEXT_BUDGET - 8)
                )
            };
            format!("{shown}\n  the prune is skipped until it is a number of days")
        }
    });
    lines.push(match facts.stamp {
        StampState::Written(age) => {
            format!("{} last ran {} ago", prune::STAMP_FILE, human_age(age))
        }
        StampState::Missing => format!("{} is missing: the prune has not run", prune::STAMP_FILE),
        StampState::InTheFuture(ahead) => format!(
            "{} is dated {} in the future\n  the clock was set back",
            prune::STAMP_FILE,
            human_age(ahead)
        ),
    });
    lines.push(format!(
        "state directory: at least {}\n  live verify checkouts under verify/ left out",
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
/// deletes nothing; `rm` lines are text for the person to run. Each file is
/// named relative to `state_dir`, which the header gives once; only the
/// `rm` lines carry the full path.
pub fn backups_finding(state_dir: &Path, backups: &[LedgerBackup]) -> Option<Finding> {
    if backups.is_empty() {
        return None;
    }
    let total: u64 = backups.iter().map(|backup| backup.bytes).sum();
    let mut lines = vec![format!(
        "{} ledger backup files, {} in all; relais deletes none",
        backups.len(),
        human_bytes(total)
    )];
    lines.push(format!("in {}", state_dir.display()));
    let shown = &backups[..backups.len().min(MAX_BACKUP_LINES)];
    for backup in shown {
        let label = if backup.written_by_relais() {
            SAFE_TO_DELETE
        } else {
            NOT_RELAIS
        };
        let name = backup.path.strip_prefix(state_dir).unwrap_or(&backup.path);
        let size = human_bytes(backup.bytes);
        let age = format!("{}d", backup.age.as_secs() / DAY.as_secs());
        let name_budget = LINE_BUDGET.saturating_sub(9 + 2 + 4 + 2 + label.len() + 2);
        lines.push(format!(
            "{size:>9}  {age:>4}  {label}  {}",
            cut_middle(&name.display().to_string(), name_budget)
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

#[cfg(test)]
mod tests {
    use super::*;

    const INDENT_FOR_TEST: usize = 16;

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
            source: SettingSource::Default,
            stamp: StampState::Written(Duration::from_secs(3_600)),
            state_bytes: 1_100_000_000,
            runs: RunsHealth::Measured(RunCounts::default()),
        }
    }

    fn counts(counts: RunCounts) -> RunsHealth {
        RunsHealth::Measured(counts)
    }

    #[test]
    fn purge_is_quiet_without_overdue_runs_and_reports_the_stamp() {
        let finding = purge_finding(&facts());
        assert_eq!(finding.level, Level::Ok);
        assert!(finding.detail.contains("prune age: 30d (default)"));
        assert!(!finding.detail.contains("--no-prune"));
        assert!(finding.detail.contains("last ran 1h ago"));
        assert!(finding.detail.contains("1.0 GiB"));
        assert!(finding.detail.contains("verify/"));
    }

    #[test]
    fn purge_warns_on_overdue_runs_even_with_a_fresh_stamp() {
        let finding = purge_finding(&PurgeFacts {
            runs: counts(RunCounts {
                old_ended_runs: 2,
                overdue_runs: 2,
                overdue_bytes: 5 * 1024 * 1024,
                ..RunCounts::default()
            }),
            ..facts()
        });
        assert_eq!(finding.level, Level::Warn);
        assert!(finding.detail.contains("2 ended runs (5.0 MiB)"));
        assert!(finding
            .detail
            .contains("relais resume --prune-runs --write"));
        assert!(finding.detail.contains("resume.log"));
    }

    #[test]
    fn purge_counts_accepted_runs_kept_for_a_missing_receipt() {
        let finding = purge_finding(&PurgeFacts {
            runs: counts(RunCounts {
                receiptless_runs: 3,
                receiptless_bytes: 2 * 1024 * 1024,
                ..RunCounts::default()
            }),
            ..facts()
        });
        assert!(finding.detail.contains("3 ended accepted runs (2.0 MiB)"));
        assert!(finding.detail.contains("never pruned"));
        assert!(finding.detail.contains("`relais resume --prune-runs`"));
        assert!(!purge_finding(&facts()).detail.contains("no receipt"));
    }

    #[test]
    fn purge_reports_a_missing_stamp_and_an_invalid_variable() {
        let finding = purge_finding(&PurgeFacts {
            setting: AutoPruneSetting::Invalid("soon".into()),
            source: SettingSource::Environment,
            stamp: StampState::Missing,
            ..facts()
        });
        assert_eq!(finding.level, Level::Warn);
        assert!(finding.detail.contains("runs-prune.stamp is missing"));
        assert!(finding
            .detail
            .contains("invalid RELAIS_AUTO_PRUNE_DAYS=\"soon\""));
    }

    #[test]
    fn purge_warns_when_old_runs_wait_and_the_stamp_is_missing_or_stale() {
        let waiting = counts(RunCounts {
            old_ended_runs: 4,
            ..RunCounts::default()
        });
        for stamp in [
            StampState::Missing,
            StampState::Written(4 * DAY),
            StampState::InTheFuture(DAY),
        ] {
            let finding = purge_finding(&PurgeFacts {
                stamp,
                runs: waiting.clone(),
                ..facts()
            });
            assert_eq!(finding.level, Level::Warn, "{stamp:?}");
            assert!(finding.detail.contains("4 ended runs are old enough"));
            let wording = match stamp {
                StampState::Missing => "runs-prune.stamp is missing",
                StampState::InTheFuture(_) => "runs-prune.stamp is dated in the future",
                StampState::Written(_) => "runs-prune.stamp has not been written for over 3 days",
            };
            assert!(finding.detail.contains(wording), "{}", finding.detail);
            assert!(finding.detail.contains("next: run"));
        }
        let fresh = purge_finding(&PurgeFacts {
            runs: waiting,
            ..facts()
        });
        assert_eq!(fresh.level, Level::Ok);
        let nothing_old = purge_finding(&PurgeFacts {
            stamp: StampState::Missing,
            ..facts()
        });
        assert_eq!(nothing_old.level, Level::Ok);
    }

    #[test]
    fn purge_says_a_stamp_in_the_future_is_one() {
        let finding = purge_finding(&PurgeFacts {
            stamp: StampState::InTheFuture(2 * DAY),
            ..facts()
        });
        assert!(finding.detail.contains("dated 2d in the future"));
    }

    #[test]
    fn purge_that_could_not_measure_warns_and_never_reports_zero() {
        let finding = purge_finding(&PurgeFacts {
            runs: RunsHealth::Unmeasured {
                operation: "opening the ledger /s/ledger.sqlite".into(),
                error: "database is locked".into(),
            },
            ..facts()
        });
        assert_eq!(finding.level, Level::Warn);
        assert!(finding
            .detail
            .contains("opening the ledger /s/ledger.sqlite failed: database is locked"));
        assert!(!finding.detail.contains(" 0 "));
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
        assert!(backups_finding(&dir, &ledger_backups(&dir, SystemTime::now())).is_none());
    }

    #[test]
    fn backups_are_sorted_marked_and_deleted_by_nobody() {
        let dir = scratch("marked");
        std::fs::write(dir.join("ledger.sqlite"), b"x").unwrap();
        std::fs::write(dir.join("ledger.sqlite.bak-v27"), vec![0; 3000]).unwrap();
        std::fs::write(dir.join("ledger.sqlite.hand-made"), vec![0; 9000]).unwrap();
        let backups = ledger_backups(&dir, SystemTime::now());
        let finding = backups_finding(&dir, &backups).unwrap();
        let lines: Vec<&str> = finding.detail.lines().collect();
        assert!(lines[2].contains(NOT_RELAIS) && lines[2].ends_with("ledger.sqlite.hand-made"));
        assert!(lines[3].contains(SAFE_TO_DELETE) && lines[3].ends_with("ledger.sqlite.bak-v27"));
        assert_eq!(
            lines[4],
            format!("rm {}", dir.join("ledger.sqlite.bak-v27").display())
        );
        assert!(!finding.detail.contains('\u{1b}'));
        assert!(dir.join("ledger.sqlite.bak-v27").exists());
        assert!(dir.join("ledger.sqlite.hand-made").exists());
    }

    #[test]
    fn findings_fit_80_columns_after_the_indent_in_a_long_state_directory() {
        const INDENT: usize = INDENT_FOR_TEST;
        let long = format!("{}/{}", scratch("width").display(), "deep-dir-".repeat(12));
        let dir = PathBuf::from(&long);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ledger.sqlite.bak-v27"), vec![0; 3000]).unwrap();
        std::fs::write(dir.join("ledger.sqlite.hand-made"), vec![0; 9000]).unwrap();
        let overlong = format!("ledger.{}.sqlite", "x".repeat(150));
        std::fs::write(dir.join(&overlong), vec![0; 100]).unwrap();
        let backups = backups_finding(&dir, &ledger_backups(&dir, SystemTime::now())).unwrap();
        assert!(backups.detail.contains("...") && backups.detail.contains(".sqlite\n"));
        let wide = "z".repeat(200);
        let purge = purge_finding(&PurgeFacts {
            setting: AutoPruneSetting::Invalid(wide.clone()),
            source: SettingSource::Environment,
            stamp: StampState::Written(4 * DAY),
            runs: counts(RunCounts {
                old_ended_runs: 9_999,
                overdue_runs: 9_999,
                overdue_bytes: 900 * 1024 * 1024 * 1024,
                receiptless_runs: 9_999,
                receiptless_bytes: 900 * 1024 * 1024 * 1024,
            }),
            ..facts()
        });
        let future = purge_finding(&PurgeFacts {
            stamp: StampState::InTheFuture(900 * DAY),
            ..facts()
        });
        let unmeasured = purge_finding(&PurgeFacts {
            runs: RunsHealth::Unmeasured {
                operation: format!("opening the ledger {long}/ledger.sqlite"),
                error: wide.clone(),
            },
            ..facts()
        });
        for line in backups
            .detail
            .lines()
            .chain(purge.detail.lines())
            .chain(future.detail.lines())
            .chain(unmeasured.detail.lines())
        {
            if line.starts_with("rm ") || line.starts_with("in ") {
                continue;
            }
            let columns = INDENT + line.chars().count();
            assert!(columns <= 80, "{columns} columns: {line}");
        }
        assert!(!purge.detail.contains(&wide) && !unmeasured.detail.contains(&wide));
        assert!(backups.detail.lines().any(|l| l == format!("in {long}")));
    }

    #[test]
    fn a_long_name_is_cut_in_its_middle_and_keeps_its_extension() {
        let name = format!("ledger.{}.sqlite", "x".repeat(150));
        let cut = cut_middle(&name, 30);
        assert_eq!(cut.chars().count(), 30, "{cut}");
        assert!(cut.starts_with("ledger.") && cut.contains("...") && cut.ends_with(".sqlite"));
        assert_eq!(cut_middle("ledger.copy1", 30), "ledger.copy1");
    }

    #[test]
    fn an_overlong_text_is_cut_to_the_budget_with_an_ellipsis() {
        let wide = "z".repeat(200);
        let cut = cut_end(&wide, TEXT_BUDGET);
        assert_eq!(cut.chars().count(), TEXT_BUDGET);
        assert!(cut.ends_with("..."));
        assert_eq!(cut_end("soon", TEXT_BUDGET), "soon");
    }

    #[test]
    fn invalid_age_and_measurement_error_lines_fit_80_columns_with_the_indent() {
        let wide = "q".repeat(200);
        let invalid = purge_finding(&PurgeFacts {
            setting: AutoPruneSetting::Invalid(wide.clone()),
            source: SettingSource::Environment,
            ..facts()
        });
        let unmeasured = purge_finding(&PurgeFacts {
            runs: RunsHealth::Unmeasured {
                operation: "reading runs/".into(),
                error: wide,
            },
            ..facts()
        });
        for line in invalid.detail.lines().chain(unmeasured.detail.lines()) {
            assert!(16 + line.chars().count() <= 80, "{line}");
        }
        assert!(invalid.detail.contains("..."));
        assert!(unmeasured.detail.contains("..."));
    }

    #[test]
    fn backups_cap_at_ten_lines_and_name_files_relative_to_the_directory() {
        let dir = scratch("cap");
        for n in 0..13 {
            std::fs::write(dir.join(format!("ledger.copy{n}")), vec![0; 100 + n]).unwrap();
        }
        std::fs::write(dir.join("ledger.sqlite.bak-v30"), vec![0; 50]).unwrap();
        let finding = backups_finding(&dir, &ledger_backups(&dir, SystemTime::now())).unwrap();
        let lines: Vec<&str> = finding.detail.lines().collect();
        assert!(lines[0].len() < 64, "{}", lines[0]);
        assert_eq!(lines[1], format!("in {}", dir.display()));
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.contains(&dir.display().to_string()))
                .count(),
            1 + lines.iter().filter(|l| l.starts_with("rm ")).count()
        );
        assert!(lines[12].starts_with("... and 4 more ("));
        for line in &lines[2..12] {
            assert!(INDENT_FOR_TEST + line.len() <= 80, "{line}");
        }
    }
}
