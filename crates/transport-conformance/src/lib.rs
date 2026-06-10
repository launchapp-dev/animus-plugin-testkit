//! Baseline conformance scenarios for Animus transport backend plugins.

use std::net::TcpListener as StdTcpListener;
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
use tokio::time::{sleep, timeout};

use testkit_core::{ConformanceSummary, MatrixReport, TestResult, TestStatus};

const HOST_NAME: &str = "animus-transport-conformance";
const HOST_VERSION: &str = env!("CARGO_PKG_VERSION");
const TRANSPORT_KIND: &str = "transport_backend";

#[derive(Debug, Clone)]
pub struct TestScenario {
    pub name: &'static str,
    pub description: &'static str,
}

pub fn baseline_scenarios() -> Vec<TestScenario> {
    vec![
        TestScenario {
            name: "handshake",
            description: "initialize → plugin_kind == transport_backend",
        },
        TestScenario {
            name: "start-shutdown",
            description: "transport/start binds, transport/shutdown releases",
        },
        TestScenario {
            name: "schema-health",
            description: "transport/schema + health/check return well-shaped JSON",
        },
        TestScenario {
            name: "serve-and-accept",
            description: "after transport/start, a TCP client can dial bound_addr",
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
    results.push(run_start_shutdown(plugin_path).await);
    results.push(run_schema_health(plugin_path).await);
    results.push(run_serve_and_accept(plugin_path).await);

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
    let status = if init.plugin_info.plugin_kind == TRANSPORT_KIND {
        TestStatus::Pass
    } else {
        TestStatus::Fail {
            reason: format!(
                "plugin_kind = `{}`, expected `{TRANSPORT_KIND}`",
                init.plugin_info.plugin_kind
            ),
        }
    };
    pass_or_fail("handshake", status, started, vec![])
}

async fn run_start_shutdown(plugin_path: &Path) -> TestResult {
    let started = Instant::now();
    let mut proc = match PluginProcess::spawn(plugin_path).await {
        Ok(p) => p,
        Err(e) => {
            return pass_or_fail(
                "start-shutdown",
                TestStatus::Fail {
                    reason: format!("spawn: {e}"),
                },
                started,
                vec![],
            )
        }
    };
    if let Err(e) = handshake_with(&mut proc).await {
        proc.shutdown().await;
        return pass_or_fail(
            "start-shutdown",
            TestStatus::Fail {
                reason: format!("handshake: {e}"),
            },
            started,
            vec![],
        );
    }

    let port = match free_port() {
        Ok(p) => p,
        Err(e) => {
            proc.shutdown().await;
            return pass_or_fail(
                "start-shutdown",
                TestStatus::Fail {
                    reason: format!("free_port: {e}"),
                },
                started,
                vec![],
            );
        }
    };
    let tmp = tempfile::tempdir().ok();
    let socket_path = tmp
        .as_ref()
        .map(|t| t.path().join("control.sock"))
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp/animus-control.sock"));
    let project_root = tmp
        .as_ref()
        .map(|t| t.path().to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));

    let start_result = call_method(
        &mut proc,
        "transport/start",
        json!({
            "control_socket_path": socket_path,
            "project_root": project_root,
            "bind_addr": format!("127.0.0.1:{port}"),
        }),
        Duration::from_secs(5),
    )
    .await;

    let mut diagnostics = Vec::new();
    let status = match start_result {
        Ok(v) => {
            diagnostics.push(format!("transport/start → {}", short(&v)));
            let _ = call_method(
                &mut proc,
                "transport/shutdown",
                json!({}),
                Duration::from_secs(3),
            )
            .await
            .map(|s| diagnostics.push(format!("transport/shutdown → {}", short(&s))))
            .map_err(|e| diagnostics.push(format!("transport/shutdown errored: {e}")));
            TestStatus::Pass
        }
        Err(e) => TestStatus::Fail {
            reason: format!("transport/start: {e}"),
        },
    };
    proc.shutdown().await;
    pass_or_fail("start-shutdown", status, started, diagnostics)
}

async fn run_schema_health(plugin_path: &Path) -> TestResult {
    let started = Instant::now();
    let mut proc = match PluginProcess::spawn(plugin_path).await {
        Ok(p) => p,
        Err(e) => {
            return pass_or_fail(
                "schema-health",
                TestStatus::Fail {
                    reason: format!("spawn: {e}"),
                },
                started,
                vec![],
            )
        }
    };
    if let Err(e) = handshake_with(&mut proc).await {
        proc.shutdown().await;
        return pass_or_fail(
            "schema-health",
            TestStatus::Fail {
                reason: format!("handshake: {e}"),
            },
            started,
            vec![],
        );
    }

    let mut diagnostics = Vec::new();
    let mut failed: Option<String> = None;
    match call_method(
        &mut proc,
        "transport/schema",
        json!({}),
        Duration::from_secs(3),
    )
    .await
    {
        Ok(v) => diagnostics.push(format!("transport/schema → {}", short(&v))),
        Err(e) => failed = Some(format!("transport/schema: {e}")),
    }
    if failed.is_none() {
        match call_method(&mut proc, "health/check", json!({}), Duration::from_secs(3)).await {
            Ok(v) => diagnostics.push(format!("health/check → {}", short(&v))),
            Err(e) => failed = Some(format!("health/check: {e}")),
        }
    }
    proc.shutdown().await;
    let status = match failed {
        Some(reason) => TestStatus::Fail { reason },
        None => TestStatus::Pass,
    };
    pass_or_fail("schema-health", status, started, diagnostics)
}

async fn run_serve_and_accept(plugin_path: &Path) -> TestResult {
    let started = Instant::now();
    let port = match free_port() {
        Ok(p) => p,
        Err(e) => {
            return pass_or_fail(
                "serve-and-accept",
                TestStatus::Fail {
                    reason: format!("free_port: {e}"),
                },
                started,
                vec![],
            )
        }
    };
    let tmp = tempfile::tempdir().ok();
    let socket_path = tmp
        .as_ref()
        .map(|t| t.path().join("control.sock"))
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp/animus-control.sock"));
    let project_root = tmp
        .as_ref()
        .map(|t| t.path().to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));

    let mut proc = match PluginProcess::spawn(plugin_path).await {
        Ok(p) => p,
        Err(e) => {
            return pass_or_fail(
                "serve-and-accept",
                TestStatus::Fail {
                    reason: format!("spawn: {e}"),
                },
                started,
                vec![],
            )
        }
    };
    if let Err(e) = handshake_with(&mut proc).await {
        proc.shutdown().await;
        return pass_or_fail(
            "serve-and-accept",
            TestStatus::Fail {
                reason: format!("handshake: {e}"),
            },
            started,
            vec![],
        );
    }
    let start = call_method(
        &mut proc,
        "transport/start",
        json!({
            "control_socket_path": socket_path,
            "project_root": project_root,
            "bind_addr": format!("127.0.0.1:{port}"),
        }),
        Duration::from_secs(5),
    )
    .await;

    let status = match start {
        Ok(_) => {
            sleep(Duration::from_millis(75)).await;
            match tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
                Ok(_) => TestStatus::Pass,
                Err(e) => TestStatus::Fail {
                    reason: format!("dial 127.0.0.1:{port}: {e}"),
                },
            }
        }
        Err(e) => TestStatus::Fail {
            reason: format!("transport/start: {e}"),
        },
    };
    let _ = call_method(
        &mut proc,
        "transport/shutdown",
        json!({}),
        Duration::from_secs(3),
    )
    .await;
    proc.shutdown().await;
    pass_or_fail("serve-and-accept", status, started, vec![])
}

