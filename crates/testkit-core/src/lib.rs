//! Shared types for the Animus plugin testkit.
//!
//! Defines the on-disk scenario format, the in-memory typed scenario the
//! harness consumes, the per-scenario result the harness emits, and the
//! aggregated matrix report the matrix runner produces.

use std::collections::HashMap;
use std::path::Path;

use animus_provider_protocol::{AgentNotification, AgentRunResponse};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TestkitError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("yaml: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid scenario `{0}`: {1}")]
    InvalidScenario(String, String),
}

/// One declarative scenario as it appears in `scenarios/*.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioFile {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    pub request: ScenarioRequest,
    #[serde(default)]
    pub expected_notifications: Vec<ExpectedNotification>,
    #[serde(default)]
    pub allow_extra_notifications: bool,
    #[serde(default)]
    pub expected_response: ExpectedResponse,
    #[serde(default)]
    pub mock: MockHint,
    #[serde(default)]
    pub requires_capabilities: Vec<String>,
    #[serde(default)]
    pub skip_if_capabilities: Vec<String>,
    #[serde(default)]
    pub method: ScenarioMethod,
    /// When set, the harness spawns a side-task that issues
    /// `agent/cancel { session_id }` this many milliseconds after the run
    /// request is sent. The scenario PASSes if the plugin terminates the
    /// run with a `BackendError::Cancelled` (`-32002`) error response or
    /// emits an `error` notification flagged as non-recoverable within the
    /// scenario `timeout_ms`. Requires the plugin to advertise
    /// `$harness/cancellation-loop-v2` in its initialize capabilities.
    #[serde(default)]
    pub cancel_after_ms: Option<u64>,
}

fn default_timeout_ms() -> u64 {
    10_000
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioMethod {
    #[default]
    Run,
    Resume,
}

/// Subset of `AgentRunRequest` the scenario author needs to set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioRequest {
    pub prompt: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

/// Loose matcher for one expected notification in the stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExpectedNotification {
    Output {
        #[serde(default)]
        contains: Option<String>,
    },
    Thinking,
    ToolCall {
        #[serde(default)]
        name: Option<String>,
    },
    ToolResult,
    Error {
        #[serde(default)]
        recoverable: Option<bool>,
    },
}

impl ExpectedNotification {
    pub fn matches(&self, n: &AgentNotification) -> bool {
        match (self, n) {
            (ExpectedNotification::Output { contains }, AgentNotification::Output { text, .. }) => {
                contains.as_ref().is_none_or(|c| text.contains(c))
            }
            (ExpectedNotification::Thinking, AgentNotification::Thinking { .. }) => true,
            (
                ExpectedNotification::ToolCall { name },
                AgentNotification::ToolCall { name: n2, .. },
            ) => name.as_ref().is_none_or(|n| n == n2),
            (ExpectedNotification::ToolResult, AgentNotification::ToolResult { .. }) => true,
            (
                ExpectedNotification::Error { recoverable },
                AgentNotification::Error {
                    recoverable: r2, ..
                },
            ) => recoverable.is_none_or(|r| r == *r2),
            _ => false,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            ExpectedNotification::Output { .. } => "output",
            ExpectedNotification::Thinking => "thinking",
            ExpectedNotification::ToolCall { .. } => "toolCall",
            ExpectedNotification::ToolResult => "toolResult",
            ExpectedNotification::Error { .. } => "error",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExpectedResponse {
    #[serde(default)]
    pub output_contains: Option<String>,
    #[serde(default)]
    pub min_output_len: Option<usize>,
    #[serde(default)]
    pub exit_code: Option<i32>,
}

/// Tells the harness which mock CLI to wire up (and which scenario id to
/// pass to it via `MOCK_SCENARIO`). The harness exports the appropriate
/// env var (`CLAUDE_BIN`, `CODEX_BIN`, ...) before spawning the plugin.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MockHint {
    /// `claude`, `codex`, `gemini`, `opencode`, `oai`, or `none`.
    #[serde(default)]
    pub tool: Option<String>,
    /// Identifier the mock CLI uses to pick its canonical response set.
    #[serde(default)]
    pub mock_scenario: Option<String>,
}

/// Result of running one scenario against one plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResult {
    pub scenario: String,
    pub status: TestStatus,
    pub duration_ms: u64,
    pub notification_log: Vec<AgentNotification>,
    pub response: Option<AgentRunResponse>,
    #[serde(default)]
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TestStatus {
    Pass,
    Fail { reason: String },
    Skip { reason: String },
}

impl TestStatus {
    pub fn is_pass(&self) -> bool {
        matches!(self, TestStatus::Pass)
    }
    pub fn is_skip(&self) -> bool {
        matches!(self, TestStatus::Skip { .. })
    }
}

/// Per-plugin run summary, suitable for CI artifacts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatrixReport {
    pub plugin_name: String,
    pub plugin_version: String,
    pub plugin_kind: String,
    pub protocol_version: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub scenarios: Vec<TestResult>,
    pub summary: ConformanceSummary,
    #[serde(default)]
    pub host: HashMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConformanceSummary {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
}

impl ConformanceSummary {
    pub fn from_results(results: &[TestResult]) -> Self {
        let mut s = ConformanceSummary {
            total: results.len(),
            ..Default::default()
        };
        for r in results {
            match &r.status {
                TestStatus::Pass => s.passed += 1,
                TestStatus::Fail { .. } => s.failed += 1,
                TestStatus::Skip { .. } => s.skipped += 1,
            }
        }
        s
    }

    pub fn overall_pass(&self) -> bool {
        self.failed == 0 && self.total > 0
    }
}

/// Load a scenario file from disk.
pub fn load_scenario(path: &Path) -> Result<ScenarioFile, TestkitError> {
    let raw = std::fs::read_to_string(path)?;
    let scenario: ScenarioFile = serde_yaml::from_str(&raw)?;
    if scenario.name.trim().is_empty() {
        return Err(TestkitError::InvalidScenario(
            path.display().to_string(),
            "scenario name is empty".into(),
        ));
    }
    Ok(scenario)
}

/// Load every `*.yaml` and `*.yml` file in a directory.
pub fn load_scenario_dir(dir: &Path) -> Result<Vec<ScenarioFile>, TestkitError> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        if matches!(ext, "yaml" | "yml") {
            out.push(load_scenario(&path)?);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("animus-testkit-core-{name}-{unique}"));
        fs::create_dir_all(&path).expect("test temp dir should be created");
        path
    }

