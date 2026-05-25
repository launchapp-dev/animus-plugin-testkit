# animus-plugin-testkit

Conformance test harness, mock CLIs, and benchmark suite for
[Animus](https://github.com/launchapp-dev) plugins
(v0.4.x stdio JSON-RPC protocol).

This repo lets a plugin author run the same set of scenarios the official
Animus CI runs, locally, without any network, API keys, or a live LLM
account. Every scenario drives the plugin through the same handshake the real
daemon uses and validates the streaming notification + response shape against
[`animus-protocol`](https://github.com/launchapp-dev/animus-protocol).

Status: **v0.3.0** — provider, subject, transport, and trigger plugin
conformance suites; concurrent-cancel dispatcher; oai-style scenario variants.

## Crates

| Crate                                       | Description                                                         |
| ------------------------------------------- | ------------------------------------------------------------------- |
| `testkit-core`                              | Shared types: `ScenarioFile`, `TestResult`, `MatrixReport`.         |
| `plugin-harness`                            | Bin `animus-plugin-harness` — runs scenarios against a plugin.      |
| `plugin-bench`                              | Bin `animus-plugin-bench` — TTFT, throughput, end-to-end duration.  |
| `provider-conformance`                      | Baseline scenarios for provider plugins (10 scenarios).             |
| `subject-conformance`                       | Baseline scenarios for subject backend plugins (5 scenarios).       |
| `transport-conformance`                     | Baseline scenarios for transport backend plugins (4 scenarios).    |
| `trigger-conformance`                       | Baseline scenarios for trigger backend plugins (3 scenarios).      |
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

# 3. Run conformance — provider (default), subject, transport, or trigger.
./target/release/animus-plugin-harness conformance \
  --plugin ../animus-provider-claude/target/release/animus-provider-claude

./target/release/animus-plugin-harness conformance --kind subject \
  --plugin ../animus-subject-default/target/release/animus-subject-default

./target/release/animus-plugin-harness conformance --kind transport \
  --plugin ../animus-transport-http/target/release/animus-transport-http

./target/release/animus-plugin-harness conformance --kind trigger \
  --plugin ../animus-trigger-webhook/target/release/animus-trigger-webhook

# 4. (Optional) save a machine-readable report.
./target/release/animus-plugin-harness conformance \
  --plugin ../animus-provider-claude/target/release/animus-provider-claude \
  --report-json ./report-claude.json
```

The harness injects `CLAUDE_BIN`, `CODEX_BIN`, `GEMINI_BIN`,
`OPENCODE_BIN`, and `MOCK_SCENARIO` into the plugin's environment before
spawning, so the provider plugin transparently uses our mock CLIs instead
of the real binaries on `$PATH`. **No network, no API keys.**

## Provider scenarios

The 10 baseline scenarios live in [`scenarios/`](./scenarios/):

| Name                       | What it exercises                                                          |
| -------------------------- | -------------------------------------------------------------------------- |
| `streaming-short`          | 3-delta short completion, final aggregated text.                           |
| `streaming-medium`         | ~40 deltas — sanity check buffering.                                       |
| `streaming-long`           | ~300 deltas — back-pressure, large output assembly.                        |
| `tool-call-single`         | One `tool_use` + `tool_result` round-trip surrounded by output.            |
| `tool-call-parallel`       | Two parallel `tool_use` blocks resolved in one envelope.                   |
| `tool-call-single-oai`     | Stateless OpenAI-style: `ToolCall` only, host owns tool execution.         |
| `tool-call-parallel-oai`   | Same as above, parallel.                                                   |
| `error-recovery`           | Mid-stream garbled line that the provider parser must ignore.              |
| `cancellation`             | Concurrent dispatcher issues `agent/cancel` mid-flight (see below).        |
| `resume-session`           | `agent/resume` against a prior session id.                                 |

Plugins that don't advertise the relevant `$harness/*` capability for a
scenario are SKIPPED (not failed). Plugins opt in by adding to their
`initialize` capabilities:

- `$harness/cancellation-loop-v2` — opt-in to the v0.3.0 concurrent-cancel test
- `$harness/oai-style` — opt-in to the stateless OpenAI tool-call scenarios

## Cancellation: concurrent dispatcher (v0.3.0)

The harness now spawns a side-task per scenario when `cancel_after_ms` is
set. It watches for the first notification to learn the session id, waits
the configured delay, then issues `agent/cancel { session_id }` via the
same stdio pipe. The plugin should terminate the run with
`BackendError::Cancelled` (`REQUEST_CANCELLED`, `-32002`) or emit a
non-recoverable `error` notification within the scenario timeout.

A `fake-cancellable-plugin` test fixture lives at
`crates/plugin-harness/src/bin/fake_cancellable_plugin.rs` and is
exercised by `crates/plugin-harness/tests/cancellation.rs` to verify the
wire dance end-to-end without depending on any real provider.

## Subject / transport / trigger conformance (v0.3.0)

Each is a separate crate that exports `pub fn baseline_scenarios() ->
Vec<TestScenario>` plus a `pub async fn run_conformance(plugin_path:
&Path) -> Result<MatrixReport>`. External CI pipelines can depend on the
crate directly:

```toml
[dev-dependencies]
subject-conformance = { git = "https://github.com/launchapp-dev/animus-plugin-testkit", tag = "v0.3.0" }
```

| Suite     | Scenarios                                                              |
| --------- | ---------------------------------------------------------------------- |
| Subject   | `handshake`, `advertise-kinds`, `subject-list`, `subject-crud-round-trip`, `subject-watch-stream` |
| Transport | `handshake`, `start-shutdown`, `schema-health`, `serve-and-accept`     |
| Trigger   | `handshake`, `watch-fires-event`, `event-payload-shape`                |

Trigger backends that need an external stimulus (a webhook POST, a Slack
message, a cron tick) will SKIP `watch-fires-event` and
`event-payload-shape`. The handshake still PASSes.

## Smoke tests (proof the harness works)

Captured 2026-05-24 against v0.3.0:

```text
$ animus-plugin-harness conformance --kind subject \
  --plugin animus-subject-default/target/release/animus-subject-default

==> conformance report: animus-subject-default v0.1.1
    kind: subject_backend   protocol: 1.0.0
  [PASS]  handshake                     0ms
  [PASS]  advertise-kinds               0ms
  [PASS]  subject-list                  8ms
  [PASS]  subject-crud-round-trip       8ms
  [PASS]  subject-watch-stream          8ms
summary: total 5   passed 5   failed 0   skipped 0
OVERALL: PASS
```

```text
$ animus-plugin-harness conformance --kind transport \
  --plugin animus-transport-http/target/release/animus-transport-http

==> conformance report: animus-transport-http v0.1.0
    kind: transport_backend   protocol: 1.0.0
  [PASS]  handshake                     0ms
  [PASS]  start-shutdown                6ms
  [PASS]  schema-health                 5ms
  [PASS]  serve-and-accept             83ms
summary: total 4   passed 4   failed 0   skipped 0
OVERALL: PASS
```

```text
$ animus-plugin-harness conformance --kind trigger \
  --plugin animus-trigger-webhook/target/release/animus-trigger-webhook

==> conformance report: animus-trigger-webhook v0.1.1
    kind: trigger_backend   protocol: 1.0.0
  [PASS]  handshake                     0ms
  [SKIP]  watch-fires-event             0ms
  [SKIP]  event-payload-shape           0ms
summary: total 3   passed 1   failed 0   skipped 2
OVERALL: PASS
```

```text
$ animus-plugin-harness conformance \
  --plugin animus-provider-claude/target/release/animus-provider-claude

==> conformance report: animus-provider-claude v0.2.1
    kind: provider   protocol: 1.0.0
  [SKIP]  cancellation                 34ms
  [PASS]  error-recovery              409ms
  [PASS]  resume-session              395ms
  [PASS]  streaming-long              402ms
  [PASS]  streaming-medium            391ms
  [PASS]  streaming-short             406ms
  [PASS]  tool-call-parallel          399ms
  [SKIP]  tool-call-parallel-oai       34ms
  [PASS]  tool-call-single            374ms
  [SKIP]  tool-call-single-oai         33ms
summary: total 10   passed 7   failed 0   skipped 3
OVERALL: PASS
```

The 3 SKIPs are expected: animus-provider-claude does not advertise the
opt-in capabilities `$harness/cancellation-loop-v2` or
`$harness/oai-style`. Provider plugins that want those tests to run
should advertise those capabilities in their `initialize` capabilities
list once their backend supports the relevant semantics.

## Adding scenarios

Drop a YAML file into `scenarios/` (or your own directory and pass
`--scenarios <dir>`):

```yaml
name: my-scenario
description: ...
timeout_ms: 8000
method: run            # or `resume`
requires_capabilities: ["agent/resume"]   # optional gate
cancel_after_ms: 100   # optional — triggers the concurrent-cancel dispatcher
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

- **Trigger conformance is shallow without an external stimulus.** The
  `webhook` backend (and any backend that needs an inbound HTTP POST,
  Slack event, cron tick, ...) will SKIP `watch-fires-event` and
  `event-payload-shape`. A future revision could spin up a stimulus
  injector per backend kind.
- **Subject CRUD requires create+get to share state.** The harness keeps a
  single plugin process alive across the round-trip so backends with
  in-process state (the default task store) can be exercised. Backends
  that persist to a global path may surface cross-test contamination.
- **Provider plugins must advertise opt-in capabilities** for the new
  cancellation + oai-style tests to run. Plugins that don't are SKIPPED
  (not failed). Update each plugin's `initialize` capabilities to opt in.
- The harness depends only on published `animus-protocol` crates — it
  intentionally has **no dependency** on the in-tree `animus-cli`.

## Versioning

This release is pinned to `animus-protocol v0.1.9`. Protocol bumps in the
patch range remain compatible; minor bumps may require harness changes.

## License

MIT — see [LICENSE](./LICENSE).
