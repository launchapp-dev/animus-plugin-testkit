use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use animus_plugin_protocol::{
    HostCapabilities, HostInfo, InitializeParams, InitializeResult, RpcNotification, RpcRequest,
    RpcResponse, PROTOCOL_VERSION,
};
use animus_provider_protocol::{
    AgentNotification, AgentRunResponse, METHOD_AGENT_RESUME, METHOD_AGENT_RUN,
    NOTIFICATION_AGENT_ERROR, NOTIFICATION_AGENT_OUTPUT, NOTIFICATION_AGENT_THINKING,
    NOTIFICATION_AGENT_TOOL_CALL, NOTIFICATION_AGENT_TOOL_RESULT,
};
use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::time::timeout;

use testkit_core::{
    ConformanceSummary, MatrixReport, ScenarioFile, ScenarioMethod, TestResult, TestStatus,
};

use crate::spawn::PluginRunner;

const HOST_NAME: &str = "animus-plugin-harness";
const HOST_VERSION: &str = env!("CARGO_PKG_VERSION");

async fn send_frame(runner: &mut PluginRunner, value: &Value) -> Result<()> {
    let mut line = serde_json::to_string(value)?;
    line.push('\n');
    runner.stdin.write_all(line.as_bytes()).await?;
    runner.stdin.flush().await.ok();
    Ok(())
}

async fn read_frame(runner: &mut PluginRunner, deadline: Instant) -> Result<Option<Value>> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Ok(None);
    }
    let mut line = String::new();
    match timeout(remaining, runner.stdout.read_line(&mut line)).await {
        Err(_) => Ok(None),
        Ok(Ok(0)) => Err(anyhow!("plugin stdout closed unexpectedly")),
        Ok(Ok(_)) => {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return Ok(Some(Value::Null));
            }
            let v: Value = serde_json::from_str(trimmed)
                .with_context(|| format!("invalid JSON-RPC frame: {trimmed}"))?;
            Ok(Some(v))
        }
        Ok(Err(e)) => Err(e.into()),
    }
}

async fn initialize(runner: &mut PluginRunner) -> Result<InitializeResult> {
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
    send_frame(runner, &init_req).await?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let frame = read_frame(runner, deadline)
            .await?
            .ok_or_else(|| anyhow!("timed out waiting for initialize response"))?;
        if frame.is_null() {
            continue;
        }
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
            send_frame(runner, &initialized).await.ok();
            return Ok(init);
        }
    }
}

