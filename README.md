# animus-plugin-testkit

Conformance test harness, mock CLIs, and benchmark suite for
[Animus](https://github.com/launchapp-dev) **provider plugins**
(v0.4.x stdio JSON-RPC protocol).

This repo lets a plugin author run the same set of scenarios the official
Animus CI runs, locally, without any network, API keys, or a live LLM
account. Every scenario drives the plugin through the same handshake the real
daemon uses and validates the streaming notification + response shape against
[`animus-protocol`](https://github.com/launchapp-dev/animus-protocol).

Status: **v0.1.0**, providers only. Subject/transport/trigger conformance is
on the v0.2.0 roadmap.

## Crates

| Crate                                       | Description                                                         |
| ------------------------------------------- | ------------------------------------------------------------------- |
| `testkit-core`                              | Shared types: `ScenarioFile`, `TestResult`, `MatrixReport`.         |
| `plugin-harness`                            | Bin `animus-plugin-harness` — runs scenarios against a plugin.      |
| `plugin-bench`                              | Bin `animus-plugin-bench` — TTFT, throughput, end-to-end duration.  |
| `provider-conformance`                      | Re-export of the baseline scenarios as a library.                   |
| `mock-cli-claude` / `-codex` / `-gemini` /  | Fake LLM CLIs that emit canonical streams for each scenario.        |
| `mock-cli-opencode`                         |                                                                     |
| `mock-cli-oai`                              | Mock OpenAI-compatible HTTP server for `animus-provider-oai`.       |

## Quickstart

```bash
# 1. Build the workspace (mocks + harness + bench).
cargo build --release --workspace

# 2. Build the plugin you want to test (here: animus-provider-claude).
cd ../animus-provider-claude
cargo build --release
cd -

# 3. Run conformance.
./target/release/animus-plugin-harness \
  conformance \
  --plugin ../animus-provider-claude/target/release/animus-provider-claude

# 4. (Optional) save a machine-readable report.
./target/release/animus-plugin-harness \
  conformance \
  --plugin ../animus-provider-claude/target/release/animus-provider-claude \
  --report-json ./report-claude.json
```

The harness injects `CLAUDE_BIN`, `CODEX_BIN`, `GEMINI_BIN`,
`OPENCODE_BIN`, and `MOCK_SCENARIO` into the plugin's environment before
spawning, so the plugin transparently uses our mock CLIs instead of the real
binaries on `$PATH`. **No network, no API keys.**

## Scenarios

The eight baseline scenarios live in [`scenarios/`](./scenarios/):

| Name                  | What it exercises                                                          |
| --------------------- | -------------------------------------------------------------------------- |
| `streaming-short`     | 3-delta short completion, final aggregated text.                           |
| `streaming-medium`    | ~40 deltas — sanity check buffering.                                       |
| `streaming-long`      | ~300 deltas — back-pressure, large output assembly.                        |
| `tool-call-single`    | One `tool_use` + `tool_result` round-trip surrounded by output.            |
| `tool-call-parallel`  | Two parallel `tool_use` blocks resolved in one envelope.                   |
| `error-recovery`      | Mid-stream garbled line that the provider parser must ignore.              |
| `cancellation`        | **Skipped** in v0.1.0 — see [Known Limitations](#known-limitations).       |
| `resume-session`      | `agent/resume` against a prior session id.                                 |

Plugins that declare `agent/resume` or `agent/cancel` in their `initialize`
capabilities run those scenarios; plugins that don't declare them are
automatically SKIPPED (not failed) for the relevant scenarios.

## Smoke Test (proof the harness works)

Run against the actual `animus-provider-claude` plugin:

```bash
cargo build --release --workspace
cd ../animus-provider-claude && cargo build --release && cd -
./target/release/animus-plugin-harness conformance \
  --plugin ../animus-provider-claude/target/release/animus-provider-claude
```

Output captured 2026-05-23 against `animus-provider-claude v0.2.1` (built
against `animus-protocol v0.1.8`; the testkit is on v0.1.9 and the wire
protocol is 1.0.0 in both):

```text
==> conformance report: animus-provider-claude v0.2.1
    kind: provider   protocol: 1.0.0

  [SKIP]  cancellation                  6ms
        skip: plugin lacks capability `$harness/cancellation-loop-v2`
  [PASS]  error-recovery                189ms
  [PASS]  resume-session                168ms
  [PASS]  streaming-long                183ms
  [PASS]  streaming-medium              171ms
  [PASS]  streaming-short               171ms
  [PASS]  tool-call-parallel            170ms
  [PASS]  tool-call-single              171ms

summary: total 8   passed 7   failed 0   skipped 1
OVERALL: PASS
```

7 PASS, 1 intentional SKIP (cancellation, deferred to v0.2.0 — see
[Known Limitations](#known-limitations)).

## Adding scenarios

Drop a YAML file into `scenarios/` (or your own directory and pass
`--scenarios <dir>`):

```yaml
name: my-scenario
description: ...
timeout_ms: 8000
method: run            # or `resume`
requires_capabilities: [agent/resume]   # optional gate
request:
  prompt: "hello"
  model: claude-sonnet-4-6
expected_notifications:
  - kind: output
    contains: "Hello"
expected_response:
  output_contains: "Hello"
  exit_code: 0
mock:
  tool: claude
  mock_scenario: streaming-short
```

See [`docs/writing-scenarios.md`](./docs/writing-scenarios.md) for the full
matcher surface.

## Benchmarks

```bash
./target/release/animus-plugin-bench \
  --plugin ../animus-provider-claude/target/release/animus-provider-claude \
  --iterations 10 \
  --mock-scenario streaming-medium
```

Reports TTFT (time-to-first-token), end-to-end duration, and notification
throughput. There's also a `criterion` micro-bench for the scenario loader
(`cargo bench -p plugin-bench`).

## CI Integration

See [`docs/ci-integration.md`](./docs/ci-integration.md). The shipped
[`provider-matrix.yml`](.github/workflows/matrix.yml) workflow runs the
harness against every published `launchapp-dev/animus-provider-*` repo every
Monday morning.

## Known Limitations

- **v0.1.0 covers provider plugins only.** Subject, transport, and trigger
  backend conformance is on the v0.2.0 roadmap.
- **`cancellation` scenario is skipped.** `animus-protocol` v0.1.9's
  `agent/cancel` needs a session id captured mid-flight from an `agent/run`.
  The harness's single-request loop can't issue both without a concurrent
  dispatcher. We'll land one in v0.2.0.
- The harness depends only on published `animus-protocol` crates — it
  intentionally has **no dependency** on the in-tree `animus-cli`.

## Versioning

This release is pinned to `animus-protocol v0.1.9`. Protocol bumps in the
patch range remain compatible; minor bumps may require harness changes.

## License

MIT — see [LICENSE](./LICENSE).
