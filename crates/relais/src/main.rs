//! CLI entry point (SPEC §3, §17, §23). Handlers live in the library
//! modules; this file is argument parsing, wiring and exit codes. The
//! exit-code table lives in [`cli::outcome`].

mod cli;

fn main() {
    cli::main()
}
