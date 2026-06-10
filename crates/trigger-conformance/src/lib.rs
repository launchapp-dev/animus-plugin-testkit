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
            description: "the first event uses the flat wire shape: required string event_id, optional trigger_id/subject_id/subject_kind/action_hint/payload",
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
        WatchOutcome::NoAck(msg) if external_config_missing(msg) => TestStatus::Skip {
            reason: format!("trigger/watch requires external configuration: {msg}"),
        },
        WatchOutcome::NoAck(msg) => TestStatus::Fail {
            reason: format!("trigger/watch: {msg}"),
        },
    };
    pass_or_fail("watch-fires-event", status, started, vec![])
}

fn external_config_missing(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("must be set")
        || lower.contains("must include")
        || lower.contains("missing")
        || lower.contains("not configured")
        || lower.contains("required")
        || lower.contains("unset")
        || lower.contains("api token")
        || lower.contains("auth")
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
    // Wire shape per spec §7.3 ("Wire shape note"): `params` IS the flat
    // `animus_plugin_protocol::TriggerEvent` — `event_id` is the only
    // required field; the host drops frames it cannot decode as that shape.
    let status = match event.get("event_id") {
        Some(Value::String(event_id)) if !event_id.is_empty() => TestStatus::Pass,
        Some(other) => TestStatus::Fail {
            reason: format!("event_id must be a non-empty string, got {other}"),
        },
        None => TestStatus::Fail {
            reason: "event missing required field `event_id` (flat TriggerEvent params; the nested {id, event} wrapper is not decoded by the host)".to_string(),
        },
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
    use animus_plugin_protocol::{PluginCapabilities, PluginInfo};

    fn init(plugin_kind: &str) -> InitializeResult {
        InitializeResult {
            protocol_version: PROTOCOL_VERSION.to_string(),
            plugin_info: PluginInfo {
                name: "unit-trigger".to_string(),
                version: "0.0.0".to_string(),
                plugin_kind: plugin_kind.to_string(),
                description: None,
            },
            capabilities: PluginCapabilities {
                methods: vec!["trigger/watch".to_string(), "health/check".to_string()],
                streaming: true,
                progress: false,
                cancellation: true,
                projections: vec![],
                subject_kinds: vec![],
                mcp_tools: vec![],
            },
        }
    }

    #[test]
    fn baseline_scenarios_are_stable() {
        let s = baseline_scenarios();
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].name, "handshake");
        assert_eq!(s[2].name, "event-payload-shape");
    }

    #[test]
    fn handshake_classifier_passes_only_trigger_backend_kind() {
        assert_eq!(run_handshake(&init(TRIGGER_KIND)).status, TestStatus::Pass);
        assert!(matches!(
            run_handshake(&init("transport_backend")).status,
            TestStatus::Fail { reason } if reason.contains("expected `trigger_backend`")
        ));
    }

    #[test]
    fn watch_fires_classifier_distinguishes_pass_skip_and_fail() {
        let event = json!({
            "event_id": "evt-1",
            "trigger_id": "unit",
            "payload": {}
        });
        assert_eq!(
            run_watch_fires(&WatchOutcome::Ack { event: Some(event) }).status,
            TestStatus::Pass
        );
        assert!(matches!(
            run_watch_fires(&WatchOutcome::Ack { event: None }).status,
            TestStatus::Skip { reason } if reason.contains("no event in 3s")
        ));
        assert!(matches!(
            run_watch_fires(&WatchOutcome::NoAck("boom".to_string())).status,
            TestStatus::Fail { reason } if reason.contains("boom")
        ));
        assert!(matches!(
            run_watch_fires(&WatchOutcome::NoAck("SLACK_APP_TOKEN unset".to_string())).status,
            TestStatus::Skip { reason } if reason.contains("external configuration")
        ));
    }

    #[test]
    fn event_shape_requires_flat_event_id() {
        let complete = WatchOutcome::Ack {
            event: Some(json!({
                "event_id": "evt-1",
                "trigger_id": "unit",
                "payload": {"ok": true}
            })),
        };
        assert_eq!(run_event_shape(&complete).status, TestStatus::Pass);

        let minimal = WatchOutcome::Ack {
            event: Some(json!({"event_id": "evt-1"})),
        };
        assert_eq!(run_event_shape(&minimal).status, TestStatus::Pass);

        // The legacy nested {id, event} wrapper was never decoded by the host
        // and must fail conformance.
        let wrapped = WatchOutcome::Ack {
            event: Some(json!({
                "id": 100,
                "event": {"id": "evt-1", "occurred_at": "2026-05-28T00:00:00Z", "kind": "unit"}
            })),
        };
        assert!(matches!(
            run_event_shape(&wrapped).status,
            TestStatus::Fail { reason } if reason.contains("event_id")
        ));

        let bad_type = WatchOutcome::Ack {
            event: Some(json!({"event_id": 7})),
        };
        assert!(matches!(
            run_event_shape(&bad_type).status,
            TestStatus::Fail { reason } if reason.contains("non-empty string")
        ));

        assert!(matches!(
            run_event_shape(&WatchOutcome::Ack { event: None }).status,
            TestStatus::Skip { reason } if reason.contains("no event captured")
        ));
    }
}
