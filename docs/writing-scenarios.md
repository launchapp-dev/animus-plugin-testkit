# Writing Scenarios

A scenario is a YAML file with this shape:

```yaml
name: string                # required, unique
description: string         # optional, shown in `list-scenarios`
timeout_ms: integer         # default 10000
method: run | resume        # default run
requires_capabilities:      # optional; SKIP if plugin lacks any
  - agent/resume
skip_if_capabilities:       # optional; SKIP if plugin advertises any
  - "$harness/no-tool-events"
request:                    # required
  prompt: string            # required
  model: string             # optional
  system_prompt: string     # optional
  cwd: string               # optional, default "."
  session_id: string        # required for `method: resume`
  env:                      # optional, k/v map injected as `request.env`
    KEY: VALUE
expected_notifications:     # optional, in-order subsequence match
  - kind: output            # variants: output / thinking / tool_call / tool_result / error
    contains: string        # substring match
  - kind: tool_call
    name: string            # tool name match
  - kind: error
    recoverable: bool
allow_extra_notifications: bool   # default false; trailing notifs are still tolerated
expected_response:          # optional
  output_contains: string
  min_output_len: integer
  exit_code: integer
mock:                       # optional but recommended
  tool: claude | codex | gemini | opencode | oai
  mock_scenario: string     # passed as MOCK_SCENARIO env to the mock CLI
```

## Matchers

`expected_notifications` is matched as a **subsequence** — every entry must
appear in order, but other notifications between them are ignored. This
matches how real LLM streams work (lots of small deltas, occasional tool
calls).

`expected_response` checks the final `AgentRunResponse`:

- `output_contains` — substring search.
- `min_output_len` — `output.len() >= n`.
- `exit_code` — exact match.

## Capability gating

Plugins declare the methods they support during `initialize`. If a scenario
sets `requires_capabilities: [agent/resume]` and the plugin doesn't list
`agent/resume`, the scenario is **skipped**, not failed. This keeps the
matrix honest for read-only providers.

If a scenario sets `skip_if_capabilities`, the harness skips it when the
plugin advertises any listed capability. Use this for mutually exclusive
harness modes such as stateless OAI providers versus tool-result providers.

## Adding a new mock scenario

If your scenario needs a new canonical stream, add a branch to the relevant
mock CLI (e.g. `crates/mock-cli-claude/src/main.rs`) and point the scenario
at it via `mock.mock_scenario: my-new-name`. Re-run `cargo build --release`.

## Discovery order

`plugin-harness conformance` sorts scenarios alphabetically by file name.
Use `--only <name>` to run just one. Use `--scenarios <dir>` to point at a
private set instead of the bundled baseline.
