//! Mock OpenAI-compatible HTTP server.
//!
//! Listens on `MOCK_OAI_PORT` (default 18080) and serves
//! `POST /v1/chat/completions` with a canonical SSE stream selected by the
//! `MOCK_SCENARIO` env var. Designed for use with `animus-provider-oai` via
//! `OPENAI_BASE_URL=http://127.0.0.1:<port>/v1`.

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

    let scenario = std::env::var("MOCK_SCENARIO").unwrap_or_else(|_| "streaming-short".to_string());
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("gpt-5")
        .to_string();

    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(64);
    tokio::spawn(async move {
        let chunks: Vec<String> = match scenario.as_str() {
            "streaming-short" => vec!["Hello ".into(), "world".into(), "!".into()],
            "streaming-medium" => (0..40).map(|i| format!("word{i} ")).collect(),
            "streaming-long" => (0..300).map(|i| format!("token{i} ")).collect(),
            _ => vec!["Hello world!".into()],
        };
        for chunk in chunks {
            let payload = json!({
                "id": "chatcmpl-mock",
                "object": "chat.completion.chunk",
                "model": model,
                "choices": [
                    { "index": 0, "delta": { "content": chunk } }
                ]
            });
            let _ = tx
                .send(Ok(Event::default().data(payload.to_string())))
                .await;
        }
        let done = json!({
            "id": "chatcmpl-mock",
            "object": "chat.completion.chunk",
            "model": model,
            "choices": [
                { "index": 0, "delta": {}, "finish_reason": "stop" }
            ]
        });
        let _ = tx.send(Ok(Event::default().data(done.to_string()))).await;
        let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
    });

    Ok(Sse::new(ReceiverStream::new(rx)))
}
