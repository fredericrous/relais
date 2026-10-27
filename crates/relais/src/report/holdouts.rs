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
            "  holdout_runs: {} (runs that named a holdout)\n",
            self.holdout_runs
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
        assert!(text.contains("machine_toml 1, overlay 1"), "{text}");
        for line in text.lines() {
            assert!(line.chars().count() <= 80, "{line}");
        }
    }
}
