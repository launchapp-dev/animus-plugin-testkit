//! Mock `gemini` CLI emitting partialResult/content events.

use std::io::{self, Write};

use serde_json::json;

fn main() {
    let scenario = std::env::var("MOCK_SCENARIO").unwrap_or_else(|_| "streaming-short".to_string());

    let chunks: Vec<String> = match scenario.as_str() {
        "streaming-short" => vec!["Hello ".into(), "world".into(), "!".into()],
        "streaming-medium" => (0..40).map(|i| format!("word{i} ")).collect(),
        "streaming-long" => (0..300).map(|i| format!("token{i} ")).collect(),
        _ => vec!["Hello world!".into()],
    };

    for chunk in &chunks {
        emit(&json!({
            "type": "partialResult",
            "partialResult": { "text": chunk }
        }));
    }

    let combined: String = chunks.concat();
    emit(&json!({
        "response": combined,
    }));
}

fn emit(v: &serde_json::Value) {
    let line = serde_json::to_string(v).expect("serialize");
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    let _ = writeln!(handle, "{line}");
    let _ = handle.flush();
}
