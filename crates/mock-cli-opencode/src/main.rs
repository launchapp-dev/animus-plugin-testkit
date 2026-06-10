//! Mock `opencode` CLI emitting canonical `text` and tool-flavored JSONL events.
//!
//! The opencode session backend parser currently translates `text` events
//! into TextDelta and a top-level `content` string into FinalText. Tool-call
//! wire shapes (`tool_use` / `tool_result`) are accepted but currently
//! dropped by the parser — we still emit them so the stream stays
//! byte-compatible with the real CLI and so that when the parser grows tool
//! support the mock does not need to change.

use std::io::{self, Write};

use serde_json::json;

fn main() {
    let scenario = std::env::var("MOCK_SCENARIO").unwrap_or_else(|_| "streaming-short".to_string());

    let final_text = match scenario.as_str() {
        "streaming-short" | "resume-session" => stream_short(),
        "streaming-medium" => stream_medium(),
        "streaming-long" => stream_long(),
        "tool-call-single" => stream_tool_single(),
        "tool-call-parallel" => stream_tool_parallel(),
        "error-recovery" => stream_error_recovery(),
        "cancellation" => stream_cancellation(),
        _ => stream_short(),
    };

    emit(&json!({ "content": final_text }));
}

fn emit(v: &serde_json::Value) {
    let line = serde_json::to_string(v).expect("serialize");
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    let _ = writeln!(handle, "{line}");
    let _ = handle.flush();
}

fn emit_text(text: &str) {
    emit(&json!({ "type": "text", "text": text }));
}

fn stream_short() -> String {
    let parts = ["Hello ", "world", "!"];
    for p in parts {
        emit_text(p);
    }
    parts.concat()
}

fn stream_medium() -> String {
    let mut out = String::new();
    for i in 0..40 {
        let c = format!("word{i} ");
        emit_text(&c);
        out.push_str(&c);
    }
    out
}

fn stream_long() -> String {
    let mut out = String::new();
    for i in 0..300 {
        let c = format!("token{i} ");
        emit_text(&c);
        out.push_str(&c);
    }
    out
}

fn stream_tool_single() -> String {
    emit_text("Looking up... ");
    emit(&json!({
        "type": "tool_use",
        "tool_use": {
            "id": "tool_1",
            "name": "shell",
            "input": { "cmd": "ls" }
        }
    }));
    emit(&json!({
        "type": "tool_result",
        "tool_result": {
            "tool_use_id": "tool_1",
            "content": "file_a\nfile_b\n"
        }
    }));
    emit_text("done.");
    "Looking up... done.".to_string()
}

fn stream_tool_parallel() -> String {
    emit_text("Parallel ops: ");
    emit(&json!({
        "type": "tool_use",
        "tool_use": { "id": "tool_a", "name": "shell", "input": { "cmd": "pwd" } }
    }));
    emit(&json!({
        "type": "tool_use",
        "tool_use": { "id": "tool_b", "name": "shell", "input": { "cmd": "whoami" } }
    }));
    emit(&json!({
        "type": "tool_result",
        "tool_result": { "tool_use_id": "tool_a", "content": "/tmp" }
    }));
    emit(&json!({
        "type": "tool_result",
        "tool_result": { "tool_use_id": "tool_b", "content": "user" }
    }));
    emit_text("complete.");
    "Parallel ops: complete.".to_string()
}

fn stream_error_recovery() -> String {
    emit_text("Working... ");
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    let _ = writeln!(handle, "{{not-json-at-all");
    let _ = handle.flush();
    emit_text("recovered.");
    "Working... recovered.".to_string()
}

fn stream_cancellation() -> String {
    let mut out = String::new();
    for i in 0..120 {
        let chunk = format!("cancel-token-{i} ");
        emit_text(&chunk);
        out.push_str(&chunk);
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    out
}
