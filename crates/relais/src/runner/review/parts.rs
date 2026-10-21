use std::io::Write;
use std::path::Path;

use crate::ids::sha256_hex;
use crate::ledger::{PartResult, PartStatus, PlannedPart, ReviewPartRow};
use crate::runner::{RunEngine, RunSpend};

use super::engine::{PartLabel, PartRun};
use super::{
    review_verdict, split_into_parts, ReviewOutcome, ReviewRequest, REVIEW_PATCH_BUDGET_BYTES,
};

/// The directory of a run's part artifacts, under its artifacts directory.
const PARTS_DIR: &str = "review-parts";

impl<'a> RunEngine<'a> {
    /// Review the candidate patch in parts: every part must pass, and a
    /// part's findings are findings as they are in a single review. A patch
    /// within the budget is one part, which makes this a retry of the review.
    ///
    /// Entered again for the same run and candidate, it resumes: a part left
    /// `dispatched` is marked `unavailable (interrupted)`, a part that
    /// passed is not dispatched again, and the rest are dispatched.
    pub(crate) fn review_candidate_in_parts(
        &mut self,
        request: &ReviewRequest<'_>,
        spend: &mut RunSpend,
    ) -> ReviewOutcome {
        let patch = match std::fs::read(&request.patch_path) {
            Ok(patch) => patch,
            Err(e) => {
                return ReviewOutcome::Unavailable(format!(
                    "the candidate patch {} could not be read for a review in parts: {e}",
                    request.patch_path.display()
                ))
            }
        };
        let parts = match split_into_parts(&patch, REVIEW_PATCH_BUDGET_BYTES) {
            Ok(parts) => parts,
            Err(refusal) => return ReviewOutcome::Unavailable(refusal.to_string()),
        };
        let dir = self.artifacts.join(PARTS_DIR);
        let planned = match write_parts(&dir, &parts) {
            Ok(planned) => planned,
            Err(e) => {
                return ReviewOutcome::Unavailable(format!(
                    "the review parts could not be written to {}: {e}",
                    dir.display()
                ))
            }
        };
        let ledger = self.config.ledger;
        let run_id = self.run_id.clone();
        let sha = request.candidate_sha;
        let manifest = ledger
            .plan_review_parts(&run_id, sha, &planned)
            .and_then(|()| ledger.interrupt_review_parts(&run_id, sha))
            .and_then(|()| ledger.review_parts_of(&run_id, sha));
        let rows = match manifest {
            Ok(rows) => rows,
            Err(e) => {
                return ReviewOutcome::Unavailable(format!(
                    "the parts manifest could not be recorded: {e}"
                ))
            }
        };
        if let Some(why) = manifest_differs(&planned, &rows) {
            return ReviewOutcome::Unavailable(why);
        }
        let of = u32::try_from(rows.len()).unwrap_or(u32::MAX);
        for row in &rows {
            let label = PartLabel {
                index: row.index,
                of,
            };
            match row.status {
                PartStatus::Passed => continue,
                PartStatus::Findings => return findings_on_record(row, label),
                PartStatus::NotRun | PartStatus::Unavailable | PartStatus::Dispatched => {}
            }
            let result_stem = dir.join(format!("{}.txt", row.index));
            let part_request = request.for_patch(Path::new(&row.patch_path).to_path_buf());
            // The dispatch id is on the manifest as soon as the dispatch
            // exists, so a part cut off mid-review still names it.
            let on_dispatch = |dispatch_id: &str| {
                ledger
                    .mark_review_part_dispatched(&run_id, sha, row.index, dispatch_id)
                    .map_err(|e| format!("the parts manifest could not record {label}: {e}"))
            };
            let reviewed = self.review_unit(
                &part_request,
                spend,
                Some(PartRun {
                    label,
                    on_dispatch: &on_dispatch,
                }),
                &result_stem,
            );
            let (status, detail) = match &reviewed.outcome {
                ReviewOutcome::NoFindings => (PartStatus::Passed, None),
                ReviewOutcome::Findings(_) => (PartStatus::Findings, None),
                ReviewOutcome::Unavailable(detail) => {
                    (PartStatus::Unavailable, Some(detail.clone()))
                }
            };
            let settled = ledger.finish_review_part(
                &run_id,
                sha,
                row.index,
                &PartResult {
                    status,
                    dispatch_id: reviewed.dispatch_id.clone(),
                    result_path: reviewed
                        .result_path
                        .as_ref()
                        .map(|path| path.to_string_lossy().into_owned()),
                    result_sha256: reviewed.result_sha256.clone(),
                    detail,
                },
            );
            if let Err(e) = settled {
                return ReviewOutcome::Unavailable(format!(
                    "the parts manifest could not record the end of {label}: {e}"
                ));
            }
            match reviewed.outcome {
                ReviewOutcome::NoFindings => {}
                ReviewOutcome::Findings(text) => {
                    return ReviewOutcome::Findings(format!("review {label} found:\n{text}"));
                }
                ReviewOutcome::Unavailable(detail) => {
                    return ReviewOutcome::Unavailable(format!(
                        "review {label} could not be had: {detail}"
                    ));
                }
            }
        }
        ReviewOutcome::NoFindings
    }
}

