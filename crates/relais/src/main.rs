//! CLI entry point (SPEC §3, §17, §23), and nothing else: argument
//! parsing, dispatch and one module per command group live in [`cli`], the
//! exit-code table in [`cli::outcome`], and the handlers' substance in the
//! library modules.

mod cli;

fn main() {
    cli::main()
}
