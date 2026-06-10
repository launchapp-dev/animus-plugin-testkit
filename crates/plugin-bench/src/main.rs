use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use animus_plugin_protocol::{
    HostCapabilities, HostInfo, InitializeParams, RpcNotification, RpcRequest, RpcResponse,
    PROTOCOL_VERSION,
};
use animus_provider_protocol::{AgentRunResponse, METHOD_AGENT_RUN, NOTIFICATION_AGENT_OUTPUT};
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use clap::{Parser, ValueEnum};
use colored::Colorize;
use plugin_harness::spawn::PluginRunner;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::time::timeout;

#[derive(Parser, Debug)]
#[command(
    name = "animus-plugin-bench",
    version,
    about = "Provider plugin benchmark matrix: TTFT, throughput, duration, and parse overhead."
)]
struct Cli {
    /// Provider plugin binary to benchmark. Repeat for a provider matrix.
    #[arg(long, required = true)]
    plugin: Vec<PathBuf>,

    /// Benchmark scenario preset. Ignored when --scenario is supplied.
    #[arg(long, value_enum)]
    suite: Option<BenchSuite>,

    /// Mock scenario id. Repeat or comma-separate values. --mock-scenario is
    /// retained as a backward-compatible alias.
    #[arg(long = "scenario", alias = "mock-scenario", value_delimiter = ',')]
    scenarios: Vec<String>,

    /// Model id to place in AgentRunRequest.model. Repeat or comma-separate
    /// values to compare model routing.
    #[arg(
        long = "model",
        alias = "models",
        value_delimiter = ',',
        default_value = "claude-sonnet-4-6"
    )]
    models: Vec<String>,

    /// Measured iterations per plugin/scenario/model cell.
    #[arg(long, default_value_t = 5)]
    iterations: u32,

    /// Unmeasured iterations before each cell. Useful for JIT/cache warmup.
    #[arg(long = "warmup", alias = "warmup-iterations", default_value_t = 1)]
    warmup_iterations: u32,

    /// Per-iteration timeout for the agent/run request.
    #[arg(long, default_value_t = 30_000)]
    timeout_ms: u64,

    /// Prompt sent to each benchmarked run.
    #[arg(long, default_value = "say hi")]
    prompt: String,

    /// Optional machine-readable JSON report.
    #[arg(long)]
    report_json: Option<PathBuf>,

    /// Optional case-summary CSV report.
    #[arg(long)]
    report_csv: Option<PathBuf>,

    /// Stop after the first failed warmup or measured iteration.
    #[arg(long)]
    fail_fast: bool,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum BenchSuite {
    Smoke,
    Streaming,
    Tools,
    Full,
}

impl BenchSuite {
    fn scenarios(self) -> Vec<String> {
        match self {
            BenchSuite::Smoke => vec!["streaming-short"],
            BenchSuite::Streaming => vec!["streaming-short", "streaming-medium", "streaming-long"],
            BenchSuite::Tools => vec!["tool-call-single", "tool-call-parallel"],
            BenchSuite::Full => vec![
                "streaming-short",
                "streaming-medium",
                "streaming-long",
                "tool-call-single",
                "tool-call-parallel",
                "error-recovery",
            ],
        }
        .into_iter()
        .map(str::to_string)
        .collect()
    }
}

#[derive(Debug, Serialize)]
struct BenchmarkSuiteReport {
    schema: &'static str,
    started_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
    host: BenchmarkHost,
    cases: Vec<BenchCaseReport>,
}

impl BenchmarkSuiteReport {
    fn has_failures(&self) -> bool {
        self.cases.iter().any(BenchCaseReport::has_failures)
    }
}

#[derive(Debug, Serialize)]
struct BenchmarkHost {
    name: &'static str,
    version: &'static str,
    command: Vec<String>,
}

#[derive(Debug, Serialize)]
struct BenchCaseReport {
    plugin_name: String,
    plugin_path: PathBuf,
    scenario: String,
    model: String,
    prompt: String,
    iterations_requested: u32,
    warmup_iterations: u32,
    timeout_ms: u64,
    samples: Vec<BenchSample>,
    errors: Vec<BenchError>,
    summary: Option<SummaryStats>,
}