async fn run_scenario(plugin: &Path, scenario: &ScenarioFile) -> TestResult {
    let started = Instant::now();

    let mock_scenario = scenario
        .mock
        .mock_scenario
        .clone()
        .unwrap_or_else(|| scenario.name.clone());

    let mut runner = match PluginRunner::launch_with_scenario(plugin, Some(&mock_scenario)).await {
        Ok(r) => r,
        Err(e) => {
            return fail(scenario, started, vec![], None, format!("spawn: {e}"));
        }
    };

    let init = match initialize(&mut runner).await {
        Ok(i) => i,
        Err(e) => {
            runner.shutdown().await;
            return fail(scenario, started, vec![], None, format!("initialize: {e}"));
        }
    };

    let plugin_caps = init.capabilities.methods.clone();
    for required in &scenario.requires_capabilities {
        if !plugin_caps.iter().any(|c| c == required) {
            runner.shutdown().await;
            return TestResult {
                scenario: scenario.name.clone(),
                status: TestStatus::Skip {
                    reason: format!("plugin lacks capability `{required}`"),
                },
                duration_ms: started.elapsed().as_millis() as u64,
                notification_log: vec![],
                response: None,
                diagnostics: vec![],
            };
        }
    }

    let method = match scenario.method {
        ScenarioMethod::Run => METHOD_AGENT_RUN,
        ScenarioMethod::Resume => METHOD_AGENT_RESUME,
    };

    let cwd = scenario
        .request
        .cwd
        .clone()
        .unwrap_or_else(|| ".".to_string());

    let mut env_map: HashMap<String, String> = scenario.request.env.clone();
    env_map
        .entry("MOCK_SCENARIO".to_string())
        .or_insert_with(|| mock_scenario.clone());

    let mut params_map = serde_json::Map::new();
    params_map.insert(
        "prompt".to_string(),
        Value::String(scenario.request.prompt.clone()),
    );
    params_map.insert("cwd".to_string(), Value::String(cwd));
    if let Some(model) = &scenario.request.model {
        params_map.insert("model".to_string(), Value::String(model.clone()));
    }
    if let Some(sp) = &scenario.request.system_prompt {
        params_map.insert("system_prompt".to_string(), Value::String(sp.clone()));
    }
    if let Some(sid) = &scenario.request.session_id {
        params_map.insert("session_id".to_string(), Value::String(sid.clone()));
    }
    if !env_map.is_empty() {
        params_map.insert(
            "env".to_string(),
            serde_json::to_value(&env_map).unwrap_or(Value::Null),
        );
    }
    params_map.insert(
        "timeout_secs".to_string(),
        Value::Number(serde_json::Number::from(
            scenario.timeout_ms.max(1000) / 1000,
        )),
    );

    let request_id: u64 = 2;
    let req_value = serde_json::to_value(RpcRequest::new(
        request_id,
        method,
        Some(Value::Object(params_map)),
    ))
    .unwrap_or(Value::Null);
    if let Err(e) = send_frame(&mut runner, &req_value).await {
        runner.shutdown().await;
        return fail(
            scenario,
            started,
            vec![],
            None,
            format!("send request: {e}"),
        );
    }

    let deadline = Instant::now() + Duration::from_millis(scenario.timeout_ms);
    let mut notifications: Vec<AgentNotification> = Vec::new();
    let mut response: Option<AgentRunResponse> = None;
    let mut diagnostics: Vec<String> = Vec::new();
    let mut final_error: Option<String> = None;

    loop {
        let frame = match read_frame(&mut runner, deadline).await {
            Ok(Some(v)) => v,
            Ok(None) => {
                runner.shutdown().await;
                return fail(
                    scenario,
                    started,
                    notifications,
                    response,
                    format!("timeout after {}ms", scenario.timeout_ms),
                );
            }
            Err(e) => {
                runner.shutdown().await;
                return fail(
                    scenario,
                    started,
                    notifications,
                    response,
                    format!("read: {e}"),
                );
            }
        };

        if frame.is_null() {
            continue;
        }

        if frame.get("id").is_some() {
            let parsed: Result<RpcResponse, _> = serde_json::from_value(frame.clone());
            match parsed {
                Ok(r) if r.id == Some(json!(request_id)) => {
                    if let Some(err) = r.error {
                        final_error = Some(format!("{} (code {})", err.message, err.code));
                    } else if let Some(result_value) = r.result {
                        match serde_json::from_value::<AgentRunResponse>(result_value.clone()) {
                            Ok(parsed_response) => response = Some(parsed_response),
                            Err(e) => diagnostics
                                .push(format!("response did not match AgentRunResponse: {e}")),
                        }
                    }
                    break;
                }
                Ok(r) => {
                    diagnostics.push(format!("ignored response with id {:?}", r.id));
                    continue;
                }
                Err(e) => {
                    diagnostics.push(format!("malformed response frame: {e}"));
                    continue;
                }
            }
        }

        if let Some(method_name) = frame.get("method").and_then(Value::as_str) {
            if method_name.starts_with("$/") {
                continue;
            }
            let params = frame.get("params").cloned().unwrap_or(Value::Null);
            if let Some(notification) = decode_notification(method_name, &params) {
                notifications.push(notification);
            } else {
                diagnostics.push(format!("unrecognized notification: {method_name}"));
            }
        }
    }

    runner.shutdown().await;
    let duration_ms = started.elapsed().as_millis() as u64;

    if let Some(err) = final_error {
        return TestResult {
            scenario: scenario.name.clone(),
            status: TestStatus::Fail {
                reason: format!("plugin returned error: {err}"),
            },
            duration_ms,
            notification_log: notifications,
            response,
            diagnostics,
        };
    }

    if let Err(reason) = validate(scenario, &notifications, response.as_ref()) {
        return TestResult {
            scenario: scenario.name.clone(),
            status: TestStatus::Fail { reason },
            duration_ms,
            notification_log: notifications,
            response,
            diagnostics,
        };
    }

    TestResult {
        scenario: scenario.name.clone(),
        status: TestStatus::Pass,
        duration_ms,
        notification_log: notifications,
        response,
        diagnostics,
    }
}

