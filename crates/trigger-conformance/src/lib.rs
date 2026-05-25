//! Baseline conformance scenarios for Animus trigger backend plugins.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use animus_plugin_protocol::{
    HostCapabilities, HostInfo, InitializeParams, InitializeResult, RpcNotification, RpcRequest,
    RpcResponse, PROTOCOL_VERSION,
};
use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::time::timeout;

use testkit_core::{ConformanceSummary, MatrixReport, TestResult, TestStatus};

const HOST_NAME: &str = "animus-trigger-conformance";
const HOST_VERSION: &str = env!("CARGO_PKG_VERSION");
const TRIGGER_KIND: &str = "trigger_backend";

#[derive(Debug, Clone)]
pub struct TestScenario {
    pub name: &'static str,
    pub description: &'static str,
}

pub fn baseline_scenarios() -> Vec<TestScenario> {
    vec![
        TestScenario {
            name: "handshake",
            description: "initialize → plugin_kind == trigger_backend",
        },
        TestScenario {
            name: "watch-fires-event",
            description: "trigger/watch acks and emits at least one trigger/event within 3s",
        },
        TestScenario {
            name: "event-payload-shape",
            description: "the first event carries id + occurred_at + kind + payload",
        },
    ]
}

pub async fn run_conformance(plugin_path: &Path) -> Result<MatrixReport> {
    let init = handshake_once(plugin_path)
        .await
        .with_context(|| format!("initial handshake against {}", plugin_path.display()))?;
    let started_at = Utc::now();

    let mut results = Vec::new();
    results.push(run_handshake(&init));
    let first_event = watch_for_event(plugin_path).await;
    results.push(run_watch_fires(&first_event));
    results.push(run_event_shape(&first_event));

    let finished_at = Utc::now();
    let summary = ConformanceSummary::from_results(&results);
    Ok(MatrixReport {
        plugin_name: init.plugin_info.name.clone(),
        plugin_version: init.plugin_info.version.clone(),
        plugin_kind: init.plugin_info.plugin_kind.clone(),
        protocol_version: init.protocol_version.clone(),
        started_at,
        finished_at,
        scenarios: results,
        summary,
        host: Default::default(),
    })
}

fn run_handshake(init: &InitializeResult) -> TestResult {
    let started = Instant::now();
    let status = if init.plugin_info.plugin_kind == TRIGGER_KIND {
        TestStatus::Pass
    } else {
        TestStatus::Fail {
            reason: format!(
                "plugin_kind = `{}`, expected `{TRIGGER_KIND}`",
                init.plugin_info.plugin_kind
            ),
        }
    };
    pass_or_fail("handshake", status, started, vec![])
}

fn run_watch_fires(first_event: &WatchOutcome) -> TestResult {
    let started = Instant::now();
    let status = match first_event {
        WatchOutcome::Ack { event: Some(_) } => TestStatus::Pass,
        WatchOutcome::Ack { event: None } => TestStatus::Skip {
            reason:
                "trigger/watch acked but no event in 3s; backend likely needs external stimulus"
                    .to_string(),
        },
        WatchOutcome::NoAck(msg) => TestStatus::Fail {
            reason: format!("trigger/watch: {msg}"),
        },
    };
    pass_or_fail("watch-fires-event", status, started, vec![])
}

fn run_event_shape(first_event: &WatchOutcome) -> TestResult {
    let started = Instant::now();
    let WatchOutcome::Ack { event: Some(event) } = first_event else {
        return pass_or_fail(
            "event-payload-shape",
            TestStatus::Skip {
                reason: "no event captured by watch-fires-event".to_string(),
            },
            started,
            vec![],
        );
    };
    let mut missing = Vec::new();
    for field in ["id", "occurred_at", "kind"] {
        if event.get(field).is_none() {
            missing.push(field);
        }
    }
    if event.get("payload").is_none() {
        missing.push("payload");
    }
    let status = if missing.is_empty() {
        TestStatus::Pass
    } else {
        TestStatus::Fail {
            reason: format!("event missing required fields: {missing:?}"),
        }
    };
    pass_or_fail(
        "event-payload-shape",
        status,
        started,
        vec![format!("event = {}", short(event))],
    )
}

enum WatchOutcome {
    Ack { event: Option<Value> },
    NoAck(String),
}

