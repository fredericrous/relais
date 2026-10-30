//! The disk floor and its reservation (SPEC §23, ADR-0010).
//!
//! A run writes to several places: the state directory, the repository, its
//! worktrees and `TMPDIR`. Each is measured with `statvfs`'s `f_bavail`
//! (bytes available to this user), and places on one device are one volume
//! (`st_dev`). A heavy lease is admitted only while what is available,
//! less the estimates of the leases already running on the volume, stays
//! above the floor plus its own estimate.
//!
//! The measurement sits behind [`DiskProbe`], so a test stubs the disk; the
//! decision ([`shortfall`]) is a pure function of what was measured. On a
//! platform without `statvfs` the probe knows nothing, the space is
//! `unknown` and nothing is ever held back.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const GIB: u64 = 1024 * 1024 * 1024;

/// `[resources] min_free_bytes` when machine.toml does not say: 8 GiB.
pub const DEFAULT_MIN_FREE_BYTES: u64 = 8 * GIB;

/// What a check is expected to write until it has been measured: 2 GiB.
pub const DEFAULT_CHECK_ESTIMATE_BYTES: u64 = 2 * GIB;

/// One volume as measured: its device, a path that names it, and the bytes
/// available to this user on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Volume {
    pub id: u64,
    pub mount: String,
    pub available: u64,
}

/// Where the space of a path is read. Injectable.
pub trait DiskProbe: Send + Sync + std::fmt::Debug {
    /// The volume holding `path` (or its nearest existing ancestor), or
    /// `None` when it cannot be measured.
    fn volume_of(&self, path: &Path) -> Option<Volume>;
}

/// `statvfs` on unix; nothing elsewhere.
#[derive(Debug, Clone, Copy, Default)]
pub struct StatvfsProbe;

#[cfg(unix)]
impl DiskProbe for StatvfsProbe {
    fn volume_of(&self, path: &Path) -> Option<Volume> {
        use std::os::unix::fs::MetadataExt;
        let existing = path.ancestors().find(|p| p.exists())?;
        let dev = std::fs::metadata(existing).ok()?.dev();
        // The mount point is the highest ancestor still on this device.
        let mount = existing
            .ancestors()
            .take_while(|p| std::fs::metadata(p).is_ok_and(|m| m.dev() == dev))
            .last()
            .unwrap_or(existing);
        let available = crate::procs::available_bytes(existing)?;
        Some(Volume {
            id: dev,
            mount: mount.display().to_string(),
            available,
        })
    }
}

#[cfg(not(unix))]
impl DiskProbe for StatvfsProbe {
    fn volume_of(&self, _path: &Path) -> Option<Volume> {
        None
    }
}

/// The volumes of `paths`, one per device, in the order first met. A path
/// that cannot be measured contributes nothing.
pub fn volumes<P: AsRef<Path>>(probe: &dyn DiskProbe, paths: &[P]) -> Vec<Volume> {
    let mut found: Vec<Volume> = Vec::new();
    for path in paths {
        if let Some(volume) = probe.volume_of(path.as_ref()) {
            if !found.iter().any(|seen| seen.id == volume.id) {
                found.push(volume);
            }
        }
    }
    found
}

/// What a heavy request says about the disk it will write: its estimate
/// and the places it writes. Additive on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskAsk {
    pub estimate_bytes: u64,
    pub paths: Vec<String>,
}

/// Why a volume cannot take a lease now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shortfall {
    pub volume: String,
    pub available: u64,
    pub floor: u64,
    pub reserved: u64,
}

/// The first volume where `available - reserved(volume) < floor +
/// estimate`. Pure: the measurements and the reservations are arguments.
pub fn shortfall(
    volumes: &[Volume],
    reserved: impl Fn(u64) -> u64,
    floor: u64,
    estimate: u64,
) -> Option<Shortfall> {
    volumes.iter().find_map(|volume| {
        let held = reserved(volume.id);
        let free = volume.available.saturating_sub(held);
        (free < floor.saturating_add(estimate)).then(|| Shortfall {
            volume: volume.mount.clone(),
            available: volume.available,
            floor,
            reserved: held,
        })
    })
}

/// Bytes as the doctor and the events say them: `5.2 GiB`.
pub fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / GIB as f64)
}

/// The doctor's line for one volume (its `disk` column names it).
pub fn doctor_line(volume: &Volume, floor: u64, reserved: u64) -> String {
    let below = volume.available.saturating_sub(reserved) < floor;
    format!(
        "{} available on {}, floor {}, reserved {}{}",
        gib(volume.available),
        volume.mount,
        gib(floor),
        gib(reserved),
        if below { " (below floor)" } else { "" }
    )
}

/// Where a check label's last measured growth is kept.
pub trait Growth {
    fn last(&self, label: &str) -> Option<u64>;
    fn record(&self, label: &str, bytes: u64);
}

/// Remembers nothing: every estimate is the default.
pub struct NoGrowth;

