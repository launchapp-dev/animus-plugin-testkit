//! Baseline conformance scenarios for Animus log storage backend plugins.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use animus_log_storage_protocol::{
    LogEntry, LogLevel, LogQuery, LogQueryResult, LogSource, LogStorageSchema,
    METHOD_LOG_STORAGE_QUERY, METHOD_LOG_STORAGE_SCHEMA, METHOD_LOG_STORAGE_STORE,
    METHOD_LOG_STORAGE_TAIL, NOTIFICATION_LOG_STORAGE_EVENT, PLUGIN_KIND_LOG_STORAGE_BACKEND,
};
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

const HOST_NAME: &str = "animus-log-storage-conformance";
const HOST_VERSION: &str = env!("CARGO_PKG_VERSION");
const SOURCE_NAME: &str = "log-storage-conformance";

#[derive(Debug, Clone)]
pub struct TestScenario {
    pub name: &'static str,
    pub description: &'static str,
}

pub fn baseline_scenarios() -> Vec<TestScenario> {
    vec![
        TestScenario {
            name: "handshake",
            description: "initialize → plugin_kind == log_storage_backend",
        },
        TestScenario {
            name: "schema-health",
            description: "log_storage/schema + health/check return well-shaped JSON",
        },
        TestScenario {
            name: "store-query-round-trip",
            description: "log_storage/store persists entries readable by log_storage/query",
        },
        TestScenario {
            name: "tail-replay",
            description: "log_storage/tail with follow=false replays matching stored entries",
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
    results.push(run_schema_health(plugin_path).await);
    results.push(run_store_query(plugin_path).await);
    results.push(run_tail_replay(plugin_path).await);

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
    let status = if init.plugin_info.plugin_kind == PLUGIN_KIND_LOG_STORAGE_BACKEND {
        TestStatus::Pass
    } else {
        TestStatus::Fail {
            reason: format!(
                "plugin_kind = `{}`, expected `{PLUGIN_KIND_LOG_STORAGE_BACKEND}`",
                init.plugin_info.plugin_kind
            ),
        }
    };
    pass_or_fail("handshake", status, started, vec![])
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
    let status = match call_method(
        &mut proc,
        METHOD_LOG_STORAGE_SCHEMA,
        json!({}),
        Duration::from_secs(3),
    )
    .await
    {
        Ok(schema_value) => {
            match serde_json::from_value::<LogStorageSchema>(schema_value.clone()) {
                Ok(schema) => {
                    diagnostics.push(format!("log_storage/schema → {}", short(&schema_value)));
                    match call_method(&mut proc, "health/check", json!({}), Duration::from_secs(3))
                        .await
                    {
                        Ok(health) if health.get("status").is_some() => {
                            diagnostics.push(format!("health/check → {}", short(&health)));
                            let advertised_query =
                                proc.capabilities_contains(METHOD_LOG_STORAGE_QUERY);
                            let advertised_tail =
                                proc.capabilities_contains(METHOD_LOG_STORAGE_TAIL);
                            if schema.supports_query != advertised_query {
                                TestStatus::Fail {
                                    reason: format!(
                                    "schema supports_query={} but capabilities advertise query={}",
                                    schema.supports_query, advertised_query
                                ),
                                }
                            } else if schema.supports_tail != advertised_tail {
                                TestStatus::Fail {
                                    reason: format!(
                                    "schema supports_tail={} but capabilities advertise tail={}",
                                    schema.supports_tail, advertised_tail
                                ),
                                }
                            } else {
                                TestStatus::Pass
                            }
                        }
                        Ok(health) => TestStatus::Fail {
                            reason: format!("health/check missing status: {}", short(&health)),
                        },
                        Err(e) => TestStatus::Fail {
                            reason: format!("health/check: {e}"),
                        },
                    }
                }
                Err(e) => TestStatus::Fail {
                    reason: format!(
                        "log_storage/schema returned invalid schema: {e}; value={}",
                        short(&schema_value)
                    ),
                },
            }
        }
        Err(e) => TestStatus::Fail {
            reason: format!("log_storage/schema: {e}"),
        },
    };
    proc.shutdown().await;
    pass_or_fail("schema-health", status, started, diagnostics)
}

async fn run_store_query(plugin_path: &Path) -> TestResult {
    let started = Instant::now();
    let mut proc = match PluginProcess::spawn(plugin_path).await {
        Ok(p) => p,
        Err(e) => {
            return pass_or_fail(
                "store-query-round-trip",
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
            "store-query-round-trip",
            TestStatus::Fail {
                reason: format!("handshake: {e}"),
            },
            started,
            vec![],
        );
    }

    let mut diagnostics = Vec::new();
    let entry = log_entry("store-query", "store/query conformance event");
    let status = match store_entry(&mut proc, entry.clone()).await {
        Ok(stored) => {
            diagnostics.push(format!("log_storage/store → {}", short(&stored)));
            match query_entries(&mut proc, &entry.id).await {
                Ok(result) => {
                    diagnostics.push(format!(
                        "log_storage/query returned {}",
                        result.entries.len()
                    ));
                    if result
                        .entries
                        .iter()
                        .any(|candidate| candidate.id == entry.id)
                    {
                        TestStatus::Pass
                    } else {
                        TestStatus::Fail {
                            reason: format!("query result did not include stored id {}", entry.id),
                        }
                    }
                }
                Err(e) => TestStatus::Fail {
                    reason: format!("log_storage/query: {e}"),
                },
            }
        }
        Err(e) => TestStatus::Fail {
            reason: format!("log_storage/store: {e}"),
        },
    };
    proc.shutdown().await;
    pass_or_fail("store-query-round-trip", status, started, diagnostics)
}

async fn run_tail_replay(plugin_path: &Path) -> TestResult {
    let started = Instant::now();
    let mut proc = match PluginProcess::spawn(plugin_path).await {
        Ok(p) => p,
        Err(e) => {
            return pass_or_fail(
                "tail-replay",
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
            "tail-replay",
            TestStatus::Fail {
                reason: format!("handshake: {e}"),
            },
            started,
            vec![],
        );
    }
    if !proc.capabilities_contains(METHOD_LOG_STORAGE_TAIL) {
        proc.shutdown().await;
        return pass_or_fail(
            "tail-replay",
            TestStatus::Skip {
                reason: "plugin does not advertise log_storage/tail".to_string(),
            },
            started,
            vec![],
        );
    }

    let entry = log_entry("tail-replay", "tail replay conformance event");
    let mut diagnostics = Vec::new();
    let status = match store_entry(&mut proc, entry.clone()).await {
        Ok(stored) => {
            diagnostics.push(format!("log_storage/store → {}", short(&stored)));
            match tail_once(&mut proc, &entry.id).await {
                Ok(Some(event)) if event.id == entry.id => {
                    diagnostics.push(format!("log_storage/event entry id={}", event.id));
                    TestStatus::Pass
                }
                Ok(Some(event)) => TestStatus::Fail {
                    reason: format!("tail replay emitted id {}, expected {}", event.id, entry.id),
                },
                Ok(None) => TestStatus::Fail {
                    reason: format!("tail replay did not emit stored id {}", entry.id),
                },
                Err(e) => TestStatus::Fail {
                    reason: format!("log_storage/tail: {e}"),
                },
            }
        }
        Err(e) => TestStatus::Fail {
            reason: format!("log_storage/store: {e}"),
        },
    };
    proc.shutdown().await;
    pass_or_fail("tail-replay", status, started, diagnostics)
}

async fn store_entry(proc: &mut PluginProcess, entry: LogEntry) -> Result<Value> {
    call_method(
        proc,
        METHOD_LOG_STORAGE_STORE,
        json!({ "entries": [entry] }),
        Duration::from_secs(3),
    )
    .await
}

async fn query_entries(proc: &mut PluginProcess, id: &str) -> Result<LogQueryResult> {
    let query = filtered_query(id, false);
    let value = call_method(
        proc,
        METHOD_LOG_STORAGE_QUERY,
        serde_json::to_value(query)?,
        Duration::from_secs(3),
    )
    .await?;
    serde_json::from_value(value).context("decode LogQueryResult")
}

async fn tail_once(proc: &mut PluginProcess, id: &str) -> Result<Option<LogEntry>> {
    let request_id = proc.next_id;
    proc.next_id += 1;
    let req = serde_json::to_value(RpcRequest::new(
        request_id,
        METHOD_LOG_STORAGE_TAIL,
        Some(serde_json::to_value(filtered_query(id, false))?),
    ))?;
    proc.send(&req).await?;

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut acked = false;
    let mut event = None;
    while Instant::now() < deadline && (!acked || event.is_none()) {
        let frame = proc.read_next(deadline).await?;
        if let Some(frame_id) = frame.get("id") {
            if frame_id == &json!(request_id) {
                let response: RpcResponse = serde_json::from_value(frame)?;
                if let Some(err) = response.error {
                    return Err(anyhow!(
                        "{} returned error: {} (code {})",
                        METHOD_LOG_STORAGE_TAIL,
                        err.message,
                        err.code
                    ));
                }
                acked = true;
                continue;
            }
        }

        if frame.get("method").and_then(Value::as_str) == Some(NOTIFICATION_LOG_STORAGE_EVENT) {
            if let Some(entry) = log_event_entry(&frame, request_id)? {
                event = Some(entry);
            }
        }
    }

    if !acked {
        return Err(anyhow!("tail request was not acknowledged within deadline"));
    }
    Ok(event)
}

fn filtered_query(id: &str, follow: bool) -> LogQuery {
    LogQuery {
        source_name: Some(SOURCE_NAME.to_string()),
        target_glob: Some(format!("testkit.log_storage.{id}")),
        limit: Some(5),
        follow,
        ..Default::default()
    }
}

fn log_event_entry(frame: &Value, request_id: u64) -> Result<Option<LogEntry>> {
    let params = frame.get("params").cloned().unwrap_or(Value::Null);
    if params.get("id") != Some(&json!(request_id)) {
        return Ok(None);
    }
    if let Some(error) = params.get("error") {
        return Err(anyhow!("tail stream error: {}", short(error)));
    }
    let Some(entry) = params.get("entry").cloned() else {
        return Ok(None);
    };
    serde_json::from_value(entry)
        .map(Some)
        .context("decode log_storage/event entry")
}

fn log_entry(id_suffix: &str, message: &str) -> LogEntry {
    let id = format!(
        "testkit-{}-{}",
        id_suffix,
        Utc::now().timestamp_nanos_opt().unwrap_or(0)
    );
    LogEntry {
        id: id.clone(),
        ts: Utc::now(),
        level: LogLevel::Info,
        source: LogSource::Plugin,
        source_name: Some(SOURCE_NAME.to_string()),
        target: format!("testkit.log_storage.{id}"),
        message: message.to_string(),
        fields: json!({ "suite": "log-storage-conformance" }),
    }
}

fn short(value: &Value) -> String {
    let s = value.to_string();
    if s.len() > 160 {
        format!("{}...", &s[..160])
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
    capabilities: Vec<String>,
    _cwd: tempfile::TempDir,
}

impl PluginProcess {
    async fn spawn(plugin_path: &Path) -> Result<Self> {
        let tmp = tempfile::tempdir().context("tempdir for log-storage-conformance cwd")?;
        let log_path = tmp.path().join("events.jsonl");
        let mut cmd = Command::new(plugin_path);
        cmd.current_dir(tmp.path())
            .env("ANIMUS_LOG_FILE_PATH", &log_path)
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
            capabilities: Vec::new(),
            _cwd: tmp,
        })
    }

    fn capabilities_contains(&self, method: &str) -> bool {
        self.capabilities
            .iter()
            .any(|candidate| candidate == method)
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
            proc.capabilities = init.capabilities.methods.clone();
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

    fn init(plugin_kind: &str, methods: Vec<&str>) -> InitializeResult {
        InitializeResult {
            protocol_version: PROTOCOL_VERSION.to_string(),
            plugin_info: PluginInfo {
                name: "unit-log-storage".to_string(),
                version: "0.0.0".to_string(),
                plugin_kind: plugin_kind.to_string(),
                description: None,
            },
            capabilities: PluginCapabilities {
                methods: methods.into_iter().map(str::to_string).collect(),
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
        assert_eq!(s[3].name, "tail-replay");
    }

    #[test]
    fn handshake_classifier_passes_only_log_storage_backend_kind() {
        assert_eq!(
            run_handshake(&init(
                PLUGIN_KIND_LOG_STORAGE_BACKEND,
                vec![METHOD_LOG_STORAGE_STORE]
            ))
            .status,
            TestStatus::Pass
        );
        assert!(matches!(
            run_handshake(&init("provider", vec![])).status,
            TestStatus::Fail { reason } if reason.contains("expected `log_storage_backend`")
        ));
    }

    #[test]
    fn filtered_query_targets_unique_entry() {
        let entry = log_entry("unit", "message");
        let query = filtered_query(&entry.id, false);
        assert_eq!(query.source_name.as_deref(), Some(SOURCE_NAME));
        assert_eq!(query.target_glob.as_deref(), Some(entry.target.as_str()));
        assert!(!query.follow);
    }

    #[test]
    fn log_event_entry_ignores_other_tail_ids() {
        let entry = log_entry("event", "message");
        let frame = json!({
            "jsonrpc": "2.0",
            "method": NOTIFICATION_LOG_STORAGE_EVENT,
            "params": {
                "id": 99,
                "entry": entry
            }
        });
        let decoded = log_event_entry(&frame, 100).expect("decode frame");
        assert!(decoded.is_none());
    }

    #[test]
    fn log_event_entry_decodes_matching_entry() {
        let entry = log_entry("event", "message");
        let id = entry.id.clone();
        let frame = json!({
            "jsonrpc": "2.0",
            "method": NOTIFICATION_LOG_STORAGE_EVENT,
            "params": {
                "id": 100,
                "entry": entry
            }
        });
        let decoded = log_event_entry(&frame, 100)
            .expect("decode frame")
            .expect("matching event");
        assert_eq!(decoded.id, id);
    }
}