impl BenchCaseReport {
    fn has_failures(&self) -> bool {
        !self.errors.is_empty() || self.samples.len() != self.iterations_requested as usize
    }

    fn status(&self) -> &'static str {
        if self.has_failures() {
            "fail"
        } else {
            "pass"
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct BenchSample {
    iteration: u32,
    ttft_ms: u64,
    duration_ms: u64,
    notification_count: u32,
    output_bytes: usize,
}

#[derive(Debug, Serialize)]
struct BenchError {
    iteration: u32,
    phase: &'static str,
    message: String,
}

#[derive(Debug, Clone, Serialize)]
struct SummaryStats {
    sample_count: usize,
    ttft_min_ms: u64,
    ttft_median_ms: u64,
    ttft_p95_ms: u64,
    ttft_avg_ms: f64,
    ttft_max_ms: u64,
    duration_min_ms: u64,
    duration_median_ms: u64,
    duration_p95_ms: u64,
    duration_avg_ms: f64,
    duration_max_ms: u64,
    notification_avg: f64,
    output_bytes_total: usize,
    throughput_bps: f64,
}

struct RunConfig<'a> {
    plugin: &'a Path,
    scenario: &'a str,
    model: &'a str,
    prompt: &'a str,
    timeout_ms: u64,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(report) if !report.has_failures() => ExitCode::SUCCESS,
        Ok(_) => ExitCode::from(1),
        Err(e) => {
            eprintln!("{} {e}", "error:".red().bold());
            ExitCode::from(1)
        }
    }
}

async fn run(cli: Cli) -> Result<BenchmarkSuiteReport> {
    let started_at = Utc::now();
    let scenarios = resolve_scenarios(&cli);
    let host = BenchmarkHost {
        name: "animus-plugin-bench",
        version: env!("CARGO_PKG_VERSION"),
        command: std::env::args().collect(),
    };

    println!(
        "{} plugins={} scenarios={} models={} iterations={} warmup={}",
        "==> bench suite".cyan().bold(),
        cli.plugin.len(),
        scenarios.len(),
        cli.models.len(),
        cli.iterations,
        cli.warmup_iterations
    );

    let mut cases = Vec::new();
    'suite: for plugin in &cli.plugin {
        for scenario in &scenarios {
            for model in &cli.models {
                let case = run_case(&cli, plugin, scenario, model).await;
                let failed = case.has_failures();
                cases.push(case);
                if failed && cli.fail_fast {
                    break 'suite;
                }
            }
        }
    }

    let report = BenchmarkSuiteReport {
        schema: "animus.plugin_bench.v1",
        started_at,
        finished_at: Utc::now(),
        host,
        cases,
    };
    print_suite_summary(&report);
    maybe_write_json(&report, cli.report_json.as_deref())?;
    maybe_write_csv(&report, cli.report_csv.as_deref())?;
    Ok(report)
}

fn resolve_scenarios(cli: &Cli) -> Vec<String> {
    if !cli.scenarios.is_empty() {
        return cli.scenarios.clone();
    }
    cli.suite
        .map(BenchSuite::scenarios)
        .unwrap_or_else(|| vec!["streaming-medium".to_string()])
}

