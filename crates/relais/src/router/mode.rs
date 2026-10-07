//! relais's half of the routing mode (plan, decision 1): `on` only with an
//! envelope in machine.toml AND a passing latest R3 row, and the
//! environment can only narrow it. The plugin narrows it again with its
//! own store records (`mode_effective`); nothing here can widen what the
//! plugin decides. Pure.

use serde::Serialize;

/// The environment variable that narrows the mode.
pub const MODE_ENV: &str = "RELAIS_SESSION_ROUTING";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Decides nothing and records nothing.
    Off,
    /// Decides and records; switches nothing.
    Shadow,
    /// Decides, records and applies, within the envelope.
    On,
}

/// The mode and the reason for it, from what relais can see: whether an
/// envelope is in machine.toml, the latest R3 row's verdict (`None` when
/// there is none), and the value of [`MODE_ENV`].
pub fn resolve_mode(envelope: bool, r3_passed: Option<bool>, env: Option<&str>) -> (Mode, String) {
    let (base, reason) = if !envelope {
        (Mode::Shadow, "no envelope".to_string())
    } else if r3_passed != Some(true) {
        (Mode::Shadow, "no r3 pass".to_string())
    } else {
        (Mode::On, "envelope and r3 recorded".to_string())
    };
    let env = env.map(str::trim).filter(|value| !value.is_empty());
    match env {
        None | Some("on") => (base, reason),
        Some("off") => (Mode::Off, format!("env {MODE_ENV}=off")),
        Some("shadow") if base == Mode::On => (Mode::Shadow, format!("env {MODE_ENV}=shadow")),
        Some("shadow") => (base, reason),
        // A value nobody meant is not a reason to route: narrow to
        // shadow, and say why.
        Some(other) => (
            base.min(Mode::Shadow),
            format!("env {MODE_ENV}={other} is not off|shadow|on; shadow"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_needs_both_the_envelope_and_a_passing_r3() {
        assert_eq!(resolve_mode(false, None, None).0, Mode::Shadow);
        assert_eq!(resolve_mode(false, Some(true), None).0, Mode::Shadow);
        assert_eq!(resolve_mode(true, None, None).0, Mode::Shadow);
        assert_eq!(resolve_mode(true, Some(false), None).0, Mode::Shadow);
        assert_eq!(
            resolve_mode(true, Some(true), None),
            (Mode::On, "envelope and r3 recorded".into())
        );
        assert_eq!(resolve_mode(true, Some(false), None).1, "no r3 pass");
        assert_eq!(resolve_mode(false, Some(true), None).1, "no envelope");
    }

    #[test]
    fn the_environment_only_narrows() {
        assert_eq!(resolve_mode(true, Some(true), Some("off")).0, Mode::Off);
        assert_eq!(
            resolve_mode(true, Some(true), Some("shadow")),
            (Mode::Shadow, "env RELAIS_SESSION_ROUTING=shadow".into())
        );
        assert_eq!(resolve_mode(false, None, Some("off")).0, Mode::Off);
        // `on` in the environment never turns routing on by itself.
        assert_eq!(resolve_mode(false, None, Some("on")).0, Mode::Shadow);
        assert_eq!(resolve_mode(true, Some(false), Some("on")).0, Mode::Shadow);
        assert_eq!(resolve_mode(true, Some(true), Some("ON!")).0, Mode::Shadow);
        assert_eq!(resolve_mode(true, Some(true), Some("")).0, Mode::On);
    }
}
