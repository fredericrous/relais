//! The routing mode (plan, decision 1, R1b): `on` whenever an envelope is
//! in machine.toml, and the environment can only narrow it. The plugin
//! follows the mode it is served. Pure.

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

/// The mode and the reason for it, from whether an envelope is in
/// machine.toml and the value of [`MODE_ENV`].
pub fn resolve_mode(envelope: bool, env: Option<&str>) -> (Mode, String) {
    let (base, reason) = if envelope {
        (Mode::On, "envelope recorded".to_string())
    } else {
        (Mode::Shadow, "no envelope".to_string())
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
    fn the_envelope_alone_turns_routing_on() {
        assert_eq!(
            resolve_mode(true, None),
            (Mode::On, "envelope recorded".into())
        );
        assert_eq!(
            resolve_mode(false, None),
            (Mode::Shadow, "no envelope".into())
        );
    }

    #[test]
    fn the_environment_only_narrows() {
        assert_eq!(resolve_mode(true, Some("off")).0, Mode::Off);
        assert_eq!(
            resolve_mode(true, Some("shadow")),
            (Mode::Shadow, "env RELAIS_SESSION_ROUTING=shadow".into())
        );
        assert_eq!(resolve_mode(false, Some("off")).0, Mode::Off);
        // `on` in the environment never turns routing on by itself.
        assert_eq!(resolve_mode(false, Some("on")).0, Mode::Shadow);
        assert_eq!(resolve_mode(true, Some("on")).0, Mode::On);
        assert_eq!(resolve_mode(true, Some("ON!")).0, Mode::Shadow);
        assert_eq!(resolve_mode(true, Some("")).0, Mode::On);
    }
}