async fn run_case(cli: &Cli, plugin: &Path, scenario: &str, model: &str) -> BenchCaseReport {
    let plugin_name = plugin_label(plugin);
    println!(
        "\n{} plugin={} scenario={} model={}",
        "==> case".cyan().bold(),
        plugin_name,
        scenario,
        model
    );

    let mut case = BenchCaseReport {
        plugin_name,
        plugin_path: plugin.to_path_buf(),
        scenario: scenario.to_string(),
        model: model.to_string(),
        prompt: cli.prompt.clone(),
        iterations_requested: cli.iterations,
        warmup_iterations: cli.warmup_iterations,
        timeout_ms: cli.timeout_ms,
        samples: Vec::with_capacity(cli.iterations as usize),
        errors: Vec::new(),
        summary: None,
    };

    let config = RunConfig {
        plugin,
        scenario,
        model,
        prompt: &cli.prompt,
        timeout_ms: cli.timeout_ms,
    };

    for iteration in 1..=cli.warmup_iterations {
        match run_once(&config, iteration).await {
            Ok(sample) => println!(
                "  warmup {:>2} ttft {:>5}ms total {:>5}ms notifs {:>4} bytes {:>6}",
                iteration,
                sample.ttft_ms,
                sample.duration_ms,
                sample.notification_count,
                sample.output_bytes
            ),
            Err(error) => {
                let message = error.to_string();
                println!(
                    "  warmup {:>2} {}",
                    iteration,
                    format!("FAIL {message}").red()
                );
                case.errors.push(BenchError {
                    iteration,
                    phase: "warmup",
                    message,
                });
                if cli.fail_fast {
                    return case;
                }
            }
        }
    }

    for iteration in 1..=cli.iterations {
        match run_once(&config, iteration).await {
            Ok(sample) => {
                println!(
                    "  iter   {:>2} ttft {:>5}ms total {:>5}ms notifs {:>4} bytes {:>6}",
                    iteration,
                    sample.ttft_ms,
                    sample.duration_ms,
                    sample.notification_count,
                    sample.output_bytes
                );
                case.samples.push(sample);
            }
            Err(error) => {
                let message = error.to_string();
                println!(
                    "  iter   {:>2} {}",
                    iteration,
                    format!("FAIL {message}").red()
                );
                case.errors.push(BenchError {
                    iteration,
                    phase: "measurement",
                    message,
                });
                if cli.fail_fast {
                    break;
                }
            }
        }
    }

    case.summary = summary_stats(&case.samples);
    print_case_summary(&case);
    case
}

async fn run_once(config: &RunConfig<'_>, iteration: u32) -> Result<BenchSample> {
    let mut runner = PluginRunner::launch_with_scenario(config.plugin, Some(config.scenario))
        .await
        .context("spawn plugin")?;

    let result = async {
        initialize(&mut runner).await?;
        drive_agent_run(&mut runner, config, iteration).await
    }
    .await;

    runner.shutdown().await;
    result
}

