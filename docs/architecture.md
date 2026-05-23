# Architecture

```
┌────────────────────┐         stdin (JSON-RPC 2.0)
│ animus-plugin-     │ ────────────────────────────► ┌────────────────────┐
│   harness          │                                 │  plugin binary     │
│                    │ ◄──────────────────────────── │  (e.g. animus-     │
│  - spawns plugin   │         stdout (JSON-RPC 2.0)  │   provider-claude) │
│  - drives scenarios│                                 │                    │
│  - validates wire  │                                 │  CLAUDE_BIN env →  │
└────────────────────┘                                 │  mock-claude       │
        │                                              └─────────┬──────────┘
        │ spawns                                                  │ stdout
        ▼                                                         ▼
┌────────────────────┐                                  ┌────────────────────┐
│  mock-claude /     │ ─── canonical SSE / JSONL ────► │  session-backend   │
│  mock-codex /      │                                  │  parser inside     │
│  mock-gemini /     │                                  │  plugin            │
│  mock-opencode /   │                                  └────────────────────┘
│  mock-oai (HTTP)   │
└────────────────────┘
```

## Pieces

- **`testkit-core`** — declarative types only. Loads `*.yaml`, holds in-memory
  scenarios, defines `TestResult` and `MatrixReport`.
- **`plugin-harness`** — async stdio JSON-RPC client. Performs `initialize`/
  `initialized` handshake, then per scenario:
  1. Sends `agent/run` (or `agent/resume`) with the scenario's request shape.
  2. Reads frames from stdout, decoding `agent/output`, `agent/thinking`,
     `agent/toolCall`, `agent/toolResult`, `agent/error` notifications back
     into the `AgentNotification` enum from `animus-provider-protocol`.
  3. Captures the terminal `RpcResponse` and validates against the scenario's
     `expected_notifications` (in-order subsequence) and
     `expected_response`.
- **`plugin-bench`** — a thin one-shot benchmark loop that measures TTFT,
  end-to-end duration, and throughput. Uses the same JSON-RPC client wiring.
- **`mock-cli-*`** — six tiny crates that mimic the real CLIs (`claude`,
  `codex`, `gemini`, `opencode`) and the OAI HTTP surface. Each picks its
  output script from `MOCK_SCENARIO`.
- **`provider-conformance`** — a `lib`-only crate that compiles the baseline
  scenarios into a downstream-friendly `baseline_scenarios()` function. A
  plugin repo can depend on this as a dev-dep and run the harness from its
  own integration tests.

## Why a single-process JSON-RPC client?

v0.1.0's scope is "every plugin sees the same wire conversation the daemon
would have sent it." We deliberately do **not** reuse any
`animus-cli`-internal scheduler — that keeps the harness honest. If our
serializer can drive a Rust plugin, it can drive a Python or TypeScript
plugin too.

## Why mock CLIs instead of fixture replay?

Provider plugins are the boundary at which "JSONL on stdout" meets "fragile
parser." A fixture replay would only test the parser; a fake CLI also tests
the spawn lifecycle and the env-var override surface (`CLAUDE_BIN`, etc.).
That said, v0.2.0 plans to add fixture replay alongside the mocks for
parser-only regression testing.