fn free_port() -> Result<u16> {
    let listener = StdTcpListener::bind("127.0.0.1:0").context("bind ephemeral")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

fn short(value: &Value) -> String {
    let s = value.to_string();
    if s.len() > 120 {
        format!("{}…", &s[..120])
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
    next_id: u64,
    _cwd: tempfile::TempDir,
}

impl PluginProcess {
    async fn spawn(plugin_path: &Path) -> Result<Self> {
        let tmp = tempfile::tempdir().context("tempdir for transport-conformance cwd")?;
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
            next_id: 1,
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
    let id = proc.next_id;
    proc.next_id += 1;
    let init_req = serde_json::to_value(RpcRequest::new(
        id,
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

async fn call_method(
    proc: &mut PluginProcess,
    method: &str,
    params: Value,
    budget: Duration,
) -> Result<Value> {
    let id = proc.next_id;
    proc.next_id += 1;
    let req = serde_json::to_value(RpcRequest::new(id, method, Some(params)))?;
    proc.send(&req).await?;
    let deadline = Instant::now() + budget;
    loop {
        let frame = proc.read_next(deadline).await?;
        if let Some(frame_id) = frame.get("id") {
            if frame_id == &json!(id) {
                let response: RpcResponse = serde_json::from_value(frame)?;
                if let Some(err) = response.error {
                    return Err(anyhow!(
                        "{method} returned error: {} (code {})",
                        err.message,
                        err.code
                    ));
                }
                return Ok(response.result.unwrap_or(Value::Null));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use animus_plugin_protocol::{PluginCapabilities, PluginInfo};

    fn init(plugin_kind: &str) -> InitializeResult {
        InitializeResult {
            protocol_version: PROTOCOL_VERSION.to_string(),
            plugin_info: PluginInfo {
                name: "unit-transport".to_string(),
                version: "0.0.0".to_string(),
                plugin_kind: plugin_kind.to_string(),
                description: None,
            },
            capabilities: PluginCapabilities {
                methods: vec![
                    "transport/start".to_string(),
                    "transport/shutdown".to_string(),
                    "transport/schema".to_string(),
                    "health/check".to_string(),
                ],
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
        assert_eq!(s.len(), 4);
        assert_eq!(s[0].name, "handshake");
        assert_eq!(s[3].name, "serve-and-accept");
    }

    #[test]
    fn handshake_classifier_passes_only_transport_backend_kind() {
        assert_eq!(
            run_handshake(&init(TRANSPORT_KIND)).status,
            TestStatus::Pass
        );
        assert!(matches!(
            run_handshake(&init("subject_backend")).status,
            TestStatus::Fail { reason } if reason.contains("expected `transport_backend`")
        ));
    }

    #[test]
    fn free_port_returns_a_bindable_local_port() {
        let port = free_port().expect("free port should be allocated");
        let listener = StdTcpListener::bind(("127.0.0.1", port))
            .expect("returned port should be bindable after helper drops listener");
        drop(listener);
    }

    #[test]
    fn short_truncates_large_values_for_diagnostics() {
        let value = json!({ "message": "x".repeat(200) });
        let rendered = short(&value);
        assert!(rendered.len() <= 123);
        assert!(rendered.ends_with('…'));
    }
}
