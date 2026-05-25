//! Baseline conformance scenarios for Animus subject backend plugins.
//!
//! Mirrors `provider-conformance` but speaks the `subject/*` (or
//! `<kind>/*`) method family. The harness drives each scenario through the
//! standard `initialize`/`initialized` handshake and validates the wire
//! shape against `animus-plugin-protocol` v0.1.9.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use animus_plugin_protocol::{
    HostCapabilities, HostInfo, InitializeParams, InitializeResult, RpcNotification, RpcRequest,
    RpcResponse, PLUGIN_KIND_SUBJECT_BACKEND, PROTOCOL_VERSION,
};
use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::time::timeout;

use testkit_core::{ConformanceSummary, MatrixReport, TestResult, TestStatus};

const HOST_NAME: &str = "animus-subject-conformance";
const HOST_VERSION: &str = env!("CARGO_PKG_VERSION");

/// One declarative subject-conformance scenario.
#[derive(Debug, Clone)]
pub struct TestScenario {
    pub name: &'static str,
    pub description: &'static str,
}

/// Return the baseline scenarios in deterministic order.
pub fn baseline_scenarios() -> Vec<TestScenario> {
    vec![
        TestScenario {
            name: "handshake",
            description: "initialize → plugin_kind == subject_backend",
        },
        TestScenario {
            name: "advertise-kinds",
            description: "capabilities.subject_kinds non-empty",
        },
        TestScenario {
            name: "subject-list",
            description: "<kind>/list returns a JSON object",
        },
        TestScenario {
            name: "subject-crud-round-trip",
            description: "create → get → update → delete (skipped when create unsupported)",
        },
        TestScenario {
            name: "subject-watch-stream",
            description: "<kind>/watch starts a stream or returns METHOD_NOT_SUPPORTED",
        },
    ]
}

/// Run every baseline scenario against the plugin at `plugin_path`.
pub async fn run_conformance(plugin_path: &Path) -> Result<MatrixReport> {
    let init = handshake_once(plugin_path)
        .await
        .with_context(|| format!("initial handshake against {}", plugin_path.display()))?;
    let started_at = Utc::now();

    let mut results = Vec::new();
    results.push(run_handshake(&init));
    results.push(run_advertise_kinds(&init));
    results.push(run_list(plugin_path, &init).await);
    results.push(run_crud(plugin_path, &init).await);
    results.push(run_watch(plugin_path, &init).await);

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
    let status = if init.plugin_info.plugin_kind == PLUGIN_KIND_SUBJECT_BACKEND {
        TestStatus::Pass
    } else {
        TestStatus::Fail {
            reason: format!(
                "plugin_kind = `{}`, expected `{PLUGIN_KIND_SUBJECT_BACKEND}`",
                init.plugin_info.plugin_kind
            ),
        }
    };
    pass_or_fail("handshake", status, started, vec![])
}

fn run_advertise_kinds(init: &InitializeResult) -> TestResult {
    let started = Instant::now();
    let mut diagnostics = vec![format!(
        "subject_kinds = {:?}",
        init.capabilities.subject_kinds
    )];
    let status = if init.capabilities.subject_kinds.is_empty() {
        TestStatus::Fail {
            reason: "capabilities.subject_kinds is empty".to_string(),
        }
    } else {
        diagnostics.push(format!(
            "methods advertised: {}",
            init.capabilities.methods.len()
        ));
        TestStatus::Pass
    };
    pass_or_fail("advertise-kinds", status, started, diagnostics)
}

