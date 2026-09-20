//! Machine-local directory conventions.
//!
//! Settings live under `~/.config/relais/`, mutable state (ledger, registry,
//! run artifacts) under `~/.local/state/relais/`. Constructed from the home
//! directory rather than a platform abstraction, so the paths are the same
//! everywhere and predictable in reports. Constructors that consume these
//! take explicit paths instead, which is what keeps tests isolated.
//!
//! `RELAIS_CONFIG_DIR` and `RELAIS_STATE_DIR` relocate both, which moves
//! the machine authority (trust grants, spending ceilings, permissions)
//! and the ledger with them. That is deliberate — the release scenarios
//! drive a whole isolated machine through them, and the worker's
//! environment is stripped of both so a nested `relais` cannot inherit a
//! relocated authority — but it is also invisible, so `doctor` names the
//! directories in effect and flags the ones the environment supplied.

use std::ffi::OsString;
use std::path::PathBuf;

/// The environment variables that relocate the two directories. Named
/// here so `doctor` and the paths themselves cannot disagree about the
/// spelling.
pub const CONFIG_DIR_ENV: &str = "RELAIS_CONFIG_DIR";
pub const STATE_DIR_ENV: &str = "RELAIS_STATE_DIR";

/// Neither `HOME` nor `USERPROFILE` is set, so there is no directory to
/// build the defaults from. An error, never a panic: the CLI prints it
/// and exits, and `doctor` reports it as a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HomeUnset;

impl std::fmt::Display for HomeUnset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "HOME is not set (nor USERPROFILE); relais keeps its settings under \
             $HOME/.config/relais and its state under $HOME/.local/state/relais. \
             Set HOME, or point RELAIS_CONFIG_DIR and RELAIS_STATE_DIR at explicit \
             directories",
        )
    }
}

impl std::error::Error for HomeUnset {}

/// Resolve the home directory from a variable lookup. Taking the lookup
/// as an argument keeps the unset case testable: `std::env::set_var` is
/// process-global and racy under a threaded test runner.
fn resolve_home(var: impl Fn(&str) -> Option<OsString>) -> Result<PathBuf, HomeUnset> {
    var("HOME")
        .or_else(|| var("USERPROFILE"))
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or(HomeUnset)
}

/// The home directory, or the reason there is none.
pub fn home_dir_checked() -> Result<PathBuf, HomeUnset> {
    resolve_home(|name| std::env::var_os(name))
}

/// The home directory for callers that have nowhere to return an error
/// to (the CLI's own path constructors). An unset `HOME` leaves relais
/// with no directory to read or write, so it is a diagnosed exit rather
/// than a panic with a backtrace.
pub fn home_dir() -> PathBuf {
    home_dir_checked().unwrap_or_else(|e| {
        eprintln!("relais: {e}");
        std::process::exit(2);
    })
}

/// The directory an environment variable relocates, when it does. An
/// empty value is no value: it would otherwise resolve to the process's
/// working directory, which is a repository checkout, not machine state.
pub fn dir_override(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

pub fn config_dir() -> PathBuf {
    dir_override(CONFIG_DIR_ENV).unwrap_or_else(|| home_dir().join(".config").join("relais"))
}

pub fn machine_settings_path() -> PathBuf {
    config_dir().join("machine.toml")
}

pub fn state_dir() -> PathBuf {
    dir_override(STATE_DIR_ENV)
        .unwrap_or_else(|| home_dir().join(".local").join("state").join("relais"))
}

pub fn ledger_path() -> PathBuf {
    state_dir().join("ledger.sqlite")
}

pub fn registry_dir() -> PathBuf {
    state_dir().join("registry")
}

pub fn runs_dir() -> PathBuf {
    state_dir().join("runs")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(*value))
        }
    }

    #[test]
    fn home_comes_from_home_then_userprofile() {
        assert_eq!(
            resolve_home(lookup(&[("HOME", "/h"), ("USERPROFILE", "C:\\u")])),
            Ok(PathBuf::from("/h"))
        );
        // Windows spells it USERPROFILE; the crate builds there too.
        assert_eq!(
            resolve_home(lookup(&[("USERPROFILE", "C:\\u")])),
            Ok(PathBuf::from("C:\\u"))
        );
    }

    #[test]
    fn an_unset_home_is_an_error_not_a_panic() {
        let err = resolve_home(lookup(&[])).expect_err("no home");
        assert_eq!(err, HomeUnset);
        let rendered = err.to_string();
        assert!(rendered.contains("HOME is not set"), "{rendered}");
        // The message says what to do instead, because the two overrides
        // are the way out for a machine without a home directory.
        assert!(rendered.contains(CONFIG_DIR_ENV), "{rendered}");
        assert!(rendered.contains(STATE_DIR_ENV), "{rendered}");
    }

    #[test]
    fn an_empty_home_is_no_home() {
        assert_eq!(resolve_home(lookup(&[("HOME", "")])), Err(HomeUnset));
    }
}
