use colored::Colorize;
use testkit_core::{ConformanceSummary, MatrixReport, TestResult, TestStatus};

pub fn print(report: &MatrixReport) {
    println!(
        "\n{} {} {} {}",
        "==>".cyan().bold(),
        "conformance report:".bold(),
        report.plugin_name.bold(),
        format!("v{}", report.plugin_version).dimmed()
    );
    println!(
        "    {} {}   {} {}",
        "kind:".dimmed(),
        report.plugin_kind,
        "protocol:".dimmed(),
        report.protocol_version,
    );
    println!();

    for r in &report.scenarios {
        print_one(r);
    }

    let summary = ConformanceSummary::from_results(&report.scenarios);
    println!();
    println!(
        "{} total {}   passed {}   failed {}   skipped {}",
        "summary:".bold(),
        summary.total,
        format!("{}", summary.passed).green(),
        if summary.failed == 0 {
            format!("{}", summary.failed).dimmed()
        } else {
            format!("{}", summary.failed).red().bold()
        },
        format!("{}", summary.skipped).yellow(),
    );
    if summary.overall_pass() {
        println!("{}", "OVERALL: PASS".green().bold());
    } else {
        println!("{}", "OVERALL: FAIL".red().bold());
    }
}

fn print_one(r: &TestResult) {
    let (badge, name_color) = match &r.status {
        TestStatus::Pass => ("[PASS]".green().bold(), r.scenario.normal()),
        TestStatus::Fail { .. } => ("[FAIL]".red().bold(), r.scenario.red().bold()),
        TestStatus::Skip { .. } => ("[SKIP]".yellow().bold(), r.scenario.yellow()),
    };
    println!(
        "  {}  {:<28}  {}",
        badge,
        name_color,
        format!("{}ms", r.duration_ms).dimmed()
    );

    match &r.status {
        TestStatus::Fail { reason } => {
            println!("        {} {}", "reason:".red(), reason);
        }
        TestStatus::Skip { reason } => {
            println!("        {} {}", "skip:".yellow(), reason);
        }
        TestStatus::Pass => {}
    }
    for d in &r.diagnostics {
        println!("        {} {}", "note:".dimmed(), d);
    }
}
