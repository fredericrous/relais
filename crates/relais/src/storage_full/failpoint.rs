//! The failpoint's state: a script armed for the current thread by a test.
//! Compiled only under `cfg(test)`: no failure can be injected into a
//! release binary, where `at` and `probe` do nothing but their real work.

#![cfg(test)]

use super::*;
use std::cell::RefCell;

/// One injected failure: `site` fails once, and only after `after` has
/// been reached (so a failure can be placed past the candidate's
/// snapshot without counting writes).
#[derive(Debug, Clone, Copy)]
pub struct Fire {
    pub site: Site,
    pub after: Option<Site>,
}

/// Space that reads high while the ballast is gone and low while it is
/// there, as a real volume does.
#[derive(Debug)]
pub struct BallastProbe {
    pub ballast: PathBuf,
    pub low: u64,
    pub high: u64,
    pub keeps_blocks: bool,
}

impl DiskProbe for BallastProbe {
    fn volume_of(&self, _path: &Path) -> Option<crate::disk::Volume> {
        let there = self.ballast.exists();
        Some(crate::disk::Volume {
            id: 1,
            mount: "/stub".into(),
            available: if there || self.keeps_blocks {
                self.low
            } else {
                self.high
            },
        })
    }
}

/// Space that reads the same whatever happens.
#[derive(Debug)]
pub struct FixedProbe(pub u64);

impl DiskProbe for FixedProbe {
    fn volume_of(&self, _path: &Path) -> Option<crate::disk::Volume> {
        Some(crate::disk::Volume {
            id: 1,
            mount: "/stub".into(),
            available: self.0,
        })
    }
}

#[derive(Debug, Default)]
pub struct Script {
    pub fires: Vec<Fire>,
    /// Once any failure fired, every later ledger write fails too.
    pub ledger_stays_full: bool,
    /// What space reads as, in place of `statvfs`.
    pub probe: Option<Arc<dyn DiskProbe>>,
    pub seen: Vec<Site>,
    pub fired: bool,
}

thread_local! {
    static SCRIPT: RefCell<Option<Script>> = const { RefCell::new(None) };
}

/// Disarms the failpoint when dropped.
pub struct Armed;

impl Drop for Armed {
    fn drop(&mut self) {
        SCRIPT.with(|script| *script.borrow_mut() = None);
    }
}

pub fn arm(script: Script) -> Armed {
    SCRIPT.with(|slot| *slot.borrow_mut() = Some(script));
    Armed
}

pub fn stub_probe() -> Option<Arc<dyn DiskProbe>> {
    SCRIPT.with(|script| script.borrow().as_ref().and_then(|s| s.probe.clone()))
}

pub fn hit(site: Site) -> io::Result<()> {
    SCRIPT.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(script) = slot.as_mut() else {
            return Ok(());
        };
        let full = || io::Error::new(io::ErrorKind::StorageFull, "injected: no space left");
        if site == Site::Ledger && script.ledger_stays_full && script.fired {
            return Err(full());
        }
        let due = script.fires.iter().position(|fire| {
            fire.site == site && fire.after.is_none_or(|after| script.seen.contains(&after))
        });
        script.seen.push(site);
        match due {
            Some(index) => {
                script.fires.remove(index);
                script.fired = true;
                Err(full())
            }
            None => Ok(()),
        }
    })
}
