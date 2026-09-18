//! Machine-local directory conventions.
//!
//! Settings live under `~/.config/relais/`, mutable state (ledger, registry,
//! run artifacts) under `~/.local/state/relais/`. Constructed from the home
//! directory rather than a platform abstraction, so the paths are the same
//! everywhere and predictable in reports. Constructors that consume these
//! take explicit paths instead, which is what keeps tests isolated.

use std::path::PathBuf;

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .expect("HOME or USERPROFILE is set")
}

pub fn config_dir() -> PathBuf {
    std::env::var_os("RELAIS_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".config").join("relais"))
}

pub fn machine_settings_path() -> PathBuf {
    config_dir().join("machine.toml")
}

pub fn state_dir() -> PathBuf {
    std::env::var_os("RELAIS_STATE_DIR")
        .map(PathBuf::from)
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
