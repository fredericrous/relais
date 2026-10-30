//! The holdouts section (decisions ADR-0042 and ADR-0045 phase 4): what
//! holdouts catch and how far hiding them by omission holds, counted over
//! the window's runs from the ledger only.
//!
//! * `holdout_runs`: runs that named at least one holdout, the base of the
//!   other two counts.
//! * `holdout_catches`: runs with a holdout that failed
//!   on a candidate whose profile checks were all green: acceptance on
//!   evidence the worker controlled, which a holdout stopped.
//! * `holdout_exposures`: runs whose workers named a holdout location in a
//!   tool call, with the number of runs per marker.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::ids::RunId;
use crate::ledger::{Ledger, LedgerError};
use crate::lifecycle::State;

/// The `holdouts` section.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct HoldoutsReport {
    pub holdout_runs: usize,
    pub holdout_catches: usize,
    pub holdout_exposures: HoldoutExposures,
    pub by_source: HoldoutsBySource,
}

/// Runs and catches per holdout source; a run counts once per source it ran.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct HoldoutsBySource {
    pub named: SourceCounts,
    pub attached: AttachedCounts,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct SourceCounts {
    pub runs: usize,
    pub catches: usize,
}

/// Attached counts plus the runs whose attached holdout was skipped.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct AttachedCounts {
    pub runs: usize,
    pub catches: usize,
    pub skipped: usize,
}

/// Runs with at least one exposure, and how many runs hit each marker.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct HoldoutExposures {
    pub runs: usize,
    pub by_marker: BTreeMap<String, usize>,
}

/// The section for the window's runs.
pub fn holdouts_report(
    ledger: &Ledger,
    window: &[(RunId, State)],
) -> Result<HoldoutsReport, LedgerError> {
    let mut report = HoldoutsReport::default();
    for (run, _) in window {
        if !ledger.skipped_attached_holdouts(run)?.is_empty() {
            report.by_source.attached.skipped += 1;
        }
        let named = crate::holdout::HoldoutSource::Named.as_str();
        let attached = crate::holdout::HoldoutSource::Attached.as_str();
        if ledger.run_ran_holdout_of_source(run, named)? {
            report.by_source.named.runs += 1;
            if ledger.run_holdout_of_source_caught(run, named)? {
                report.by_source.named.catches += 1;
            }
        }
        if ledger.run_ran_holdout_of_source(run, attached)? {
            report.by_source.attached.runs += 1;
            if ledger.run_holdout_of_source_caught(run, attached)? {
                report.by_source.attached.catches += 1;
            }
        }
        if !ledger.run_named_a_holdout(run)? {
            continue;
        }
        report.holdout_runs += 1;
        if ledger.run_holdout_caught(run)? {
            report.holdout_catches += 1;
        }
        let markers = ledger.run_exposure_markers(run)?;
        if !markers.is_empty() {
            report.holdout_exposures.runs += 1;
        }
        for marker in markers {
            *report
                .holdout_exposures
                .by_marker
                .entry(marker)
                .or_default() += 1;
        }
    }
    Ok(report)
}

impl HoldoutsReport {
    /// The section's lines, one per count, within 80 columns; zeros when
    /// no run in the window named a holdout.
    pub fn render(&self) -> String {
        let mut out = String::from("holdouts:\n");
        out.push_str(&format!(
            "  holdout_runs: {} (runs with a holdout)\n",
            self.holdout_runs
        ));
        let by_source = &self.by_source;
        out.push_str(&format!(
            "  by source: named {}/{}, attached {}/{}, skipped {}\n",
            by_source.named.runs,
            by_source.named.catches,
            by_source.attached.runs,
            by_source.attached.catches,
            by_source.attached.skipped
        ));
        out.push_str(&format!(
            "  holdout_catches: {} (failed while the profile checks passed)\n",
            self.holdout_catches
        ));
        let exposures = &self.holdout_exposures;
        out.push_str(&format!(
            "  holdout_exposures: {} (a worker named a holdout location)",
            exposures.runs
        ));
        if !exposures.by_marker.is_empty() {
            let by_marker = exposures
                .by_marker
                .iter()
                .map(|(marker, runs)| format!("{marker} {runs}"))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("\n    by marker: {by_marker}"));
        }
        out.push('\n');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_rendered_line_fits_eighty_columns() {
        let report = HoldoutsReport {
            holdout_runs: 12,
            holdout_catches: 3,
            by_source: HoldoutsBySource::default(),
            holdout_exposures: HoldoutExposures {
                runs: 2,
                by_marker: [("machine_toml", 1), ("overlay", 1)]
                    .into_iter()
                    .map(|(marker, runs)| (marker.to_string(), runs))
                    .collect(),
            },
        };
        let text = report.render();
        assert!(text.contains("holdout_catches: 3"), "{text}");
        assert!(text.contains("(runs with a holdout)"), "{text}");
        assert!(
            text.contains("by source: named 0/0, attached 0/0, skipped 0"),
            "{text}"
        );
        assert!(text.contains("machine_toml 1, overlay 1"), "{text}");
        for line in text.lines() {
            assert!(line.chars().count() <= 80, "{line}");
        }
    }
}
