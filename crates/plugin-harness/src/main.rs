mod report;

use plugin_harness::{protocol, scenarios};

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use colored::Colorize;
use testkit_core::{ConformanceSummary, MatrixReport};

#[derive(Parser, Debug)]
#[command(
    name = "animus-plugin-harness",
    version,
    about = "Conformance test harness for Animus plugins."
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
        /// Which baseline suite to run. Defaults to provider for back-compat.
        #[arg(long, value_enum, default_value_t = Kind::Provider)]
        kind: Kind,
        /// Directory of scenario YAML files (provider only).
        #[arg(long)]
        scenarios: Option<PathBuf>,
        /// Optional path to dump a JSON MatrixReport.
        #[arg(long)]
        report_json: Option<PathBuf>,
        /// Run a single named scenario instead of all (provider only).
        #[arg(long)]
        only: Option<String>,
    },
    /// Print resolved scenario metadata without running anything.
    ListScenarios {
        #[arg(long, value_enum, default_value_t = Kind::Provider)]
        kind: Kind,
        #[arg(long)]
        scenarios: Option<PathBuf>,
    },
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum Kind {
    Provider,
    Subject,
    Transport,
    Trigger,
    LogStorage,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.cmd {
        Command::Conformance {
            plugin,
            kind,
            scenarios,
            report_json,
            only,
        } => run_conformance(plugin, kind, scenarios, report_json, only).await,
        Command::ListScenarios { kind, scenarios } => list_scenarios(kind, scenarios),
    }
}

async fn run_conformance(
    plugin: PathBuf,
    kind: Kind,
    scenarios: Option<PathBuf>,
    report_json: Option<PathBuf>,
    only: Option<String>,
) -> ExitCode {
    let report = match kind {
        Kind::Provider => {
            let resolved = match scenarios::resolve(scenarios) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("{} {}", "error:".red().bold(), e);
                    return ExitCode::from(2);
                }
            };
            match protocol::run_all(plugin, resolved, only).await {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("{} harness failed: {}", "error:".red().bold(), e);
                    return ExitCode::from(2);
                }
            }
        }
        Kind::Subject => match subject_conformance::run_conformance(&plugin).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("{} subject harness failed: {}", "error:".red().bold(), e);
                return ExitCode::from(2);
            }
        },
        Kind::Transport => match transport_conformance::run_conformance(&plugin).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("{} transport harness failed: {}", "error:".red().bold(), e);
                return ExitCode::from(2);
            }
        },
        Kind::Trigger => match trigger_conformance::run_conformance(&plugin).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("{} trigger harness failed: {}", "error:".red().bold(), e);
                return ExitCode::from(2);
            }
        },
        Kind::LogStorage => match log_storage_conformance::run_conformance(&plugin).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!(
                    "{} log-storage harness failed: {}",
                    "error:".red().bold(),
                    e
                );
                return ExitCode::from(2);
            }
        },
    };

    report::print(&report);
    maybe_write_report(&report, report_json);

    let summary = ConformanceSummary::from_results(&report.scenarios);
    if summary.overall_pass() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn maybe_write_report(report: &MatrixReport, report_json: Option<PathBuf>) {
    if let Some(path) = report_json {
        match std::fs::write(
            &path,
            serde_json::to_vec_pretty(&report).unwrap_or_default(),
        ) {
            Ok(_) => println!("\nreport written to {}", path.display()),
            Err(e) => {
                eprintln!("{} could not write report: {}", "warn:".yellow().bold(), e);
            }
        }
    }
}

fn list_scenarios(kind: Kind, scenarios: Option<PathBuf>) -> ExitCode {
    match kind {
        Kind::Provider => match scenarios::resolve(scenarios) {
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
        },
        Kind::Subject => {
            for s in subject_conformance::baseline_scenarios() {
                println!("{}\t{}", s.name, s.description);
            }
            ExitCode::SUCCESS
        }
        Kind::Transport => {
            for s in transport_conformance::baseline_scenarios() {
                println!("{}\t{}", s.name, s.description);
            }
            ExitCode::SUCCESS
        }
        Kind::Trigger => {
            for s in trigger_conformance::baseline_scenarios() {
                println!("{}\t{}", s.name, s.description);
            }
            ExitCode::SUCCESS
        }
        Kind::LogStorage => {
            for s in log_storage_conformance::baseline_scenarios() {
                println!("{}\t{}", s.name, s.description);
            }
            ExitCode::SUCCESS
        }
    }
}
