use std::path::PathBuf;
use std::process::{ExitCode, Stdio};
use std::time::{Duration, Instant};

use animus_plugin_protocol::{
    HostCapabilities, HostInfo, InitializeParams, RpcNotification, RpcRequest, RpcResponse,
    PROTOCOL_VERSION,
};
use animus_provider_protocol::{AgentRunResponse, METHOD_AGENT_RUN, NOTIFICATION_AGENT_OUTPUT};
use anyhow::{anyhow, Context, Result};
use clap::Parser;
use colored::Colorize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::timeout;

#[derive(Parser, Debug)]
#[command(
    name = "animus-plugin-bench",
    version,
    about = "Provider plugin benchmark: TTFT, throughput, end-to-end duration."
)]
struct Cli {
    #[arg(long)]
    plugin: PathBuf,
    #[arg(long, default_value = "streaming-medium")]
    mock_scenario: String,
    #[arg(long, default_value_t = 5)]
    iterations: u32,
    #[arg(long, default_value_t = 30_000)]
    timeout_ms: u64,
    #[arg(long, default_value = "claude-sonnet-4-6")]
    model: String,
    #[arg(long, default_value = "say hi")]
    prompt: String,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{} {e}", "error:".red().bold());
            ExitCode::from(1)
        }
    }
}

struct BenchSample {
    ttft_ms: u64,
    duration_ms: u64,
    notif_count: u32,
    output_bytes: usize,
}

async fn run(cli: Cli) -> Result<()> {
    println!(
        "{} plugin={} iterations={} mock={}",
        "==> bench".cyan().bold(),
        cli.plugin.display(),
        cli.iterations,
        cli.mock_scenario
    );

    let mut samples: Vec<BenchSample> = Vec::with_capacity(cli.iterations as usize);
    for i in 0..cli.iterations {
        let sample = run_once(&cli)
            .await
            .with_context(|| format!("iteration {i}"))?;
        println!(
            "  iter {:>2}  ttft {:>5}ms  total {:>5}ms  notifs {:>4}  bytes {:>6}",
            i + 1,
            sample.ttft_ms,
            sample.duration_ms,
            sample.notif_count,
            sample.output_bytes,
        );
        samples.push(sample);
    }

    summarize(&samples);
    Ok(())
}

async fn run_once(cli: &Cli) -> Result<BenchSample> {
    let workspace_root = workspace_root();
    let mut cmd = Command::new(&cli.plugin);
    if let Some(p) = find_mock(&workspace_root, "mock-claude") {
        cmd.env("CLAUDE_BIN", p);
    }
    if let Some(p) = find_mock(&workspace_root, "mock-codex") {
        cmd.env("CODEX_BIN", p);
    }
    if let Some(p) = find_mock(&workspace_root, "mock-gemini") {
        cmd.env("GEMINI_BIN", p);
    }
    if let Some(p) = find_mock(&workspace_root, "mock-opencode") {
        cmd.env("OPENCODE_BIN", p);
    }
    cmd.env("MOCK_SCENARIO", &cli.mock_scenario);
    cmd.env("ANIMUS_TESTKIT", "1");

    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().context("spawn plugin")?;
    let mut stdin = child.stdin.take().context("stdin missing")?;
    let mut stdout = BufReader::new(child.stdout.take().context("stdout missing")?);

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
    let mut line = serde_json::to_string(&init)?;
    line.push('\n');
    stdin.write_all(line.as_bytes()).await?;

    read_until_response(&mut stdout, 1, Duration::from_secs(10)).await?;
    let notif = RpcNotification::new("initialized", Some(json!({})));
    let mut line = serde_json::to_string(&notif)?;
    line.push('\n');
    stdin.write_all(line.as_bytes()).await?;

    let req = RpcRequest::new(
        2,
        METHOD_AGENT_RUN,
        Some(json!({
            "prompt": cli.prompt,
            "model": cli.model,
            "cwd": ".",
        })),
    );
    let mut line = serde_json::to_string(&req)?;
    line.push('\n');
    let started = Instant::now();
    stdin.write_all(line.as_bytes()).await?;
    stdin.flush().await.ok();

    let mut ttft: Option<Duration> = None;
    let mut notif_count = 0u32;
    let mut output_bytes = 0usize;
    let deadline = Instant::now() + Duration::from_millis(cli.timeout_ms);

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(anyhow!("bench iteration timed out"));
        }
        let mut frame_line = String::new();
        match timeout(remaining, stdout.read_line(&mut frame_line)).await {
            Err(_) => return Err(anyhow!("bench iteration timed out")),
            Ok(Ok(0)) => return Err(anyhow!("plugin stdout closed")),
            Ok(Ok(_)) => {}
            Ok(Err(e)) => return Err(e.into()),
        }
        let trimmed = frame_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(trimmed)?;
        if value.get("id").is_some() {
            let resp: RpcResponse = serde_json::from_value(value)?;
            if resp.id == Some(json!(2)) {
                let duration = started.elapsed();
                let _ = stdin.shutdown().await;
                let _ = child.kill().await;
                if let Some(result) = resp.result {
                    if let Ok(parsed) = serde_json::from_value::<AgentRunResponse>(result) {
                        output_bytes = output_bytes.max(parsed.output.len());
                    }
                }
                return Ok(BenchSample {
                    ttft_ms: ttft.unwrap_or(duration).as_millis() as u64,
                    duration_ms: duration.as_millis() as u64,
                    notif_count,
                    output_bytes,
                });
            }
        } else if let Some(method) = value.get("method").and_then(Value::as_str) {
            if method == NOTIFICATION_AGENT_OUTPUT {
                if ttft.is_none() {
                    ttft = Some(started.elapsed());
                }
                notif_count += 1;
                if let Some(text) = value
                    .get("params")
                    .and_then(|p| p.get("text"))
                    .and_then(Value::as_str)
                {
                    output_bytes += text.len();
                }
            } else if method.starts_with("agent/") {
                notif_count += 1;
            }
        }
    }
}