async fn initialize(runner: &mut PluginRunner) -> Result<()> {
    let init = RpcRequest::new(
        1,
        "initialize",
        Some(serde_json::to_value(InitializeParams {
            protocol_version: PROTOCOL_VERSION.to_string(),
            host_info: HostInfo {
                name: "animus-plugin-bench".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
            capabilities: HostCapabilities {
                streaming: true,
                progress: false,
                cancellation: false,
            },
        })?),
    );
    send_frame(runner, &serde_json::to_value(init)?).await?;
    read_until_response(runner, 1, Duration::from_secs(10)).await?;
    send_frame(
        runner,
        &serde_json::to_value(RpcNotification::new("initialized", Some(json!({}))))?,
    )
    .await
}

async fn drive_agent_run(
    runner: &mut PluginRunner,
    config: &RunConfig<'_>,
    iteration: u32,
) -> Result<BenchSample> {
    let req = RpcRequest::new(
        2,
        METHOD_AGENT_RUN,
        Some(json!({
            "prompt": config.prompt,
            "model": config.model,
            "cwd": ".",
            "timeout_secs": config.timeout_ms.max(1000) / 1000,
            "env": {
                "MOCK_SCENARIO": config.scenario,
            },
        })),
    );
    let started = Instant::now();
    send_frame(runner, &serde_json::to_value(req)?).await?;

    let mut ttft: Option<Duration> = None;
    let mut notification_count = 0u32;
    let mut output_bytes = 0usize;
    let deadline = Instant::now() + Duration::from_millis(config.timeout_ms);

    loop {
        let frame = read_frame(runner, deadline).await?;
        if frame.get("id").is_some() {
            let response: RpcResponse = serde_json::from_value(frame)?;
            if response.id == Some(json!(2)) {
                if let Some(err) = response.error {
                    return Err(anyhow!(
                        "agent/run failed: {} (code {})",
                        err.message,
                        err.code
                    ));
                }
                let duration = started.elapsed();
                if let Some(result) = response.result {
                    if let Ok(parsed) = serde_json::from_value::<AgentRunResponse>(result) {
                        output_bytes = output_bytes.max(parsed.output.len());
                    }
                }
                return Ok(BenchSample {
                    iteration,
                    ttft_ms: ttft.unwrap_or(duration).as_millis() as u64,
                    duration_ms: duration.as_millis() as u64,
                    notification_count,
                    output_bytes,
                });
            }
        } else if let Some(method) = frame.get("method").and_then(Value::as_str) {
            if method == NOTIFICATION_AGENT_OUTPUT {
                if ttft.is_none() {
                    ttft = Some(started.elapsed());
                }
                notification_count += 1;
                if let Some(text) = frame
                    .get("params")
                    .and_then(|p| p.get("text"))
                    .and_then(Value::as_str)
                {
                    output_bytes += text.len();
                }
            } else if method.starts_with("agent/") {
                notification_count += 1;
            }
        }
    }
}

async fn send_frame(runner: &mut PluginRunner, value: &Value) -> Result<()> {
    let stdin = runner
        .stdin
        .as_mut()
        .ok_or_else(|| anyhow!("plugin stdin already taken"))?;
    let mut line = serde_json::to_string(value)?;
    line.push('\n');
    stdin.write_all(line.as_bytes()).await?;
    stdin.flush().await.ok();
    Ok(())
}

async fn read_frame(runner: &mut PluginRunner, deadline: Instant) -> Result<Value> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(anyhow!("bench iteration timed out"));
        }
        let mut line = String::new();
        match timeout(remaining, runner.stdout.read_line(&mut line)).await {
            Err(_) => return Err(anyhow!("bench iteration timed out")),
            Ok(Ok(0)) => return Err(anyhow!("plugin stdout closed")),
            Ok(Ok(_)) => {}
            Ok(Err(e)) => return Err(e.into()),
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        return serde_json::from_str(trimmed)
            .with_context(|| format!("invalid JSON-RPC frame: {trimmed}"));
    }
}

async fn read_until_response(
    runner: &mut PluginRunner,
    id: u64,
    deadline: Duration,
) -> Result<RpcResponse> {
    let end = Instant::now() + deadline;
    loop {
        let value = read_frame(runner, end).await?;
        if value.get("id") == Some(&json!(id)) {
            let response: RpcResponse = serde_json::from_value(value)?;
            if let Some(err) = response.error.as_ref() {
                return Err(anyhow!(
                    "initialize failed: {} (code {})",
                    err.message,
                    err.code
                ));
            }
            return Ok(response);
        }
    }
}

fn print_case_summary(case: &BenchCaseReport) {
    let Some(stats) = &case.summary else {
        println!("  summary unavailable: no successful measured iterations");
        return;
    };
    println!(
        "  summary ttft median {:>5}ms p95 {:>5}ms | total median {:>5}ms p95 {:>5}ms | throughput {:>7.1} B/s",
        stats.ttft_median_ms,
        stats.ttft_p95_ms,
        stats.duration_median_ms,
        stats.duration_p95_ms,
        stats.throughput_bps
    );
}

fn print_suite_summary(report: &BenchmarkSuiteReport) {
    println!();
    println!("{}", "==> suite summary".cyan().bold());
    for case in &report.cases {
        if let Some(stats) = &case.summary {
            println!(
                "  [{:>4}] {:<28} {:<18} {:<24} samples {:>2}/{:<2} ttft_p95 {:>5}ms dur_p95 {:>5}ms",
                case.status().to_uppercase(),
                case.plugin_name,
                case.scenario,
                case.model,
                stats.sample_count,
                case.iterations_requested,
                stats.ttft_p95_ms,
                stats.duration_p95_ms
            );
        } else {
            println!(
                "  [{:>4}] {:<28} {:<18} {:<24} samples  0/{:<2}",
                case.status().to_uppercase(),
                case.plugin_name,
                case.scenario,
                case.model,
                case.iterations_requested
            );
        }
    }
}

