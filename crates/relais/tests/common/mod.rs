//! What every integration test shares: the `relais` command, started clean.

use std::process::Command;

/// A command for the `relais` binary at `bin`, with every `RELAIS_*`
/// variable of the test's own environment removed.
///
/// The suite runs inside relais runs too (`make check` as a verification
/// command), whose environment carries `RELAIS_HOST`, `RELAIS_SESSION_ID`
/// and the rest. A test that means "no host" must not inherit one; each
/// test sets what it means on the returned command.
pub fn relais(bin: &str) -> Command {
    let mut command = Command::new(bin);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("RELAIS_") {
            command.env_remove(&key);
        }
    }
    command
}