/// Is the manifest on record for another patch than the one being reviewed?
/// The parts are pinned by hash when first planned; a patch that cuts
/// differently now is not the review the manifest describes.
fn manifest_differs(planned: &[PlannedPart], rows: &[ReviewPartRow]) -> Option<String> {
    let same = planned.len() == rows.len()
        && planned
            .iter()
            .zip(rows)
            .all(|(part, row)| part.index == row.index && part.patch_sha256 == row.patch_sha256);
    (!same).then(|| {
        "the parts manifest on record for this run and candidate was planned for another \
         patch than the one being reviewed"
            .to_string()
    })
}

/// A part that already found something stays what it found: its answer is
/// read back from the kept artifact.
fn findings_on_record(row: &ReviewPartRow, label: PartLabel) -> ReviewOutcome {
    let text = row
        .result_path
        .as_deref()
        .map(std::fs::read_to_string)
        .transpose()
        .map_err(|e| format!("its answer could not be read back: {e}"))
        .and_then(|text| text.ok_or_else(|| "it has no kept answer".to_string()));
    match text.map(|text| review_verdict(&text)) {
        Ok(ReviewOutcome::Findings(text)) => {
            ReviewOutcome::Findings(format!("review {label} found:\n{text}"))
        }
        Ok(_) | Err(_) => ReviewOutcome::Unavailable(format!(
            "review {label} is recorded as having found something, but the kept answer \
             does not say what"
        )),
    }
}

/// Create `path` with `bytes` and sync it, or find it already there with the
/// same bytes. A file that exists with other bytes is an error: artifacts of
/// a review in parts are immutable.
pub(in crate::runner) fn write_immutable(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => {
            file.write_all(bytes)?;
            file.sync_all()?;
            // The new name is durable too.
            match path.parent() {
                Some(dir) => std::fs::File::open(dir)?.sync_all(),
                None => Ok(()),
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if std::fs::read(path)? == bytes {
                Ok(())
            } else {
                Err(std::io::Error::other(format!(
                    "{} exists with other content; part artifacts are immutable",
                    path.display()
                )))
            }
        }
        Err(e) => Err(e),
    }
}

/// Write each part as `<index>.patch` under `dir`, counted from one, and
/// name it for the manifest. A part file that exists must carry the same
/// bytes: part artifacts are immutable.
fn write_parts(dir: &Path, parts: &[Vec<u8>]) -> std::io::Result<Vec<PlannedPart>> {
    std::fs::create_dir_all(dir)?;
    let mut planned = Vec::with_capacity(parts.len());
    for (at, bytes) in parts.iter().enumerate() {
        let index = u32::try_from(at + 1).unwrap_or(u32::MAX);
        let path = dir.join(format!("{index}.patch"));
        write_immutable(&path, bytes)?;
        planned.push(PlannedPart {
            index,
            patch_path: path.to_string_lossy().into_owned(),
            patch_sha256: sha256_hex(bytes),
        });
    }
    Ok(planned)
}