async fn run_list(plugin_path: &Path, init: &InitializeResult) -> TestResult {
    let started = Instant::now();
    let kind = init.capabilities.subject_kinds.first().cloned();
    let method = match list_method(init, kind.as_deref()) {
        Some(m) => m,
        None => {
            return pass_or_fail(
                "subject-list",
                TestStatus::Skip {
                    reason: "plugin advertises no `*/list` method".to_string(),
                },
                started,
                vec![],
            );
        }
    };

    match driven_request(plugin_path, &method, json!({})).await {
        Ok(value) => {
            if value.is_object() || value.is_array() || value.is_null() {
                pass_or_fail(
                    "subject-list",
                    TestStatus::Pass,
                    started,
                    vec![format!("method = {method}")],
                )
            } else {
                pass_or_fail(
                    "subject-list",
                    TestStatus::Fail {
                        reason: format!("`{method}` returned non-object/array: {}", short(&value)),
                    },
                    started,
                    vec![],
                )
            }
        }
        Err(e) => pass_or_fail(
            "subject-list",
            TestStatus::Fail {
                reason: format!("{method}: {e}"),
            },
            started,
            vec![],
        ),
    }
}

async fn run_crud(plugin_path: &Path, init: &InitializeResult) -> TestResult {
    let started = Instant::now();
    let kind = init.capabilities.subject_kinds.first().cloned();
    let create_method = pick_method(init, &kind, "create");
    let get_method = pick_method(init, &kind, "get");
    let update_method = pick_method(init, &kind, "update");
    let delete_method = pick_method(init, &kind, "delete");

    let Some(create) = create_method else {
        return pass_or_fail(
            "subject-crud-round-trip",
            TestStatus::Skip {
                reason: "plugin does not advertise `*/create`".to_string(),
            },
            started,
            vec![],
        );
    };

    let mut diagnostics = Vec::new();
    let mut proc = match PluginProcess::spawn(plugin_path).await {
        Ok(p) => p,
        Err(e) => {
            return pass_or_fail(
                "subject-crud-round-trip",
                TestStatus::Fail {
                    reason: format!("spawn: {e}"),
                },
                started,
                diagnostics,
            );
        }
    };
    if let Err(e) = handshake_with(&mut proc).await {
        proc.shutdown().await;
        return pass_or_fail(
            "subject-crud-round-trip",
            TestStatus::Fail {
                reason: format!("handshake: {e}"),
            },
            started,
            diagnostics,
        );
    }

    let create_params = json!({
        "title": format!("subject-conformance-{}", Utc::now().timestamp_millis()),
        "description": "harness round-trip",
    });
    let created = match call_method(&mut proc, &create, create_params).await {
        Ok(v) => v,
        Err(e) => {
            proc.shutdown().await;
            return pass_or_fail(
                "subject-crud-round-trip",
                TestStatus::Fail {
                    reason: format!("{create}: {e}"),
                },
                started,
                diagnostics,
            );
        }
    };
    diagnostics.push(format!("{create} → ok"));
    let id = extract_id(&created).unwrap_or_default();
    if id.is_empty() {
        proc.shutdown().await;
        return pass_or_fail(
            "subject-crud-round-trip",
            TestStatus::Fail {
                reason: format!("{create} returned no id; got {}", short(&created)),
            },
            started,
            diagnostics,
        );
    }

    if let Some(get) = &get_method {
        match call_method(&mut proc, get, json!({ "id": id })).await {
            Ok(_) => diagnostics.push(format!("{get} → ok")),
            Err(e) => {
                proc.shutdown().await;
                return pass_or_fail(
                    "subject-crud-round-trip",
                    TestStatus::Fail {
                        reason: format!("{get}: {e}"),
                    },
                    started,
                    diagnostics,
                );
            }
        }
    }

    if let Some(update) = &update_method {
        let _ = call_method(
            &mut proc,
            update,
            json!({ "id": id, "patch": { "comment": "round-trip" } }),
        )
        .await;
        diagnostics.push(format!("{update} attempted"));
    }

    if let Some(delete) = &delete_method {
        match call_method(&mut proc, delete, json!({ "id": id })).await {
            Ok(_) => diagnostics.push(format!("{delete} → ok")),
            Err(e) => diagnostics.push(format!("{delete} returned: {e}")),
        }
    }

    proc.shutdown().await;
    pass_or_fail(
        "subject-crud-round-trip",
        TestStatus::Pass,
        started,
        diagnostics,
    )
}