fn summary_stats(samples: &[BenchSample]) -> Option<SummaryStats> {
    if samples.is_empty() {
        return None;
    }

    let mut ttfts: Vec<u64> = samples.iter().map(|s| s.ttft_ms).collect();
    let mut durations: Vec<u64> = samples.iter().map(|s| s.duration_ms).collect();
    ttfts.sort_unstable();
    durations.sort_unstable();

    let avg_u64 = |v: &[u64]| v.iter().sum::<u64>() as f64 / v.len() as f64;
    let total_bytes: usize = samples.iter().map(|s| s.output_bytes).sum();
    let total_ms: u64 = samples.iter().map(|s| s.duration_ms).sum();
    let total_notifications: u32 = samples.iter().map(|s| s.notification_count).sum();
    let throughput_bps = if total_ms > 0 {
        (total_bytes as f64 * 1000.0) / total_ms as f64
    } else {
        0.0
    };

    Some(SummaryStats {
        sample_count: samples.len(),
        ttft_min_ms: ttfts.first().copied().unwrap_or(0),
        ttft_median_ms: percentile_nearest_rank(&ttfts, 50.0),
        ttft_p95_ms: percentile_nearest_rank(&ttfts, 95.0),
        ttft_avg_ms: avg_u64(&ttfts),
        ttft_max_ms: ttfts.last().copied().unwrap_or(0),
        duration_min_ms: durations.first().copied().unwrap_or(0),
        duration_median_ms: percentile_nearest_rank(&durations, 50.0),
        duration_p95_ms: percentile_nearest_rank(&durations, 95.0),
        duration_avg_ms: avg_u64(&durations),
        duration_max_ms: durations.last().copied().unwrap_or(0),
        notification_avg: total_notifications as f64 / samples.len() as f64,
        output_bytes_total: total_bytes,
        throughput_bps,
    })
}

