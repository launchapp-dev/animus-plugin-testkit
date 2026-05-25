//! Tiny stdio plugin used by the cancellation integration test.
//!
//! Advertises `$harness/cancellation-loop-v2`, accepts `agent/run`, emits a
//! few notifications (so the harness learns the session id), then waits for
//! `agent/cancel` and replies to the original run request with a Cancelled
//! error response.

use std::io::{self, BufRead, Write};

use serde_json::{json, Value};

fn main() {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut run_request_id: Option<Value> = None;
    let session_id = "fake-session-1".to_string();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let frame: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let method = frame.get("method").and_then(Value::as_str).unwrap_or("");
        let id = frame.get("id").cloned();

        match method {
            "initialize" => {
                let result = json!({
                    "protocol_version": "1.0.0",
                    "plugin_info": {
                        "name": "fake-cancellable-plugin",
                        "version": "0.0.0",
                        "plugin_kind": "provider",
                        "description": "test fixture",
                    },
                    "capabilities": {
                        "methods": [
                            "agent/run",
                            "agent/cancel",
                            "$harness/cancellation-loop-v2",
                        ],
                        "streaming": true,
                        "progress": false,
                        "cancellation": true,
                        "subject_kinds": [],
                        "mcp_tools": [],
                    }
                });
                emit(
                    &mut stdout,
                    &json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                );
            }
            "initialized" => {}
            "agent/run" => {
                run_request_id = id.clone();
                for i in 0..3 {
                    emit(
                        &mut stdout,
                        &json!({
                            "jsonrpc": "2.0",
                            "method": "agent/output",
                            "params": {
                                "session_id": session_id,
                                "text": format!("tick{i} "),
                                "final": false,
                            }
                        }),
                    );
                }
            }
            "agent/cancel" => {
                emit(
                    &mut stdout,
                    &json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
                );
                if let Some(run_id) = run_request_id.take() {
                    emit(
                        &mut stdout,
                        &json!({
                            "jsonrpc": "2.0",
                            "id": run_id,
                            "error": {
                                "code": -32002,
                                "message": "cancelled",
                            }
                        }),
                    );
                }
                return;
            }
            "shutdown" => {
                emit(
                    &mut stdout,
                    &json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
                );
                return;
            }
            _ => {}
        }
    }
}

fn emit<W: Write>(w: &mut W, value: &Value) {
    let line = serde_json::to_string(value).expect("serialize");
    let _ = writeln!(w, "{line}");
    let _ = w.flush();
}