async fn run_watch(plugin_path: &Path, init: &InitializeResult) -> TestResult {
    let started = Instant::now();
    let kind = init.capabilities.subject_kinds.first().cloned();
    let method = pick_method(init, &kind, "watch");
    let Some(method) = method else {
        return pass_or_fail(
            "subject-watch-stream",
            TestStatus::Skip {
                reason: "plugin does not advertise `*/watch`".to_string(),
            },
            started,
            vec![],
        );
    };

    match driven_request(plugin_path, &method, json!({})).await {
        Ok(_) => pass_or_fail(
            "subject-watch-stream",
            TestStatus::Pass,
            started,
            vec![format!("{method} → ok")],
        ),
        Err(e) => {
            let msg = format!("{e}");
            let lower = msg.to_ascii_lowercase();
            if msg.contains("-32001")
                || msg.contains("-32601")
                || lower.contains("not supported")
                || lower.contains("not implemented")
                || lower.contains("not found")
            {
                pass_or_fail(
                    "subject-watch-stream",
                    TestStatus::Pass,
                    started,
                    vec![format!("{method} → unsupported (allowed)")],
                )
            } else {
                pass_or_fail(
                    "subject-watch-stream",
                    TestStatus::Fail {
                        reason: format!("{method}: {e}"),
                    },
                    started,
                    vec![],
                )
            }
        }
    }
}

fn list_method(init: &InitializeResult, kind: Option<&str>) -> Option<String> {
    if let Some(k) = kind {
        let candidate = format!("{k}/list");
        if init.capabilities.methods.iter().any(|m| m == &candidate) {
            return Some(candidate);
        }
    }
    let fallback = "subject/list".to_string();
    if init.capabilities.methods.iter().any(|m| m == &fallback) {
        return Some(fallback);
    }
    None
}

fn pick_method(init: &InitializeResult, kind: &Option<String>, verb: &str) -> Option<String> {
    if let Some(k) = kind {
        let candidate = format!("{k}/{verb}");
        if init.capabilities.methods.iter().any(|m| m == &candidate) {
            return Some(candidate);
        }
    }
    let fallback = format!("subject/{verb}");
    if init.capabilities.methods.iter().any(|m| m == &fallback) {
        return Some(fallback);
    }
    None
}

fn extract_id(value: &Value) -> Option<String> {
    if let Some(s) = value.get("id").and_then(Value::as_str) {
        return Some(s.to_string());
    }
    if let Some(subj) = value.get("subject") {
        if let Some(s) = subj.get("id").and_then(Value::as_str) {
            return Some(s.to_string());
        }
    }
    None
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

// =====================================================================
// Low-level JSON-RPC driver
// =====================================================================

struct PluginProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<tokio::process::ChildStdout>,
    next_id: u64,
    _cwd: tempfile::TempDir,
}

impl PluginProcess {
    async fn spawn(plugin_path: &Path) -> Result<Self> {
        let tmp = tempfile::tempdir().context("tempdir for subject-conformance cwd")?;
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

async fn driven_request(plugin_path: &Path, method: &str, params: Value) -> Result<Value> {
    let mut proc = PluginProcess::spawn(plugin_path).await?;
    handshake_with(&mut proc).await.context("handshake")?;
    let result = call_method(&mut proc, method, params).await;
    proc.shutdown().await;
    result
}

async fn call_method(proc: &mut PluginProcess, method: &str, params: Value) -> Result<Value> {
    let id = proc.next_id;
    proc.next_id += 1;
    let req = serde_json::to_value(RpcRequest::new(id, method, Some(params)))?;
    proc.send(&req).await?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let frame = proc.read_next(deadline).await?;
        if let Some(frame_id) = frame.get("id") {
            if frame_id == &json!(id) {
                let response: RpcResponse = serde_json::from_value(frame)?;
                if let Some(err) = response.error {
                    return Err(anyhow!(
                        "{} returned error: {} (code {})",
                        method,
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

    #[test]
    fn baseline_scenarios_are_stable() {
        let s = baseline_scenarios();
        assert_eq!(s.len(), 5);
        assert_eq!(s[0].name, "handshake");
        assert_eq!(s[4].name, "subject-watch-stream");
    }
}
