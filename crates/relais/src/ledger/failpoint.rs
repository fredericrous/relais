//! Test-only crash points for the successor outbox. A test arms a name on
//! its own thread, the code under test reaches `hit(name)` and "crashes"
//! (panics) there, and the test then runs recovery on a fresh connection.
//! Compiled only under `cfg(test)`: no env var or flag reaches a release
//! binary.

#![cfg(test)]

use std::cell::RefCell;

pub const AFTER_DECIDE_COMMIT: &str = "after_decide_commit";
pub const AFTER_CLAIM: &str = "after_claim";
pub const AFTER_LAUNCH: &str = "after_launch";

thread_local! {
    static ARMED: RefCell<Option<&'static str>> = const { RefCell::new(None) };
}

/// Arm `name` on this thread until it fires or `disarm` runs.
pub fn arm(name: &'static str) {
    ARMED.with(|armed| *armed.borrow_mut() = Some(name));
}

pub fn disarm() {
    ARMED.with(|armed| *armed.borrow_mut() = None);
}

/// Crash here if `name` is armed. Fires once.
pub fn hit(name: &str) {
    let fire = ARMED.with(|armed| {
        let mut armed = armed.borrow_mut();
        if *armed == Some(name) {
            *armed = None;
            true
        } else {
            false
        }
    });
    if fire {
        panic!("failpoint {name}");
    }
}