fn percentile_nearest_rank(sorted: &[u64], percentile: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let percentile = percentile.clamp(0.0, 100.0);
    let rank = ((percentile / 100.0) * sorted.len() as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[idx]
}

fn maybe_write_json(report: &BenchmarkSuiteReport, path: Option<&Path>) -> Result<()> {
    let Some(path) = path else {
        return Ok(());
    };
    let payload = serde_json::to_vec_pretty(report)?;
    std::fs::write(path, payload).with_context(|| format!("write {}", path.display()))?;
    println!("report written to {}", path.display());
    Ok(())
}

fn maybe_write_csv(report: &BenchmarkSuiteReport, path: Option<&Path>) -> Result<()> {
    let Some(path) = path else {
        return Ok(());
    };
    let mut out = String::from(
        "plugin,plugin_path,scenario,model,status,sample_count,error_count,ttft_min_ms,ttft_median_ms,ttft_p95_ms,ttft_avg_ms,ttft_max_ms,duration_min_ms,duration_median_ms,duration_p95_ms,duration_avg_ms,duration_max_ms,notification_avg,output_bytes_total,throughput_bps\n",
    );
    for case in &report.cases {
        let stats = case.summary.as_ref();
        let fields = vec![
            csv_field(&case.plugin_name),
            csv_field(&case.plugin_path.display().to_string()),
            csv_field(&case.scenario),
            csv_field(&case.model),
            csv_field(case.status()),
            case.samples.len().to_string(),
            case.errors.len().to_string(),
            stats.map(|s| s.ttft_min_ms.to_string()).unwrap_or_default(),
            stats
                .map(|s| s.ttft_median_ms.to_string())
                .unwrap_or_default(),
            stats.map(|s| s.ttft_p95_ms.to_string()).unwrap_or_default(),
            stats
                .map(|s| format!("{:.3}", s.ttft_avg_ms))
                .unwrap_or_default(),
            stats.map(|s| s.ttft_max_ms.to_string()).unwrap_or_default(),
            stats
                .map(|s| s.duration_min_ms.to_string())
                .unwrap_or_default(),
            stats
                .map(|s| s.duration_median_ms.to_string())
                .unwrap_or_default(),
            stats
                .map(|s| s.duration_p95_ms.to_string())
                .unwrap_or_default(),
            stats
                .map(|s| format!("{:.3}", s.duration_avg_ms))
                .unwrap_or_default(),
            stats
                .map(|s| s.duration_max_ms.to_string())
                .unwrap_or_default(),
            stats
                .map(|s| format!("{:.3}", s.notification_avg))
                .unwrap_or_default(),
            stats
                .map(|s| s.output_bytes_total.to_string())
                .unwrap_or_default(),
            stats
                .map(|s| format!("{:.3}", s.throughput_bps))
                .unwrap_or_default(),
        ];
        out.push_str(&fields.join(","));
        out.push('\n');
    }
    std::fs::write(path, out).with_context(|| format!("write {}", path.display()))?;
    println!("csv written to {}", path.display());
    Ok(())
}

fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn plugin_label(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("plugin")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(iteration: u32, ttft_ms: u64, duration_ms: u64, output_bytes: usize) -> BenchSample {
        BenchSample {
            iteration,
            ttft_ms,
            duration_ms,
            notification_count: 1,
            output_bytes,
        }
    }

    #[test]
    fn summary_stats_returns_none_for_empty_samples() {
        assert!(summary_stats(&[]).is_none());
    }

    #[test]
    fn summary_stats_computes_percentiles_averages_and_throughput() {
        let stats = summary_stats(&[
            sample(1, 30, 300, 300),
            sample(2, 10, 100, 100),
            sample(3, 20, 200, 200),
        ])
        .expect("non-empty samples should produce stats");

        assert_eq!(stats.sample_count, 3);
        assert_eq!(stats.ttft_min_ms, 10);
        assert_eq!(stats.ttft_median_ms, 20);
        assert_eq!(stats.ttft_p95_ms, 30);
        assert!((stats.ttft_avg_ms - 20.0).abs() < f64::EPSILON);
        assert_eq!(stats.ttft_max_ms, 30);
        assert_eq!(stats.duration_min_ms, 100);
        assert_eq!(stats.duration_median_ms, 200);
        assert_eq!(stats.duration_p95_ms, 300);
        assert!((stats.duration_avg_ms - 200.0).abs() < f64::EPSILON);
        assert_eq!(stats.duration_max_ms, 300);
        assert_eq!(stats.output_bytes_total, 600);
        assert!((stats.throughput_bps - 1000.0).abs() < f64::EPSILON);
    }

    #[test]
    fn summary_stats_handles_zero_total_duration() {
        let stats = summary_stats(&[sample(1, 1, 0, 100)]).expect("sample should produce stats");
        assert_eq!(stats.throughput_bps, 0.0);
    }

    #[test]
    fn suite_presets_are_stable() {
        assert_eq!(BenchSuite::Smoke.scenarios(), vec!["streaming-short"]);
        assert_eq!(
            BenchSuite::Full.scenarios(),
            vec![
                "streaming-short",
                "streaming-medium",
                "streaming-long",
                "tool-call-single",
                "tool-call-parallel",
                "error-recovery"
            ]
        );
    }

    #[test]
    fn scenario_resolution_preserves_legacy_default() {
        let cli = Cli {
            plugin: vec![PathBuf::from("plugin")],
            suite: None,
            scenarios: vec![],
            models: vec!["model".to_string()],
            iterations: 1,
            warmup_iterations: 0,
            timeout_ms: 1000,
            prompt: "prompt".to_string(),
            report_json: None,
            report_csv: None,
            fail_fast: false,
        };
        assert_eq!(resolve_scenarios(&cli), vec!["streaming-medium"]);
    }

    #[test]
    fn csv_field_quotes_delimiters_and_quotes() {
        assert_eq!(csv_field("simple"), "simple");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("a\"b"), "\"a\"\"b\"");
    }
}
