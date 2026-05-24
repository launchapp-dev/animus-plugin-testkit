//! Mock OpenAI-compatible HTTP server.
//!
//! Listens on `--port` / `MOCK_OAI_PORT` (default 18080) and serves
//! `POST /v1/chat/completions` with a canonical SSE stream selected by
//! `--scenario` / `MOCK_SCENARIO`. Designed for use with `animus-provider-oai`
//! via `OPENAI_BASE_URL=http://127.0.0.1:<port>/v1`.
//!
//! Resume detection is structural: the OAI Chat Completions API is stateless,
//! so a "resume" call is just a follow-up `messages` array containing prior
//! `assistant` (or `tool`) turns. When the incoming `messages` carry any
//! assistant turn, we emit the `resume-session` continuation regardless of the
//! configured `MOCK_SCENARIO`.

use std::convert::Infallible;

use axum::extract::Json;
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::routing::{get, post};
use axum::Router;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = parse_args();
    if let Some(p) = args.port {
        std::env::set_var("MOCK_OAI_PORT", p.to_string());
    }
    if let Some(s) = args.scenario {
        std::env::set_var("MOCK_SCENARIO", s);
    }

    let port: u16 = std::env::var("MOCK_OAI_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(18080);

    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/models", get(list_models));

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    eprintln!("mock-oai listening on http://127.0.0.1:{port}");
    axum::serve(listener, app).await?;
    Ok(())
}

struct Args {
    port: Option<u16>,
    scenario: Option<String>,
}

fn parse_args() -> Args {
    let mut iter = std::env::args().skip(1);
    let mut port = None;
    let mut scenario = None;
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--port" => {
                port = iter.next().and_then(|s| s.parse().ok());
            }
            "--scenario" => {
                scenario = iter.next();
            }
            _ => {}
        }
    }
    Args { port, scenario }
}

async fn health() -> (StatusCode, &'static str) {
    (StatusCode::OK, "ok")
}

async fn list_models() -> Json<Value> {
    Json(json!({
        "object": "list",
        "data": [
            { "id": "gpt-5", "object": "model" },
            { "id": "gpt-5-mini", "object": "model" },
        ]
    }))
}

async fn chat_completions(
    Json(body): Json<Value>,
) -> Result<Sse<ReceiverStream<Result<Event, Infallible>>>, (StatusCode, String)> {
    let stream_flag = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    if !stream_flag {
        return Err((
            StatusCode::BAD_REQUEST,
            "mock-oai requires stream:true".to_string(),
        ));
    }

    let configured =
        std::env::var("MOCK_SCENARIO").unwrap_or_else(|_| "streaming-short".to_string());
    let scenario = if has_assistant_history(&body) {
        "resume-session".to_string()
    } else {
        configured
    };
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("gpt-5")
        .to_string();

    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(64);
    tokio::spawn(async move {
        match scenario.as_str() {
            "streaming-short" => emit_text_chunks(&tx, &model, &["Hello ", "world", "!"]).await,
            "streaming-medium" => {
                let chunks: Vec<String> = (0..40).map(|i| format!("word{i} ")).collect();
                let refs: Vec<&str> = chunks.iter().map(String::as_str).collect();
                emit_text_chunks(&tx, &model, &refs).await
            }
            "streaming-long" => {
                let chunks: Vec<String> = (0..300).map(|i| format!("token{i} ")).collect();
                let refs: Vec<&str> = chunks.iter().map(String::as_str).collect();
                emit_text_chunks(&tx, &model, &refs).await
            }
            "tool-call-single" => emit_tool_single(&tx, &model).await,
            "tool-call-parallel" => emit_tool_parallel(&tx, &model).await,
            "error-recovery" => emit_error_recovery(&tx, &model).await,
            "resume-session" => emit_text_chunks(&tx, &model, &["Hello ", "again", "!"]).await,
            _ => emit_text_chunks(&tx, &model, &["Hello ", "world", "!"]).await,
        }

        let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
    });

    Ok(Sse::new(ReceiverStream::new(rx)))
}

fn has_assistant_history(body: &Value) -> bool {
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return false;
    };
    messages.iter().any(|m| {
        matches!(
            m.get("role").and_then(Value::as_str),
            Some("assistant") | Some("tool")
        )
    })
}

async fn send_chunk(
    tx: &mpsc::Sender<Result<Event, Infallible>>,
    model: &str,
    delta: Value,
    finish_reason: Option<&str>,
) {
    let mut choice = json!({ "index": 0, "delta": delta });
    if let Some(reason) = finish_reason {
        choice["finish_reason"] = Value::String(reason.to_string());
    }
    let payload = json!({
        "id": "chatcmpl-mock",
        "object": "chat.completion.chunk",
        "model": model,
        "choices": [choice],
    });
    let _ = tx
        .send(Ok(Event::default().data(payload.to_string())))
        .await;
}

async fn emit_text_chunks(
    tx: &mpsc::Sender<Result<Event, Infallible>>,
    model: &str,
    chunks: &[&str],
) {
    for chunk in chunks {
        send_chunk(tx, model, json!({ "content": chunk }), None).await;
    }
    send_chunk(tx, model, json!({}), Some("stop")).await;
}

async fn emit_tool_single(tx: &mpsc::Sender<Result<Event, Infallible>>, model: &str) {
    send_chunk(tx, model, json!({ "content": "Looking up... " }), None).await;
    send_chunk(
        tx,
        model,
        json!({
            "tool_calls": [{
                "index": 0,
                "id": "call_1",
                "type": "function",
                "function": { "name": "shell", "arguments": "" },
            }]
        }),
        None,
    )
    .await;
    send_chunk(
        tx,
        model,
        json!({
            "tool_calls": [{
                "index": 0,
                "function": { "arguments": "{\"cmd\":" },
            }]
        }),
        None,
    )
    .await;
    send_chunk(
        tx,
        model,
        json!({
            "tool_calls": [{
                "index": 0,
                "function": { "arguments": "\"ls\"}" },
            }]
        }),
        None,
    )
    .await;
    send_chunk(tx, model, json!({}), Some("tool_calls")).await;
}

async fn emit_tool_parallel(tx: &mpsc::Sender<Result<Event, Infallible>>, model: &str) {
    send_chunk(tx, model, json!({ "content": "Parallel ops: " }), None).await;
    send_chunk(
        tx,
        model,
        json!({
            "tool_calls": [
                {
                    "index": 0,
                    "id": "call_a",
                    "type": "function",
                    "function": { "name": "shell", "arguments": "" },
                },
                {
                    "index": 1,
                    "id": "call_b",
                    "type": "function",
                    "function": { "name": "shell", "arguments": "" },
                }
            ]
        }),
        None,
    )
    .await;
    send_chunk(
        tx,
        model,
        json!({
            "tool_calls": [
                { "index": 0, "function": { "arguments": "{\"cmd\":\"pwd\"}" } },
                { "index": 1, "function": { "arguments": "{\"cmd\":\"whoami\"}" } }
            ]
        }),
        None,
    )
    .await;
    send_chunk(tx, model, json!({}), Some("tool_calls")).await;
}

async fn emit_error_recovery(tx: &mpsc::Sender<Result<Event, Infallible>>, model: &str) {
    send_chunk(tx, model, json!({ "content": "Working... " }), None).await;
    let _ = tx.send(Ok(Event::default().data("{not-json-at-all"))).await;
    send_chunk(tx, model, json!({ "content": "recovered." }), None).await;
    send_chunk(tx, model, json!({}), Some("stop")).await;
}
