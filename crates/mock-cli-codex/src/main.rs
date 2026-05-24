//! Mock `codex` CLI emitting `thread.started` / `item.completed` JSONL events.
//!
//! The codex session backend's parser only knows `thread.started`,
//! `turn.started`, `turn.completed`, and `item.completed` events (and within
//! `item.completed` it only translates `agent_message` / `message` text and
//! `reasoning` text). Tool-call wire shapes are accepted but currently dropped
//! by the parser — we still emit them so the scenarios stay byte-compatible
//! with the real codex stream and so that when the parser grows tool support
//! the mock does not need to change.

use std::io::{self, Write};

use serde_json::json;

fn main() {
    let scenario = std::env::var("MOCK_SCENARIO").unwrap_or_else(|_| "streaming-short".to_string());
    let session_id = format!("mock-codex-{scenario}");

    emit(&json!({
        "type": "thread.started",
        "thread_id": session_id,
    }));
    emit(&json!({
        "type": "turn.started",
    }));

    match scenario.as_str() {
        "streaming-short" => stream_short(),
        "streaming-medium" => stream_medium(),
        "streaming-long" => stream_long(),
        "tool-call-single" => stream_tool_single(),
        "tool-call-parallel" => stream_tool_parallel(),
        "error-recovery" => stream_error_recovery(),
        "resume-session" => stream_short(),
        _ => stream_short(),
    }

    emit(&json!({
        "type": "turn.completed",
        "usage": { "input_tokens": 4, "output_tokens": 8 },
    }));
}

fn emit(v: &serde_json::Value) {
    let line = serde_json::to_string(v).expect("serialize");
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    let _ = writeln!(handle, "{line}");
    let _ = handle.flush();
}

fn emit_agent_chunk(text: &str) {
    emit(&json!({
        "type": "item.completed",
        "item": { "type": "agent_message", "text": text },
    }));
}

fn stream_short() {
    let parts = ["Hello ", "Hello world", "Hello world!"];
    for cumulative in parts {
        emit_agent_chunk(cumulative);
    }
}

fn stream_medium() {
    let mut buf = String::new();
    for i in 0..40 {
        buf.push_str(&format!("word{i} "));
        emit_agent_chunk(&buf);
    }
}

fn stream_long() {
    let mut buf = String::new();
    for i in 0..300 {
        buf.push_str(&format!("token{i} "));
        emit_agent_chunk(&buf);
    }
}

fn stream_tool_single() {
    emit_agent_chunk("Looking up... ");
    emit(&json!({
        "type": "item.completed",
        "item": {
            "type": "function_call",
            "call_id": "call_1",
            "name": "shell",
            "arguments": "{\"cmd\":\"ls\"}",
        }
    }));
    emit(&json!({
        "type": "item.completed",
        "item": {
            "type": "function_call_output",
            "call_id": "call_1",
            "output": "file_a\nfile_b\n",
        }
    }));
    emit_agent_chunk("Looking up... done.");
}

fn stream_tool_parallel() {
    emit_agent_chunk("Parallel ops: ");
    emit(&json!({
        "type": "item.completed",
        "item": {
            "type": "function_call",
            "call_id": "call_a",
            "name": "shell",
            "arguments": "{\"cmd\":\"pwd\"}",
        }
    }));
    emit(&json!({
        "type": "item.completed",
        "item": {
            "type": "function_call",
            "call_id": "call_b",
            "name": "shell",
            "arguments": "{\"cmd\":\"whoami\"}",
        }
    }));
    emit(&json!({
        "type": "item.completed",
        "item": {
            "type": "function_call_output",
            "call_id": "call_a",
            "output": "/tmp",
        }
    }));
    emit(&json!({
        "type": "item.completed",
        "item": {
            "type": "function_call_output",
            "call_id": "call_b",
            "output": "user",
        }
    }));
    emit_agent_chunk("Parallel ops: complete.");
}

fn stream_error_recovery() {
    emit_agent_chunk("Working... ");
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    let _ = writeln!(handle, "{{not-json-at-all");
    let _ = handle.flush();
    emit_agent_chunk("Working... recovered.");
}
