//! relais never launches `claude -p`: every dispatch is a native agent the
//! relais plugin spawns (SPEC §23). The only argument relais passes to the
//! `claude` binary is a probe's `--version` or `--help`; a `"-p"` argument
//! in a source that names that binary is the headless mode coming back.

use std::fs;
use std::path::{Path, PathBuf};

fn sources(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("a readable source directory") {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name != "tests") {
                sources(&path, found);
            }
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
}

/// The code of a source file: no comments, and nothing from its first
/// `#[cfg(test)]` on.
fn non_test_code(text: &str) -> String {
    text.lines()
        .take_while(|line| line.trim() != "#[cfg(test)]")
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn names_the_claude_binary(code: &str) -> bool {
    ["RELAIS_CLAUDE_BIN", "which(\"claude\")", "\"claude\""]
        .iter()
        .any(|needle| code.contains(needle))
}

#[test]
fn no_source_builds_a_dash_p_argument_for_claude() {
    let mut files = Vec::new();
    sources(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    assert!(!files.is_empty(), "the crate's sources were found");
    let mut launch_sites = Vec::new();
    let mut probes = 0;
    for file in &files {
        let code = non_test_code(&fs::read_to_string(file).expect("a readable source"));
        if !names_the_claude_binary(&code) {
            continue;
        }
        if code.contains("\"-p\"") {
            launch_sites.push(file.display().to_string());
        }
        if code.contains("\"--version\"") && code.contains("\"--help\"") {
            probes += 1;
        }
    }
    assert!(
        launch_sites.is_empty(),
        "relais never launches `claude -p`; a \"-p\" argument is built in {launch_sites:?}"
    );
    assert!(
        probes >= 1,
        "`--version` and `--help` remain, in the prober"
    );
}

#[test]
fn the_scan_sees_a_dash_p_argument_for_claude() {
    let code = non_test_code(
        "let mut c = Command::new(which(\"claude\")?);\nc.args([\"-p\", \"--model\"]);\n#[cfg(test)]\nmod tests {}",
    );
    assert!(names_the_claude_binary(&code) && code.contains("\"-p\""));
}
