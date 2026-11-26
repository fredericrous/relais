//! `relais counter` (programme agents-many-nodes, an-t35): the shared day
//! counter beside this node's ledger, the spend the coordinator dropped and
//! the settles still waiting, per UTC day. One run is the 5% check of
//! `docs/shared-state.md`: it prints a verdict per day, so the check is no
//! longer a search of `coordinator.log`.

use super::ctx::shared_state;
use super::ctx::shared_state_error;
use super::outcome::operational;
use super::outcome::CliError;
use super::outcome::CliOutcome;
use chrono::NaiveDate;
use relais::ledger::Ledger;
use relais::money::MicroUsd;
use relais::paths;
use relais::shared_state::SharedState;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

/// How far back `--days` may reach.
const MAX_DAYS: u32 = 31;
/// The shortfall, in percent of the ledger, past which the floor is due.
const FLOOR_PERCENT: f64 = 5.0;

/// What a day's figures say. The wire names are the `check` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Check {
    /// This node keeps no shared counter (`shared_state = "local"`).
    NoCounter,
    /// The log was rotated past the day, so its drops are unknown.
    LogIncomplete,
    /// The ledger holds no spend that day.
    NoLedgerSpend,
    /// Today: settles are still arriving.
    Partial,
    /// The first day the counter was fed: it started part way through.
    Cutover,
    /// Short, but the next day over-counts by about as much: a drop logged
    /// after midnight, not a floor.
    Edge,
    /// More than the ledger by over 5%: another node feeds the counter, or
    /// something books twice. This node's ledger cannot judge the day.
    CounterAboveLedger,
    /// Within 5% of the ledger.
    Ok,
    /// Over 5% below the ledger: the floor is due.
    Short,
}

impl Check {
    fn name(self) -> &'static str {
        match self {
            Self::NoCounter => "no_counter",
            Self::LogIncomplete => "log_incomplete",
            Self::NoLedgerSpend => "no_ledger_spend",
            Self::Partial => "partial",
            Self::Cutover => "cutover",
            Self::Edge => "edge",
            Self::CounterAboveLedger => "counter_above_ledger",
            Self::Ok => "ok",
            Self::Short => "short",
        }
    }
}

/// One UTC day's measured figures, in micro-dollars.
#[derive(Debug, Clone)]
struct Figures {
    day: NaiveDate,
    counter: Option<i64>,
    ledger: i64,
    /// `None` for a day the log does not cover.
    dropped: Option<i64>,
    waiting: i64,
}

impl Figures {
    /// What the ledger holds that neither the counter, the drops nor the
    /// waiting settles account for.
    fn gap(&self) -> Option<i64> {
        Some(self.ledger - self.counter? - self.dropped? - self.waiting)
    }

    fn short_percent(&self) -> Option<f64> {
        if self.ledger <= 0 {
            return None;
        }
        Some(self.gap()? as f64 / self.ledger as f64 * 100.0)
    }
}

/// The verdicts for days in ascending order, with the percent each is
/// judged on. Pure: the day edge and the cutover need the neighbours.
fn assess(days: &[Figures], today: NaiveDate) -> Vec<(Option<f64>, Check)> {
    let percents: Vec<Option<f64>> = days.iter().map(Figures::short_percent).collect();
    let fed_before = |at: usize| days[..at].iter().all(|day| day.counter == Some(0));
    days.iter()
        .enumerate()
        .map(|(at, day)| {
            let check = if day.counter.is_none() {
                Check::NoCounter
            } else if day.dropped.is_none() {
                Check::LogIncomplete
            } else if day.ledger <= 0 {
                Check::NoLedgerSpend
            } else if day.day == today {
                Check::Partial
            } else if at > 0 && day.counter > Some(0) && fed_before(at) {
                Check::Cutover
            } else {
                match percents[at] {
                    Some(percent) if percent > FLOOR_PERCENT => {
                        if is_day_edge(days, &percents, at) {
                            Check::Edge
                        } else {
                            Check::Short
                        }
                    }
                    Some(percent) if percent < -FLOOR_PERCENT => Check::CounterAboveLedger,
                    _ => Check::Ok,
                }
            };
            (percents[at], check)
        })
        .collect()
}

