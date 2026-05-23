mod protocol;
mod report;
mod scenarios;
mod spawn;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use colored::Colorize;
use testkit_core::ConformanceSummary;

#[derive(Parser, Debug)]
#[command(
    name = "animus-plugin-harness",
    version,
    about = "Conformance test harness for Animus provider plugins."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run conformance scenarios against a plugin binary.
    Conformance {
        /// Path to the plugin binary to exercise.
        #[arg(long)]
        plugin: PathBuf,
        /// Directory of scenario YAML files. Defaults to the bundled baseline.
        #[arg(long)]
        scenarios: Option<PathBuf>,
        /// Optional path to dump a JSON MatrixReport.
        #[arg(long)]
        report_json: Option<PathBuf>,
        /// Run a single named scenario instead of all.
        #[arg(long)]
        only: Option<String>,
    },
    /// Print resolved scenario metadata without running anything.
    ListScenarios {
        #[arg(long)]
        scenarios: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.cmd {
        Command::Conformance {
            plugin,
            scenarios,
            report_json,
            only,
        } => run_conformance(plugin, scenarios, report_json, only).await,
        Command::ListScenarios { scenarios } => list_scenarios(scenarios),
    }
}

async fn run_conformance(
    plugin: PathBuf,
    scenarios: Option<PathBuf>,
    report_json: Option<PathBuf>,
    only: Option<String>,
) -> ExitCode {
    let resolved = match scenarios::resolve(scenarios) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{} {}", "error:".red().bold(), e);
            return ExitCode::from(2);
        }
    };

    let report = match crate::protocol::run_all(plugin, resolved, only).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{} harness failed: {}", "error:".red().bold(), e);
            return ExitCode::from(2);
        }
    };

    crate::report::print(&report);

    if let Some(path) = report_json {
        if let Err(e) = std::fs::write(
            &path,
            serde_json::to_vec_pretty(&report).unwrap_or_default(),
        ) {
            eprintln!("{} could not write report: {}", "warn:".yellow().bold(), e);
        } else {
            println!("\nreport written to {}", path.display());
        }
    }

    let summary = ConformanceSummary::from_results(&report.scenarios);
    if summary.overall_pass() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn list_scenarios(scenarios: Option<PathBuf>) -> ExitCode {
    match scenarios::resolve(scenarios) {
        Ok(list) => {
            for s in list {
                println!("{}\t{}", s.name, s.description);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{} {}", "error:".red().bold(), e);
            ExitCode::from(2)
        }
    }
}
