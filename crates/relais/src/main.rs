//! CLI entry point. The full command surface (SPEC §3, §17, §23) is declared
//! here up front; handlers land milestone by milestone and unimplemented
//! ones exit with a clear message instead of pretending.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "relais",
    version,
    about = "Coding-agent execution companion: routing, context, verification, accounting"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Diagnose environment, integrations, coordinator, ledger and registry
    Doctor {
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Create a relais.toml policy for this repository
    Init,
    /// Preflight a task contract: validate, resolve base, explain the route
    /// without launching a model (SPEC §3)
    Plan {
        /// Path to the task contract
        #[arg(long = "task")]
        task: PathBuf,
    },
    /// Validate the contract and start an execution (SPEC §3)
    Run {
        /// Path to the task contract
        #[arg(long = "task")]
        task: PathBuf,
    },
    /// Show runs, or one run's current state and evidence
    Status { run_id: Option<String> },
    /// Explain one run: route reasons, transitions, costs, decisions
    Explain { run_id: String },
    /// Reconcile interrupted state; never blindly repeats the last command
    /// (SPEC §3)
    Resume { run_id: String },
    /// Cost and outcome reporting since a date (SPEC §11)
    Report {
        /// Inclusive lower bound, e.g. 2026-09-01
        #[arg(long)]
        since: Option<String>,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Owned-dataset and learned-routing operations (SPEC §17)
    Dataset {
        #[command(subcommand)]
        cmd: DatasetCommand,
    },
    /// Fit feature transforms and predictors on an owned dataset
    Train,
    /// Compare a candidate artifact with deterministic baselines on held-out
    /// data
    Evaluate {
        /// Artifact ID to evaluate
        #[arg(long = "artifact")]
        artifact: String,
    },
    /// Atomically activate an evaluated artifact; previous artifact stays
    /// available for rollback (SPEC §17)
    Promote { artifact_id: String },
    /// Record a final outcome for an accepted candidate (SPEC §20)
    Feedback {
        run_id: String,
        /// What actually happened to the accepted change
        #[arg(long = "outcome")]
        outcome: FeedbackOutcome,
    },
    /// Install the Claude Code integration; preview-first, --write applies
    /// (SPEC §3)
    Install {
        /// Install the Claude Code skill and agent definitions
        #[arg(long = "claude")]
        claude: bool,
        /// Apply the reviewed changes instead of previewing
        #[arg(long)]
        write: bool,
    },
    /// Remove only owned, unchanged artifacts (SPEC §3)
    Uninstall {
        /// Remove the Claude Code skill and agent definitions
        #[arg(long = "claude")]
        claude: bool,
        /// Apply the removal instead of previewing
        #[arg(long)]
        write: bool,
    },
    /// Coordinator operations (SPEC §23). Started lazily by the CLI when
    /// anything needs it; --daemon is the internal foreground form.
    Coordinator {
        /// Run the shared per-user coordinator in the foreground
        #[arg(long)]
        daemon: bool,
    },
}

#[derive(Subcommand)]
enum DatasetCommand {
    /// Snapshot a versioned dataset from the ledger (SPEC §17)
    Build,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum FeedbackOutcome {
    /// Merged and unchanged
    Accepted,
    /// Needed corrections; magnitude and evidence recorded where available
    Corrected,
    /// Backed out
    Reverted,
    /// A later user-reported regression was confirmed
    Regression,
}

fn main() {
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Doctor { .. } => stub("doctor", "M7"),
        Command::Init => stub("init", "M1"),
        Command::Plan { .. } => stub("plan", "M3"),
        Command::Run { .. } => stub("run", "M5"),
        Command::Status { .. } => stub("status", "M7"),
        Command::Explain { .. } => stub("explain", "M7"),
        Command::Resume { .. } => stub("resume", "M5"),
        Command::Report { .. } => stub("report", "M7"),
        Command::Dataset { cmd } => match cmd {
            DatasetCommand::Build => stub("dataset build", "M8"),
        },
        Command::Train => stub("train", "M8"),
        Command::Evaluate { .. } => stub("evaluate", "M8"),
        Command::Promote { .. } => stub("promote", "M8"),
        Command::Feedback { .. } => stub("feedback", "M7"),
        Command::Install { .. } => stub("install --claude", "M9"),
        Command::Uninstall { .. } => stub("uninstall --claude", "M9"),
        Command::Coordinator { .. } => stub("coordinator", "M6"),
    };
    std::process::exit(code);
}

fn stub(name: &str, milestone: &str) -> i32 {
    eprintln!("relais {name}: not implemented yet (milestone {milestone})");
    2
}