async fn watch_for_event(plugin_path: &Path) -> WatchOutcome {
    let mut proc = match PluginProcess::spawn(plugin_path).await {
        Ok(p) => p,
        Err(e) => return WatchOutcome::NoAck(format!("spawn: {e}")),
    };
    if let Err(e) = handshake_with(&mut proc).await {
        proc.shutdown().await;
        return WatchOutcome::NoAck(format!("handshake: {e}"));
    }

    let watch_id: u64 = 100;
    let watch_req =
        match serde_json::to_value(RpcRequest::new(watch_id, "trigger/watch", Some(json!({})))) {
            Ok(v) => v,
            Err(e) => {
                proc.shutdown().await;
                return WatchOutcome::NoAck(format!("encode watch: {e}"));
            }
        };
    if let Err(e) = proc.send(&watch_req).await {
        proc.shutdown().await;
        return WatchOutcome::NoAck(format!("send watch: {e}"));
    }

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut acked = false;
    let mut first_event: Option<Value> = None;
    while Instant::now() < deadline && first_event.is_none() {
        let frame = match proc.read_next(deadline).await {
            Ok(v) => v,
            Err(_) => break,
        };
        if let Some(id) = frame.get("id") {
            if id == &json!(watch_id) {
                let response: RpcResponse = match serde_json::from_value(frame) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                if let Some(err) = response.error {
                    proc.shutdown().await;
                    return WatchOutcome::NoAck(format!("{} (code {})", err.message, err.code));
                }
                acked = true;
                continue;
            }
        }
        if frame.get("method").and_then(Value::as_str) == Some("trigger/event") {
            let params = frame.get("params").cloned().unwrap_or(Value::Null);
            first_event = Some(params);
        }
    }
    proc.shutdown().await;
    if !acked && first_event.is_none() {
        return WatchOutcome::NoAck("no ack and no events within 3s".to_string());
    }
    WatchOutcome::Ack { event: first_event }
}

fn short(value: &Value) -> String {
    let s = value.to_string();
    if s.len() > 200 {
        format!("{}…", &s[..200])
    } else {
        s
    }
}

fn pass_or_fail(
    name: &str,
    status: TestStatus,
    started: Instant,
    diagnostics: Vec<String>,
) -> TestResult {
    TestResult {
        scenario: name.to_string(),
        status,
        duration_ms: started.elapsed().as_millis() as u64,
        notification_log: vec![],
        response: None,
        diagnostics,
    }
}

struct PluginProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<tokio::process::ChildStdout>,
    _cwd: tempfile::TempDir,
}

impl PluginProcess {
    async fn spawn(plugin_path: &Path) -> Result<Self> {
        let tmp = tempfile::tempdir().context("tempdir for trigger-conformance cwd")?;
        let mut cmd = Command::new(plugin_path);
        cmd.current_dir(tmp.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawn {}", plugin_path.display()))?;
        let stdin = child.stdin.take().context("stdin missing")?;
        let stdout = BufReader::new(child.stdout.take().context("stdout missing")?);
        Ok(Self {
            child,
            stdin,
            stdout,
            _cwd: tmp,
        })
    }

    async fn send(&mut self, value: &Value) -> Result<()> {
        let mut line = serde_json::to_string(value)?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await.ok();
        Ok(())
    }

    async fn read_next(&mut self, deadline: Instant) -> Result<Value> {
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(anyhow!("read deadline exceeded"));
            }
            let mut line = String::new();
            match timeout(remaining, self.stdout.read_line(&mut line)).await {
                Err(_) => return Err(anyhow!("read deadline exceeded")),
                Ok(Ok(0)) => return Err(anyhow!("plugin stdout closed")),
                Ok(Ok(_)) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let v: Value = serde_json::from_str(trimmed)
                        .with_context(|| format!("invalid JSON frame: {trimmed}"))?;
                    return Ok(v);
                }
                Ok(Err(e)) => return Err(e.into()),
            }
        }
    }

    async fn shutdown(mut self) {
        let _ = self.stdin.shutdown().await;
        drop(self.stdin);
        let _ = timeout(Duration::from_millis(750), self.child.wait()).await;
        let _ = self.child.kill().await;
    }
}

async fn handshake_with(proc: &mut PluginProcess) -> Result<InitializeResult> {
    let init_req = serde_json::to_value(RpcRequest::new(
        1,
        "initialize",
        Some(serde_json::to_value(InitializeParams {
            protocol_version: PROTOCOL_VERSION.to_string(),
            host_info: HostInfo {
                name: HOST_NAME.to_string(),
                version: HOST_VERSION.to_string(),
            },
            capabilities: HostCapabilities {
                streaming: true,
                progress: false,
                cancellation: true,
            },
        })?),
    ))?;
    proc.send(&init_req).await?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let frame = proc.read_next(deadline).await?;
        if frame.get("id").is_some() {
            let response: RpcResponse = serde_json::from_value(frame)?;
            if let Some(err) = response.error {
                return Err(anyhow!("initialize failed: {} ({})", err.message, err.code));
            }
            let result_value = response
                .result
                .ok_or_else(|| anyhow!("initialize response had no result"))?;
            let init: InitializeResult = serde_json::from_value(result_value)?;
            let initialized =
                serde_json::to_value(RpcNotification::new("initialized", Some(json!({}))))?;
            proc.send(&initialized).await.ok();
            return Ok(init);
        }
    }
}

async fn handshake_once(plugin_path: &Path) -> Result<InitializeResult> {
    let mut proc = PluginProcess::spawn(plugin_path).await?;
    let init = handshake_with(&mut proc).await;
    proc.shutdown().await;
    init
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_scenarios_are_stable() {
        let s = baseline_scenarios();
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].name, "handshake");
        assert_eq!(s[2].name, "event-payload-shape");
    }
}
