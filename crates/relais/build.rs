//! Embeds the Claude Code plugin (`claude-plugin/`) in the binary, so a
//! relais installed from a release installs the plugin with nothing but
//! itself. Every file under the plugin directory goes in except the
//! development-only ones named by `is_development_only`; the list is
//! generated, so a new plugin file can never be left out by hand.
//! `install::plugin`'s tests assert the embedded set against the disk.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// What the plugin directory holds for developing it, not for running it:
/// its tests, the generated type declarations and the editor's tsconfig.
fn is_development_only(relative: &str) -> bool {
    relative.starts_with("tests/")
        || relative.starts_with(".claude-plugin/types/")
        || relative == "tsconfig.json"
}

/// Every file under `dir`, as `/`-separated paths relative to `root`.
fn files_under(root: &Path, dir: &Path, found: &mut Vec<String>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            files_under(root, &path, found);
        } else {
            let relative = path
                .strip_prefix(root)
                .expect("under the plugin root")
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            if !is_development_only(&relative) {
                found.push(relative);
            }
        }
    }
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let root = manifest.join("../../claude-plugin");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut files = Vec::new();
    files_under(&root, &root, &mut files);
    files.sort();
    let mut list = String::from("&[\n");
    for relative in &files {
        let absolute = root.join(relative);
        writeln!(
            list,
            "    ({relative:?}, include_bytes!({:?})),",
            absolute.to_string_lossy()
        )
        .expect("writing to a String");
    }
    list.push_str("]\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets it"));
    std::fs::write(out.join("plugin_files.rs"), list).expect("write plugin_files.rs");
}