    #[test]
    fn parses_minimal_scenario() {
        let yaml = r#"
name: streaming-short
description: just a hello
timeout_ms: 5000
request:
  prompt: "say hi"
  model: claude-sonnet-4-6
expected_notifications:
  - kind: output
    contains: "hi"
expected_response:
  min_output_len: 1
mock:
  tool: claude
  mock_scenario: streaming-short
skip_if_capabilities:
  - "$harness/incompatible"
"#;
        let s: ScenarioFile = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(s.name, "streaming-short");
        assert_eq!(s.expected_notifications.len(), 1);
        assert_eq!(s.skip_if_capabilities, vec!["$harness/incompatible"]);
    }

    #[test]
    fn matcher_contains_works() {
        let n = AgentNotification::Output {
            session_id: "s".into(),
            text: "hello world".into(),
            is_final: false,
        };
        let m = ExpectedNotification::Output {
            contains: Some("world".into()),
        };
        assert!(m.matches(&n));
        let bad = ExpectedNotification::Output {
            contains: Some("nope".into()),
        };
        assert!(!bad.matches(&n));
    }

    #[test]
    fn matcher_variants_check_their_relevant_fields() {
        assert!(
            ExpectedNotification::Thinking.matches(&AgentNotification::Thinking {
                session_id: "s".into(),
                text: "thinking".into(),
            })
        );
        assert!(ExpectedNotification::ToolCall {
            name: Some("shell".into())
        }
        .matches(&AgentNotification::ToolCall {
            session_id: "s".into(),
            name: "shell".into(),
            arguments: Value::Null,
            server: None,
        }));
        assert!(!ExpectedNotification::ToolCall {
            name: Some("browser".into())
        }
        .matches(&AgentNotification::ToolCall {
            session_id: "s".into(),
            name: "shell".into(),
            arguments: Value::Null,
            server: None,
        }));
        assert!(
            ExpectedNotification::ToolResult.matches(&AgentNotification::ToolResult {
                session_id: "s".into(),
                name: "shell".into(),
                output: Value::Null,
                success: true,
            })
        );
        assert!(ExpectedNotification::Error {
            recoverable: Some(false)
        }
        .matches(&AgentNotification::Error {
            session_id: "s".into(),
            message: "terminal".into(),
            recoverable: false,
        }));
    }

    #[test]
    fn summary_counts_correctly() {
        let results = vec![
            TestResult {
                scenario: "a".into(),
                status: TestStatus::Pass,
                duration_ms: 1,
                notification_log: vec![],
                response: None,
                diagnostics: vec![],
            },
            TestResult {
                scenario: "b".into(),
                status: TestStatus::Fail { reason: "x".into() },
                duration_ms: 1,
                notification_log: vec![],
                response: None,
                diagnostics: vec![],
            },
            TestResult {
                scenario: "c".into(),
                status: TestStatus::Skip {
                    reason: "no cap".into(),
                },
                duration_ms: 0,
                notification_log: vec![],
                response: None,
                diagnostics: vec![],
            },
        ];
        let s = ConformanceSummary::from_results(&results);
        assert_eq!(s.total, 3);
        assert_eq!(s.passed, 1);
        assert_eq!(s.failed, 1);
        assert_eq!(s.skipped, 1);
        assert!(!s.overall_pass());
    }

    #[test]
    fn scenario_dir_loads_yaml_files_sorted_by_scenario_name() {
        let dir = temp_dir("sorted");
        fs::write(dir.join("z.yaml"), "name: zeta\nrequest:\n  prompt: z\n").unwrap();
        fs::write(dir.join("a.yml"), "name: alpha\nrequest:\n  prompt: a\n").unwrap();
        fs::write(
            dir.join("ignored.txt"),
            "name: ignored\nrequest:\n  prompt: nope\n",
        )
        .unwrap();

        let scenarios = load_scenario_dir(&dir).expect("scenario dir should load");
        let names: Vec<&str> = scenarios.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "zeta"]);

        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn load_scenario_rejects_blank_names() {
        let dir = temp_dir("blank-name");
        let path = dir.join("blank.yaml");
        fs::write(&path, "name: '   '\nrequest:\n  prompt: nope\n").unwrap();

        let err = load_scenario(&path).expect_err("blank scenario name should fail");
        assert!(
            matches!(err, TestkitError::InvalidScenario(_, ref reason) if reason == "scenario name is empty"),
            "unexpected error: {err}"
        );

        fs::remove_dir_all(dir).ok();
    }
}
