//! Exhaustion is evidence, not a verdict (SPEC §9, §10; plan B4).
//!
//! A check that fails because the machine ran out of disk or memory says
//! nothing reliable about the code it ran against, and a repair worker
//! cannot fix a full disk. Recognising it is a calculation on what the
//! failed check printed (its last 64 KiB) and on how it ended, so it lives
//! here as pure functions: [`classify`] reads an end and a log tail,
//! [`measure`] reads the disk through a [`DiskProbe`], and the sentences a
//! person reads are built from those two. The runner owns what to do with
//! the answer.
//!
//! Three rules shape the classification:
//!
//! - Only a failed check is read. A check that exited 0 passed, whatever
//!   its output says.
//! - relais's own kills are never memory. A timeout or a cancellation ends
//!   [`Ended::TimedOut`] or [`Ended::Cancelled`], which is never classified;
//!   only a process that died on a signal nobody here sent, or reported the
//!   shell's `137` (128 + SIGKILL), is a memory suspect.
//! - A disk signature is combined with the space measured at failure time,
//!   so the sentence can say whether the volume was below the floor.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::disk::{gib, volumes, DiskProbe};
use crate::procs::Ended;
use crate::verify::CheckOutcome;

/// How much of a failed check's output is read: the end of its log.
pub const TAIL_BYTES: u64 = 64 * 1024;

/// What a full disk looks like in a command's output.
pub const DISK_SIGNATURES: [&str; 3] = [
    "No space left on device",
    "os error 28",
    "database or disk is full",
];

/// What running out of memory looks like in a command's output.
pub const MEMORY_SIGNATURES: [&str; 2] = ["Cannot allocate memory", "os error 12"];

/// How the kernel says it killed a process for memory, when its log is
/// visible to the check.
const OOM_NOTES: [&str; 3] = ["Out of memory: Killed process", "oom-kill", "oom_reaper"];

/// The shell's status for a child that died on SIGKILL (128 + 9).
const EXIT_SIGKILL: i32 = 137;

/// Which resource ran out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Resource {
    Disk,
    Memory,
}

impl Resource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disk => "disk",
            Self::Memory => "memory",
        }
    }
}

/// A failed check that shows exhaustion: which resource, what showed it,
/// and the kernel's OOM note when the output carried one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exhaustion {
    pub resource: Resource,
    pub evidence: String,
    pub oom_note: Option<String>,
}

/// The end of `log`, at most [`TAIL_BYTES`], lossily decoded. A log that
/// cannot be read has no tail: nothing is classified from it.
pub fn log_tail(path: &Path) -> String {
    let read = || -> std::io::Result<Vec<u8>> {
        let mut file = std::fs::File::open(path)?;
        let length = file.metadata()?.len();
        file.seek(SeekFrom::Start(length.saturating_sub(TAIL_BYTES)))?;
        let mut bytes = Vec::new();
        file.take(TAIL_BYTES).read_to_end(&mut bytes)?;
        Ok(bytes)
    };
    read()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default()
}

/// Does a failed check's end and output show exhaustion?
pub fn classify(ended: Ended, tail: &str) -> Option<Exhaustion> {
    match ended {
        Ended::Exited(0) | Ended::TimedOut | Ended::Cancelled => return None,
        Ended::Exited(_) | Ended::Signalled => {}
    }
    let oom_note = || {
        tail.lines()
            .find(|line| OOM_NOTES.iter().any(|note| line.contains(note)))
            .map(|line| line.trim().to_string())
    };
    if let Some(signature) = DISK_SIGNATURES.iter().find(|s| tail.contains(**s)) {
        return Some(Exhaustion {
            resource: Resource::Disk,
            evidence: format!("the output says `{signature}`"),
            oom_note: None,
        });
    }
    if let Some(signature) = MEMORY_SIGNATURES.iter().find(|s| tail.contains(**s)) {
        return Some(Exhaustion {
            resource: Resource::Memory,
            evidence: format!("the output says `{signature}`"),
            oom_note: oom_note(),
        });
    }
    let killed = match ended {
        Ended::Signalled => Some("the process died on a signal relais did not send"),
        Ended::Exited(EXIT_SIGKILL) => Some("the process was killed (exit 137, SIGKILL)"),
        _ => None,
    };
    killed.map(|evidence| Exhaustion {
        resource: Resource::Memory,
        evidence: evidence.to_string(),
        oom_note: oom_note(),
    })
}

/// The first failed check of `checks` whose end and log tail show
/// exhaustion, with its label.
pub fn first_exhaustion(checks: &[CheckOutcome]) -> Option<(String, Exhaustion)> {
    checks
        .iter()
        .filter(|check| check.failed())
        .find_map(|check| {
            let ended = check.ended.recorded()?;
            classify(ended, &log_tail(Path::new(&check.log_path)))
                .map(|found| (check.label.clone(), found))
        })
}

/// What the machine had at failure time. A memory failure names no volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Measured {
    pub volume: Option<String>,
    pub available: Option<u64>,
    pub floor: u64,
}

/// For a disk failure, the volume with the least space among `paths` and
/// what it had available; for memory, nothing but the floor.
pub fn measure(
    resource: Resource,
    probe: &dyn DiskProbe,
    paths: &[PathBuf],
    floor: u64,
) -> Measured {
    let tightest = match resource {
        Resource::Memory => None,
        Resource::Disk => volumes(probe, paths)
            .into_iter()
            .min_by_key(|volume| volume.available),
    };
    Measured {
        volume: tightest.as_ref().map(|volume| volume.mount.clone()),
        available: tightest.map(|volume| volume.available),
        floor,
    }
}

