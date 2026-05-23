//! Mock `claude` CLI.
//!
//! Mimics enough of `claude --print --verbose --output-format stream-json` to
//! satisfy the real `animus-session-backend` claude transport. The actual
//! output is selected by the `MOCK_SCENARIO` environment variable (set by
//! the testkit harness). Unknown scenarios fall back to `streaming-short`.

use std::io::{self, Write};

use serde_json::json;

fn main() {
    let scenario = std::env::var("MOCK_SCENARIO").unwrap_or_else(|_| "streaming-short".to_string());

    // Real claude emits an initial `system` line carrying the session id.
    let session_id = format!("mock-session-{scenario}");
    emit(&json!({
        "type": "system",
        "subtype": "init",
        "session_id": session_id,
    }));

    let final_text = match scenario.as_str() {
        "streaming-short" => stream_short(&session_id),
        "streaming-medium" => stream_medium(&session_id),
        "streaming-long" => stream_long(&session_id),
        "tool-call-single" => stream_tool_single(&session_id),
        "tool-call-parallel" => stream_tool_parallel(&session_id),
        "error-recovery" => stream_error_recovery(&session_id),
        "resume-session" => stream_short(&session_id),
        _ => stream_short(&session_id),
    };

    // Final result line.
    emit(&json!({
        "type": "result",
        "subtype": "success",
        "session_id": session_id,
        "result": final_text,
        "is_error": false,
        "duration_ms": 5,
        "num_turns": 1,
    }));
}

fn emit(v: &serde_json::Value) {
    let line = serde_json::to_string(v).expect("serialize");
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    let _ = writeln!(handle, "{line}");
    let _ = handle.flush();
}

fn delta(session_id: &str, text: &str) {
    emit(&json!({
        "type": "content_block_delta",
        "session_id": session_id,
        "index": 0,
        "delta": { "type": "text_delta", "text": text },
    }));
}

fn stream_short(session_id: &str) -> String {
    let parts = ["Hello ", "world", "!"];
    for p in parts {
        delta(session_id, p);
    }
    parts.concat()
}

fn stream_medium(session_id: &str) -> String {
    let mut out = String::new();
    for i in 0..40 {
        let chunk = format!("word{i} ");
        delta(session_id, &chunk);
        out.push_str(&chunk);
    }
    out
}

fn stream_long(session_id: &str) -> String {
    let mut out = String::new();
    for i in 0..300 {
        let chunk = format!("token{i} ");
        delta(session_id, &chunk);
        out.push_str(&chunk);
    }
    out
}

fn stream_tool_single(session_id: &str) -> String {
    delta(session_id, "Looking up... ");
    emit(&json!({
        "type": "assistant",
        "session_id": session_id,
        "message": {
            "content": [
                {
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "shell",
                    "input": { "cmd": "ls" }
                }
            ]
        }
    }));
    emit(&json!({
        "type": "user",
        "session_id": session_id,
        "message": {
            "content": [
                {
                    "type": "tool_result",
                    "tool_use_id": "toolu_1",
                    "content": "file_a\nfile_b\n"
                }
            ]
        }
    }));
    delta(session_id, "done.");
    "Looking up... done.".to_string()
}

fn stream_tool_parallel(session_id: &str) -> String {
    delta(session_id, "Parallel ops: ");
    emit(&json!({
        "type": "assistant",
        "session_id": session_id,
        "message": {
            "content": [
                { "type": "tool_use", "id": "toolu_a", "name": "shell", "input": { "cmd": "pwd" } },
                { "type": "tool_use", "id": "toolu_b", "name": "shell", "input": { "cmd": "whoami" } }
            ]
        }
    }));
    emit(&json!({
        "type": "user",
        "session_id": session_id,
        "message": {
            "content": [
                { "type": "tool_result", "tool_use_id": "toolu_a", "content": "/tmp" },
                { "type": "tool_result", "tool_use_id": "toolu_b", "content": "user" }
            ]
        }
    }));
    delta(session_id, "complete.");
    "Parallel ops: complete.".to_string()
}

fn stream_error_recovery(session_id: &str) -> String {
    delta(session_id, "Working... ");
    // Garbled line — real provider parsers ignore it.
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    let _ = writeln!(handle, "{{not-json-at-all");
    let _ = handle.flush();
    delta(session_id, "recovered.");
    "Working... recovered.".to_string()
}
