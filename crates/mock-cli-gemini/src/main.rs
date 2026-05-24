//! Mock `gemini` CLI emitting `partialResult` / final `response` JSONL events.
//!
//! The gemini session backend parser currently only translates `partialResult`
//! text chunks, a top-level `text` shortcut, and the final `response` string
//! into session events. Tool-call wire shapes (`functionCall` /
//! `functionResponse`) are accepted but currently dropped by the parser — we
//! still emit them so the stream stays byte-compatible with the real CLI and
//! so that when the parser grows tool support the mock does not need to
//! change.

use std::io::{self, Write};

use serde_json::json;

fn main() {
    let scenario = std::env::var("MOCK_SCENARIO").unwrap_or_else(|_| "streaming-short".to_string());
    let session_id = format!("mock-gemini-{scenario}");

    emit(&json!({
        "session_id": session_id,
    }));

    let final_text = match scenario.as_str() {
        "streaming-short" | "resume-session" => stream_short(),
        "streaming-medium" => stream_medium(),
        "streaming-long" => stream_long(),
        "tool-call-single" => stream_tool_single(),
        "tool-call-parallel" => stream_tool_parallel(),
        "error-recovery" => stream_error_recovery(),
        _ => stream_short(),
    };

    emit(&json!({ "response": final_text }));
    emit(&json!({
        "stats": { "promptTokenCount": 4, "candidatesTokenCount": 8 }
    }));
}

fn emit(v: &serde_json::Value) {
    let line = serde_json::to_string(v).expect("serialize");
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    let _ = writeln!(handle, "{line}");
    let _ = handle.flush();
}

fn emit_partial(text: &str) {
    emit(&json!({
        "type": "partialResult",
        "partialResult": { "text": text }
    }));
}

fn stream_short() -> String {
    let chunks = ["Hello ", "world", "!"];
    for c in chunks {
        emit_partial(c);
    }
    chunks.concat()
}

fn stream_medium() -> String {
    let mut out = String::new();
    for i in 0..40 {
        let c = format!("word{i} ");
        emit_partial(&c);
        out.push_str(&c);
    }
    out
}

fn stream_long() -> String {
    let mut out = String::new();
    for i in 0..300 {
        let c = format!("token{i} ");
        emit_partial(&c);
        out.push_str(&c);
    }
    out
}

fn stream_tool_single() -> String {
    emit_partial("Looking up... ");
    emit(&json!({
        "type": "functionCall",
        "functionCall": {
            "name": "shell",
            "args": { "cmd": "ls" }
        }
    }));
    emit(&json!({
        "type": "functionResponse",
        "functionResponse": {
            "name": "shell",
            "response": { "stdout": "file_a\nfile_b\n" }
        }
    }));
    emit_partial("done.");
    "Looking up... done.".to_string()
}

fn stream_tool_parallel() -> String {
    emit_partial("Parallel ops: ");
    emit(&json!({
        "type": "functionCall",
        "functionCall": { "name": "shell", "args": { "cmd": "pwd" } }
    }));
    emit(&json!({
        "type": "functionCall",
        "functionCall": { "name": "shell", "args": { "cmd": "whoami" } }
    }));
    emit(&json!({
        "type": "functionResponse",
        "functionResponse": { "name": "shell", "response": { "stdout": "/tmp" } }
    }));
    emit(&json!({
        "type": "functionResponse",
        "functionResponse": { "name": "shell", "response": { "stdout": "user" } }
    }));
    emit_partial("complete.");
    "Parallel ops: complete.".to_string()
}

fn stream_error_recovery() -> String {
    emit_partial("Working... ");
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    let _ = writeln!(handle, "{{not-json-at-all");
    let _ = handle.flush();
    emit_partial("recovered.");
    "Working... recovered.".to_string()
}