fn decode_notification(method: &str, params: &Value) -> Option<AgentNotification> {
    let session_id = params
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    match method {
        NOTIFICATION_AGENT_OUTPUT => Some(AgentNotification::Output {
            session_id,
            text: params
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            is_final: params
                .get("final")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }),
        NOTIFICATION_AGENT_THINKING => Some(AgentNotification::Thinking {
            session_id,
            text: params
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        }),
        NOTIFICATION_AGENT_TOOL_CALL => Some(AgentNotification::ToolCall {
            session_id,
            name: params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            arguments: params.get("arguments").cloned().unwrap_or(Value::Null),
            server: params
                .get("server")
                .and_then(Value::as_str)
                .map(str::to_string),
        }),
        NOTIFICATION_AGENT_TOOL_RESULT => Some(AgentNotification::ToolResult {
            session_id,
            name: params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            output: params.get("output").cloned().unwrap_or(Value::Null),
            success: params
                .get("success")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        }),
        NOTIFICATION_AGENT_ERROR => Some(AgentNotification::Error {
            session_id,
            message: params
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            recoverable: params
                .get("recoverable")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }),
        _ => None,
    }
}

fn validate(
    scenario: &ScenarioFile,
    notifications: &[AgentNotification],
    response: Option<&AgentRunResponse>,
) -> std::result::Result<(), String> {
    let mut cursor = 0usize;
    for expected in &scenario.expected_notifications {
        let mut found = None;
        for (i, n) in notifications.iter().enumerate().skip(cursor) {
            if expected.matches(n) {
                found = Some(i);
                break;
            }
        }
        match found {
            Some(i) => cursor = i + 1,
            None => {
                return Err(format!(
                    "expected notification `{}` not found after index {cursor}",
                    expected.label()
                ));
            }
        }
    }

    if let Some(r) = response {
        if let Some(needle) = &scenario.expected_response.output_contains {
            if !r.output.contains(needle) {
                return Err(format!("response.output missing substring `{needle}`"));
            }
        }
        if let Some(min) = scenario.expected_response.min_output_len {
            if r.output.len() < min {
                return Err(format!(
                    "response.output length {} below min {min}",
                    r.output.len()
                ));
            }
        }
        if let Some(exit) = scenario.expected_response.exit_code {
            if r.exit_code != exit {
                return Err(format!(
                    "response.exit_code {} != expected {exit}",
                    r.exit_code
                ));
            }
        }
    } else if scenario.expected_response.output_contains.is_some()
        || scenario.expected_response.min_output_len.is_some()
        || scenario.expected_response.exit_code.is_some()
    {
        return Err("no AgentRunResponse received but expected_response set".to_string());
    }

    Ok(())
}

fn fail(
    scenario: &ScenarioFile,
    started: Instant,
    notifications: Vec<AgentNotification>,
    response: Option<AgentRunResponse>,
    reason: String,
) -> TestResult {
    TestResult {
        scenario: scenario.name.clone(),
        status: TestStatus::Fail { reason },
        duration_ms: started.elapsed().as_millis() as u64,
        notification_log: notifications,
        response,
        diagnostics: vec![],
    }
}

async fn discover_plugin_info(plugin: &Path) -> Result<InitializeResult> {
    let mut runner = PluginRunner::launch(plugin).await?;
    let init = initialize(&mut runner).await;
    runner.shutdown().await;
    init
}

pub async fn run_all(
    plugin: PathBuf,
    scenarios: Vec<ScenarioFile>,
    only: Option<String>,
) -> Result<MatrixReport> {
    let init = discover_plugin_info(&plugin)
        .await
        .context("initial plugin probe (handshake)")?;

    let started_at = Utc::now();
    let mut results = Vec::new();
    for scenario in &scenarios {
        if let Some(filter) = &only {
            if &scenario.name != filter {
                continue;
            }
        }
        let res = run_scenario(&plugin, scenario).await;
        results.push(res);
    }
    let finished_at = Utc::now();

    let summary = ConformanceSummary::from_results(&results);
    Ok(MatrixReport {
        plugin_name: init.plugin_info.name,
        plugin_version: init.plugin_info.version,
        plugin_kind: init.plugin_info.plugin_kind,
        protocol_version: init.protocol_version,
        started_at,
        finished_at,
        scenarios: results,
        summary,
        host: HashMap::new(),
    })
}