/// A short day followed by an over-counted one whose gaps together stay
/// within the floor: the spend was booked on the wrong side of midnight.
fn is_day_edge(days: &[Figures], percents: &[Option<f64>], at: usize) -> bool {
    let (Some(this), Some(next)) = (days.get(at), days.get(at + 1)) else {
        return false;
    };
    let next_below = percents
        .get(at + 1)
        .copied()
        .flatten()
        .is_some_and(|percent| percent < 0.0);
    let (Some(this_gap), Some(next_gap)) = (this.gap(), next.gap()) else {
        return false;
    };
    let together = (this_gap + next_gap) as f64 / (this.ledger + next.ledger) as f64 * 100.0;
    next_below && together.abs() <= FLOOR_PERCENT
}

/// What `coordinator.log` and `coordinator.log.1` say about drops.
#[derive(Debug, Default)]
struct DropLog {
    /// Dropped micro-dollars by the UTC day of the line's stamp.
    by_day: BTreeMap<NaiveDate, i64>,
    /// The oldest stamp read, on any line: the log says nothing before it.
    covers_from: Option<chrono::DateTime<chrono::Utc>>,
    /// Drop lines without a readable stamp or amount.
    unparsed: usize,
    /// The files that were read.
    files: Vec<PathBuf>,
}

/// The line-final `(dropped_micros=<n>)` of a line: the refusal line carries
/// the primary's own error text before it, so only the last token counts.
fn dropped_in(line: &str) -> Option<i64> {
    let body = line.trim_end().strip_suffix(')')?;
    let (_, digits) = body.rsplit_once("(dropped_micros=")?;
    digits.parse().ok()
}

impl DropLog {
    /// Add the complete lines of one log file's text; a last line with no
    /// newline may be half written, and is left out.
    fn add_text(&mut self, text: &str) {
        let Some(end) = text.rfind('\n') else {
            return;
        };
        for line in text[..end].lines() {
            let stamp = line
                .split_whitespace()
                .next()
                .and_then(|word| chrono::DateTime::parse_from_rfc3339(word).ok())
                .map(|at| at.with_timezone(&chrono::Utc));
            if let Some(at) = stamp {
                if self.covers_from.map_or(true, |oldest| at < oldest) {
                    self.covers_from = Some(at);
                }
            }
            if !line.contains("dropped_micros=") {
                continue;
            }
            match (stamp, dropped_in(line)) {
                (Some(at), Some(micros)) => {
                    *self.by_day.entry(at.date_naive()).or_insert(0) += micros;
                }
                _ => self.unparsed += 1,
            }
        }
    }