/// The transition detail of a `resource_exhausted` or
/// `baseline_resource_exhausted` end. `peak_rss` is the check's peak
/// resident memory when something measured it; nothing does yet, so it is
/// recorded as unknown rather than guessed.
pub fn detail_json(
    check: &str,
    found: &Exhaustion,
    measured: &Measured,
    candidate: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "resource": found.resource,
        "volume": measured.volume,
        "available": measured.available,
        "floor": measured.floor,
        "peak_rss": serde_json::Value::Null,
        "check": check,
        "evidence": found.evidence,
        "oom_note": found.oom_note,
        "candidate": candidate,
    })
}

/// What was observed, in one sentence.
fn observed(check: &str, found: &Exhaustion, measured: &Measured) -> String {
    let mut text = format!(
        "check `{check}` failed and {} ({} exhausted",
        found.evidence,
        found.resource.as_str()
    );
    if let (Some(volume), Some(available)) = (&measured.volume, measured.available) {
        let side = if available < measured.floor {
            "below"
        } else {
            "not below"
        };
        text.push_str(&format!(
            "; {volume} had {} available, {side} the floor of {}",
            gib(available),
            gib(measured.floor)
        ));
    }
    text.push(')');
    if let Some(note) = &found.oom_note {
        text.push_str(&format!("; the kernel noted: {note}"));
    }
    text
}

/// The `needs_decision (resource_exhausted)` sentence. It says plainly that
/// exhaustion was observed and does not prove the code innocent, and it
/// offers the two answers: a `kind: verify` rerun of the same candidate
/// (a successor intent) or a revision.
pub fn candidate_text(check: &str, found: &Exhaustion, measured: &Measured, run: &str) -> String {
    format!(
        "{}. Exhaustion was observed; it is not proof that the code is innocent, since a \
         candidate can itself cause the out-of-memory kill or the disk blow-up. No repair was \
         spent and the candidate is preserved. Once the machine has room, rerun this same \
         candidate as a `kind: verify` successor intent with `relais decide {run} --answer \
         revise --rerun-verify`, or answer `revise` with a changed contract",
        observed(check, found, measured)
    )
}

/// The `blocked (baseline_resource_exhausted)` sentence: the base gave no
/// verdict, so it is not red.
pub fn baseline_text(check: &str, found: &Exhaustion, measured: &Measured) -> String {
    format!(
        "the base revision's {}. The base is not red: exhaustion gives no verdict, so nothing \
         ran against a candidate. Free the resource and run again",
        observed(check, found, measured)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(ended: Ended, tail: &str) -> Option<(Resource, String)> {
        classify(ended, tail).map(|e| (e.resource, e.evidence))
    }

    #[test]
    fn each_disk_signature_is_disk() {
        for signature in DISK_SIGNATURES {
            let got = found(Ended::Exited(1), &format!("error: {signature}\n"));
            assert_eq!(got.map(|g| g.0), Some(Resource::Disk), "{signature}");
        }
    }

    #[test]
    fn each_memory_signature_is_memory() {
        for signature in MEMORY_SIGNATURES {
            let got = found(Ended::Exited(2), &format!("fatal: {signature}\n"));
            assert_eq!(got.map(|g| g.0), Some(Resource::Memory), "{signature}");
        }
    }

    #[test]
    fn a_sigkill_nobody_here_sent_is_memory_and_carries_the_oom_note() {
        let tail = "building\nOut of memory: Killed process 4242 (rustc)\n";
        let got = classify(Ended::Signalled, tail).expect("memory");
        assert_eq!(got.resource, Resource::Memory);
        assert_eq!(
            got.oom_note.as_deref(),
            Some("Out of memory: Killed process 4242 (rustc)")
        );
        assert!(classify(Ended::Exited(EXIT_SIGKILL), "").is_some());
    }

    #[test]
    fn relais_own_kills_and_passes_are_never_exhaustion() {
        let loud = "No space left on device\nCannot allocate memory\n";
        assert_eq!(classify(Ended::TimedOut, loud), None);
        assert_eq!(classify(Ended::Cancelled, loud), None);
        assert_eq!(classify(Ended::Exited(0), loud), None);
        assert_eq!(classify(Ended::Exited(1), "an ordinary failure"), None);
    }

    #[test]
    fn only_the_last_64_kib_of_a_log_is_read() {
        let dir = crate::test_support::temp_dir("exhaustion-tail");
        let path = dir.join("big.log");
        let mut text = String::from("No space left on device\n");
        text.push_str(&"x".repeat(TAIL_BYTES as usize + 10));
        std::fs::write(&path, &text).expect("write");
        let tail = log_tail(&path);
        assert!(tail.len() as u64 <= TAIL_BYTES);
        assert!(
            !tail.contains("No space"),
            "the signature is before the tail"
        );
        assert_eq!(log_tail(&dir.join("absent.log")), "");
    }

    #[test]
    fn the_candidate_text_says_exhaustion_is_not_innocence_and_offers_the_rerun() {
        let found = classify(Ended::Exited(1), "No space left on device").expect("disk");
        let measured = Measured {
            volume: Some("/data".into()),
            available: Some(1 << 30),
            floor: 8 << 30,
        };
        let text = candidate_text("make test", &found, &measured, "run-1");
        assert!(
            text.contains("not proof that the code is innocent"),
            "{text}"
        );
        assert!(text.contains("--answer revise --rerun-verify"), "{text}");
        assert!(text.contains("kind: verify"), "{text}");
        assert!(text.contains("below the floor"), "{text}");
    }
}
