//! Mock `codex` CLI emitting `item.completed`-shape JSONL events.

use std::io::{self, Write};

use serde_json::json;

fn main() {
    let scenario = std::env::var("MOCK_SCENARIO").unwrap_or_else(|_| "streaming-short".to_string());
    let session_id = format!("mock-codex-{scenario}");

    emit(&json!({
        "type": "session.started",
        "session_id": session_id,
    }));

    let text = match scenario.as_str() {
        "streaming-short" => "Hello world!".to_string(),
        "streaming-medium" => (0..40).map(|i| format!("word{i} ")).collect(),
        "streaming-long" => (0..300).map(|i| format!("token{i} ")).collect(),
        _ => "Hello world!".to_string(),
    };

    emit(&json!({
        "type": "item.completed",
        "item": {
            "type": "agent_message",
            "text": text,
        }
    }));
}

fn emit(v: &serde_json::Value) {
    let line = serde_json::to_string(v).expect("serialize");
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    let _ = writeln!(handle, "{line}");
    let _ = handle.flush();
}