impl Growth for NoGrowth {
    fn last(&self, _label: &str) -> Option<u64> {
        None
    }
    fn record(&self, _label: &str, _bytes: u64) {}
}

/// What a run needs to keep the disk floor.
pub struct DiskCheck<'a> {
    /// The floor in bytes; zero disables the check.
    pub floor: u64,
    pub probe: std::sync::Arc<dyn DiskProbe>,
    /// The places the run writes.
    pub paths: Vec<PathBuf>,
    pub growth: &'a (dyn Growth + Sync),
}

impl DiskCheck<'static> {
    /// No floor: nothing is measured and nothing waits.
    pub fn off() -> Self {
        Self {
            floor: 0,
            probe: std::sync::Arc::new(StatvfsProbe),
            paths: Vec::new(),
            growth: &NoGrowth,
        }
    }
}

impl DiskCheck<'_> {
    pub fn enabled(&self) -> bool {
        self.floor > 0 && !self.paths.is_empty()
    }

    /// Bytes the check `label` grew its tree by last time, else 2 GiB.
    pub fn estimate(&self, label: &str) -> u64 {
        self.growth
            .last(label)
            .unwrap_or(DEFAULT_CHECK_ESTIMATE_BYTES)
    }

    /// The first volume that cannot take `estimate` more above the floor
    /// right now, ignoring other leases (the coordinator knows those).
    pub fn short(&self, estimate: u64) -> Option<Shortfall> {
        if !self.enabled() {
            return None;
        }
        shortfall(
            &volumes(self.probe.as_ref(), &self.paths),
            |_| 0,
            self.floor,
            estimate,
        )
    }

    /// What a lease request carries to the coordinator.
    pub fn ask(&self, estimate: u64) -> Option<DiskAsk> {
        self.enabled().then(|| DiskAsk {
            estimate_bytes: estimate,
            paths: self.paths.iter().map(|p| p.display().to_string()).collect(),
        })
    }
}

/// Bytes of the files under `dir`, not following links and leaving `.git`
/// out. What a check's growth is measured on.
pub fn tree_bytes(dir: &Path) -> u64 {
    let mut total = 0u64;
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if entry.file_name() != ".git" {
                    pending.push(entry.path());
                }
            } else if kind.is_file() {
                total = total.saturating_add(entry.metadata().map_or(0, |m| m.len()));
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    fn volume(id: u64, mount: &str, available: u64) -> Volume {
        Volume {
            id,
            mount: mount.into(),
            available,
        }
    }

    #[test]
    fn a_volume_short_of_the_floor_plus_estimate_is_named() {
        let vols = [volume(1, "/", 20 * GIB), volume(2, "/tmp", 9 * GIB)];
        let short = shortfall(&vols, |_| 0, 8 * GIB, 2 * GIB).expect("short");
        assert_eq!(short.volume, "/tmp");
        assert_eq!(
            (short.available, short.floor, short.reserved),
            (9 * GIB, 8 * GIB, 0)
        );
        assert!(shortfall(&vols, |_| 0, 8 * GIB, GIB).is_none());
    }

    #[test]
    fn reservations_come_off_what_is_available() {
        let vols = [volume(1, "/", 11 * GIB)];
        assert!(shortfall(&vols, |_| 0, 8 * GIB, 2 * GIB).is_none());
        let short = shortfall(&vols, |_| 2 * GIB, 8 * GIB, 2 * GIB).expect("short");
        assert_eq!(short.reserved, 2 * GIB);
    }

    #[test]
    fn the_doctor_line_says_below_floor() {
        let line = doctor_line(&volume(1, "/data", 5 * GIB + GIB / 5), 8 * GIB, 2 * GIB);
        assert_eq!(
            line,
            "5.2 GiB available on /data, floor 8.0 GiB, reserved 2.0 GiB (below floor)"
        );
        let ok = doctor_line(&volume(1, "/data", 50 * GIB), 8 * GIB, 0);
        assert!(!ok.contains("below floor"), "{ok}");
    }

    #[cfg(unix)]
    #[test]
    fn the_real_probe_measures_a_directory() {
        let dir = crate::test_support::temp_dir("disk-probe");
        let volume = StatvfsProbe.volume_of(&dir).expect("measured");
        assert!(volume.available > 0);
        let again = StatvfsProbe
            .volume_of(&dir.join("not/yet/there"))
            .expect("nearest ancestor");
        assert_eq!(volume.id, again.id);
    }

    #[test]
    fn tree_bytes_counts_files_and_skips_git() {
        let dir = crate::test_support::temp_dir("disk-tree");
        std::fs::create_dir_all(dir.join("a/.git")).expect("mkdir");
        std::fs::write(dir.join("a/f"), vec![0u8; 1000]).expect("write");
        std::fs::write(dir.join("a/.git/g"), vec![0u8; 5000]).expect("write");
        assert_eq!(tree_bytes(&dir), 1000);
    }
}