    /// Read both files of the rotation. Both are opened before either is
    /// read, and a file seen twice (the same inode, a rotation between the
    /// two opens) is read once.
    fn read(dir: &Path) -> Result<Self, std::io::Error> {
        let mut log = Self::default();
        let mut open = Vec::new();
        for name in [relais::coordinator::DAEMON_LOG, "coordinator.log.1"] {
            let path = dir.join(name);
            match std::fs::File::open(&path) {
                Ok(file) => open.push((path, file)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        let mut seen = Vec::new();
        for (path, mut file) in open {
            if let Some(id) = file_id(&file) {
                if seen.contains(&id) {
                    continue;
                }
                seen.push(id);
            }
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut file, &mut bytes)?;
            log.add_text(&String::from_utf8_lossy(&bytes));
            log.files.push(path);
        }
        Ok(log)
    }

    /// Whether the log covers `day`: it has no gap before its oldest stamp.
    fn covers(&self, day: NaiveDate) -> bool {
        self.covers_from
            .is_some_and(|oldest| day >= oldest.date_naive())
    }
}

#[cfg(unix)]
fn file_id(file: &std::fs::File) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = file.metadata().ok()?;
    Some((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn file_id(_file: &std::fs::File) -> Option<(u64, u64)> {
    None
}

#[derive(serde::Serialize)]
struct DayRow {
    day: String,
    counter_micros: Option<i64>,
    ledger_micros: i64,
    dropped_micros: Option<i64>,
    waiting_micros: i64,
    short_percent: Option<f64>,
    check: &'static str,
}

#[derive(serde::Serialize)]
struct WaitingInfo {
    count: usize,
    micros: i64,
    oldest_since: Option<String>,
    error: Option<String>,
}

#[derive(serde::Serialize)]
struct LogInfo {
    path: String,
    files: Vec<String>,
    unparsed: usize,
    covers_from: Option<String>,
}

#[derive(serde::Serialize)]
struct CounterDocument {
    selection: &'static str,
    days: Vec<DayRow>,
    waiting: WaitingInfo,
    log: LogInfo,
}

fn day_start(day: NaiveDate) -> String {
    day.and_hms_opt(0, 0, 0)
        .expect("midnight exists")
        .and_utc()
        .to_rfc3339()
}

fn unix_day(secs: i64) -> Option<NaiveDate> {
    chrono::DateTime::from_timestamp(secs, 0).map(|at| at.date_naive())
}

/// Print one text block; a closed stdout is not a failure.
fn emit(text: &str) -> Result<(), CliError> {
    let mut out = std::io::stdout().lock();
    match write!(out, "{text}").and_then(|()| out.flush()) {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        other => other.map_err(|cause| CliError::Operational {
            operation: "counter: writing stdout",
            cause: Box::new(cause),
        }),
    }
}

fn money(micros: Option<i64>) -> String {
    micros.map_or("-".to_string(), |micros| {
        MicroUsd::from_micros(micros).to_string()
    })
}

/// One text row, at most 80 columns for the usual figures.
fn text_row(row: &DayRow) -> String {
    format!(
        "{:<10} {:>9} {:>9} {:>9} {:>9} {:>6} {}\n",
        row.day,
        money(row.counter_micros),
        money(Some(row.ledger_micros)),
        money(row.dropped_micros),
        money(Some(row.waiting_micros)),
        row.short_percent
            .map_or("-".to_string(), |percent| format!("{percent:.1}")),
        row.check,
    )
}

fn render_text(document: &CounterDocument) -> String {
    let mut out = String::new();
    if document.selection == "local" {
        out.push_str("this node keeps no shared counter (shared_state = local)\n");
    } else {
        out.push_str("the shared counter beside this node's ledger (shared_state = external)\n");
    }
    out.push_str(&format!(
        "{:<10} {:>9} {:>9} {:>9} {:>9} {:>6} check\n",
        "day", "counter", "ledger", "dropped", "waiting", "short%"
    ));
    for row in &document.days {
        out.push_str(&text_row(row));
    }
    let waiting = &document.waiting;
    out.push_str(&format!(
        "waiting settles on this node: {} worth {}{}\n",
        waiting.count,
        MicroUsd::from_micros(waiting.micros),
        waiting
            .oldest_since
            .as_deref()
            .map_or(String::new(), |since| format!(", oldest since {since}")),
    ));
    out.push_str(
        "dropped is read from this node's coordinator.log; the first-fed (cutover) day\n\
         is the person's to exclude.\n",
    );
    out
}

/// The figures of the last `days` UTC days, ascending, today last.
fn measure(
    state: &dyn SharedState,
    ledger: Option<&Ledger>,
    log: &DropLog,
    waiting: &[relais::coordinator::WaitingSettle],
    today: NaiveDate,
    days: u32,
) -> Result<Vec<Figures>, CliError> {
    let mut figures = Vec::new();
    for back in (0..days).rev() {
        let day = today - chrono::Days::new(u64::from(back));
        let (from, to) = (day_start(day), day_start(day + chrono::Days::new(1)));
        let counter = state
            .spent_between(&from, &to)
            .map_err(|e| shared_state_error("the shared counter", e))?
            .map(MicroUsd::to_micros);
        let ledger_micros = match ledger {
            Some(ledger) => {
                operational(ledger.spend_between(&from, &to), "the ledger sum")?.to_micros()
            }
            None => 0,
        };
        figures.push(Figures {
            day,
            counter,
            ledger: ledger_micros,
            dropped: log
                .covers(day)
                .then(|| log.by_day.get(&day).copied().unwrap_or(0)),
            waiting: waiting
                .iter()
                .filter(|settle| unix_day(settle.since) == Some(day))
                .map(|settle| settle.micros)
                .sum(),
        });
    }
    Ok(figures)
}

pub(crate) fn counter_command(days: u32, json: bool) -> Result<CliOutcome, CliError> {
    if days == 0 || days > MAX_DAYS {
        return Err(CliError::Usage {
            detail: format!("--days takes 1 to {MAX_DAYS}, not {days}"),
        });
    }
    let state = shared_state()?;
    let ledger_path = paths::ledger_path().map_err(CliError::Home)?;
    let ledger = if ledger_path.exists() {
        Some(operational(
            Ledger::open_read_only(&ledger_path),
            "the ledger is unavailable",
        )?)
    } else {
        None
    };
    let state_dir = paths::state_dir().map_err(CliError::Home)?;
    let log = operational(DropLog::read(&state_dir), "reading the coordinator log")?;
    let settles_file = relais::coordinator::settles_path(&ledger_path);
    let waiting = relais::coordinator::waiting_settles(&settles_file);
    let today = chrono::Utc::now().date_naive();
    let figures = measure(
        &*state,
        ledger.as_ref(),
        &log,
        waiting.as_deref().unwrap_or(&[]),
        today,
        days,
    )?;
    let verdicts = assess(&figures, today);
    // Newest first, as a person reads it.
    let rows: Vec<DayRow> = figures
        .iter()
        .zip(&verdicts)
        .rev()
        .map(|(figures, (percent, check))| DayRow {
            day: figures.day.format("%Y-%m-%d").to_string(),
            counter_micros: figures.counter,
            ledger_micros: figures.ledger,
            dropped_micros: figures.dropped,
            waiting_micros: figures.waiting,
            short_percent: percent.map(|percent| (percent * 10.0).round() / 10.0),
            check: check.name(),
        })
        .collect();
    let selection = match state.selection() {
        relais::shared_state::Selection::Local => "local",
        relais::shared_state::Selection::External => "external",
    };
    let waiting_info = match &waiting {
        Ok(list) => WaitingInfo {
            count: list.len(),
            micros: list.iter().map(|settle| settle.micros).sum(),
            oldest_since: list
                .iter()
                .map(|settle| settle.since)
                .min()
                .and_then(|since| chrono::DateTime::from_timestamp(since, 0))
                .map(|at| at.to_rfc3339()),
            error: None,
        },
        Err(error) => WaitingInfo {
            count: 0,
            micros: 0,
            oldest_since: None,
            error: Some(error.clone()),
        },
    };
    let document = CounterDocument {
        selection,
        days: rows,
        waiting: waiting_info,
        log: LogInfo {
            path: state_dir
                .join(relais::coordinator::DAEMON_LOG)
                .display()
                .to_string(),
            files: log.files.iter().map(|p| p.display().to_string()).collect(),
            unparsed: log.unparsed,
            covers_from: log.covers_from.map(|at| at.to_rfc3339()),
        },
    };
    if json {
        let text = operational(serde_json::to_string_pretty(&document), "rendering JSON")?;
        emit(&format!("{text}\n"))?;
    } else {
        emit(&render_text(&document))?;
        if let Some(error) = &document.waiting.error {
            eprintln!(
                "relais counter: the waiting settles could not be read: {error} ({})",
                settles_file.display()
            );
        }
        if document.log.unparsed > 0 {
            eprintln!(
                "relais counter: {} dropped line(s) had no readable stamp or amount ({})",
                document.log.unparsed, document.log.path
            );
        }
    }
    Ok(CliOutcome::Accepted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(n: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, n).expect("a date")
    }

    fn figures(
        n: u32,
        counter: Option<i64>,
        ledger: i64,
        dropped: Option<i64>,
        waiting: i64,
    ) -> Figures {
        Figures {
            day: day(n),
            counter,
            ledger,
            dropped,
            waiting,
        }
    }

    fn checks(days: &[Figures]) -> Vec<&'static str> {
        assess(days, day(30))
            .iter()
            .map(|(_, check)| check.name())
            .collect()
    }

    #[test]
    fn a_day_is_judged_against_the_five_percent_floor() {
        let before = figures(1, Some(0), 0, Some(0), 0);
        assert_eq!(
            checks(&[
                before.clone(),
                figures(2, Some(960), 1000, Some(0), 0),
                figures(3, Some(970), 1000, Some(0), 0),
                figures(4, Some(1100), 1000, Some(0), 0),
                figures(5, Some(920), 1000, Some(0), 0),
            ]),
            [
                "no_ledger_spend",
                "cutover",
                "ok",
                "counter_above_ledger",
                "short"
            ]
        );
        // Dropped and waiting spend is added back before the verdict.
        assert_eq!(
            checks(&[
                before,
                figures(2, Some(700), 1000, Some(250), 50),
                figures(3, Some(700), 1000, Some(100), 0),
            ]),
            ["no_ledger_spend", "cutover", "short"]
        );
    }

    #[test]
    fn a_local_node_and_a_rotated_log_are_not_verdicts() {
        assert_eq!(
            checks(&[figures(1, None, 1000, Some(0), 0)]),
            ["no_counter"]
        );
        assert_eq!(
            checks(&[figures(1, Some(0), 1000, None, 0)]),
            ["log_incomplete"]
        );
    }

    #[test]
    fn today_is_partial_and_a_day_edge_pair_is_not_a_floor() {
        // 6% short, then 6% over: together 0%, so the first is an edge.
        let days = [
            figures(1, Some(500), 520, Some(0), 0),
            figures(2, Some(940), 1000, Some(0), 0),
            figures(3, Some(1060), 1000, Some(0), 0),
            figures(30, Some(10), 1000, Some(0), 0),
        ];
        assert_eq!(
            checks(&days),
            ["ok", "edge", "counter_above_ledger", "partial"]
        );
    }

    #[test]
    fn a_short_day_before_a_small_overcount_stays_short() {
        // 12% short, next day 1% over: together still 5.5% short.
        let days = [
            figures(2, Some(500), 600, Some(0), 0),
            figures(3, Some(880), 1000, Some(0), 0),
            figures(4, Some(1010), 1000, Some(0), 0),
        ];
        assert_eq!(checks(&days)[1], "short");
    }

    #[test]
    fn only_the_line_final_dropped_token_counts_and_a_half_line_is_left() {
        let mut log = DropLog::default();
        log.add_text(
            "2026-11-26T10:00:00.000Z relais coordinator: seat d: booking the spend of $0.0004 \
             was not accepted: refused: reason (dropped_micros=1); dropped (dropped_micros=400)\n\
             2026-11-26T11:00:00.000Z relais coordinator: seat settles: dropped 2 waiting \
             settle(s); the shared counter is short by $1 (dropped_micros=1000000)\n\
             2026-11-25T09:00:00.000Z relais coordinator: seat d: shared seat granted\n\
             not a stamp (dropped_micros=5)\n\
             2026-11-26T12:00:00.000Z relais coordinator: half (dropped_micros=9",
        );
        assert_eq!(log.by_day.get(&day(26)), Some(&1_000_400));
        assert_eq!(
            log.unparsed, 1,
            "an unstamped drop line is counted, a half line is not"
        );
        assert_eq!(log.covers_from.map(|at| at.date_naive()), Some(day(25)));
        assert!(log.covers(day(25)) && !log.covers(day(24)));
    }

    #[test]
    fn the_daemons_own_stamp_is_what_the_parser_reads() {
        let at = chrono::Utc::now();
        let line = relais::coordinator::stamped(
            at,
            "seat settles: dropped 1 waiting settle(s) (dropped_micros=42)",
        );
        let mut log = DropLog::default();
        log.add_text(&format!("{line}\n"));
        assert_eq!(log.by_day.get(&at.date_naive()), Some(&42));
        assert_eq!(log.unparsed, 0);
    }

    #[test]
    fn a_log_seen_twice_by_inode_is_read_once() {
        let dir = relais::test_support::short_temp_dir("counter-log");
        let first = dir.join(relais::coordinator::DAEMON_LOG);
        std::fs::write(
            &first,
            "2026-11-26T10:00:00.000Z relais coordinator: x (dropped_micros=7)\n",
        )
        .expect("write");
        // A rotation between the two opens leaves one inode under two names.
        std::fs::hard_link(&first, dir.join("coordinator.log.1")).expect("link");
        let log = DropLog::read(&dir).expect("read");
        assert_eq!(log.by_day.get(&day(26)), Some(&7));
        assert_eq!(log.files.len(), 1);
    }

    #[test]
    fn the_text_row_fits_eighty_columns_for_usual_figures() {
        let row = DayRow {
            day: "2026-11-26".into(),
            counter_micros: Some(12_345_678),
            ledger_micros: 12_345_678,
            dropped_micros: Some(1_500_000),
            waiting_micros: 0,
            short_percent: Some(-100.0),
            check: "counter_above_ledger",
        };
        let text = text_row(&row);
        assert!(text.trim_end().chars().count() <= 80, "{text}");
        assert!(!text.contains('\u{1b}'));
    }
}