async fn read_until_response(
    stdout: &mut BufReader<tokio::process::ChildStdout>,
    id: u64,
    deadline: Duration,
) -> Result<RpcResponse> {
    let end = Instant::now() + deadline;
    loop {
        let remaining = end.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(anyhow!("init timeout"));
        }
        let mut line = String::new();
        match timeout(remaining, stdout.read_line(&mut line)).await {
            Err(_) => return Err(anyhow!("init timeout")),
            Ok(Ok(0)) => return Err(anyhow!("plugin closed")),
            Ok(Ok(_)) => {}
            Ok(Err(e)) => return Err(e.into()),
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(trimmed)?;
        if value.get("id") == Some(&json!(id)) {
            return Ok(serde_json::from_value(value)?);
        }
    }
}

fn summarize(samples: &[BenchSample]) {
    if samples.is_empty() {
        return;
    }
    let mut ttfts: Vec<u64> = samples.iter().map(|s| s.ttft_ms).collect();
    let mut durs: Vec<u64> = samples.iter().map(|s| s.duration_ms).collect();
    ttfts.sort_unstable();
    durs.sort_unstable();
    let median = |v: &[u64]| v[v.len() / 2];
    let avg = |v: &[u64]| v.iter().sum::<u64>() as f64 / v.len() as f64;
    let total_bytes: usize = samples.iter().map(|s| s.output_bytes).sum();
    let total_ms: u64 = samples.iter().map(|s| s.duration_ms).sum();
    let throughput_bps = if total_ms > 0 {
        (total_bytes as f64 * 1000.0) / total_ms as f64
    } else {
        0.0
    };
    println!();
    println!("{}", "==> summary".cyan().bold());
    println!(
        "  ttft       median {:>5}ms   avg {:>7.1}ms   max {:>5}ms",
        median(&ttfts),
        avg(&ttfts),
        ttfts.last().copied().unwrap_or(0)
    );
    println!(
        "  duration   median {:>5}ms   avg {:>7.1}ms   max {:>5}ms",
        median(&durs),
        avg(&durs),
        durs.last().copied().unwrap_or(0)
    );
    println!(
        "  throughput {:>5.1} bytes/sec (across all iterations)",
        throughput_bps
    );
}

fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .unwrap_or(manifest)
}

fn find_mock(root: &std::path::Path, name: &str) -> Option<PathBuf> {
    let candidates = [
        root.join("target/release").join(name),
        root.join("target/debug").join(name),
    ];
    candidates.into_iter().find(|p| p.is_file())
}
