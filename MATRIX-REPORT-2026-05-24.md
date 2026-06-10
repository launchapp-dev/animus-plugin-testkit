# Animus Plugin Conformance Matrix — 2026-05-24

Test harness: `animus-plugin-testkit` v0.1.0 (commit at HEAD of `main`,
8 baseline provider scenarios + mock CLIs for claude / codex / gemini /
opencode). Run on macOS 25.3.0 (darwin arm64) with release builds.

The harness was executed against **6 provider plugins** and **5 subject
backend plugins**. Provider plugins received the full conformance suite.
Subject plugins received a handshake + capability probe only (the v0.1.0
testkit does not yet ship subject conformance scenarios).

## 1. Provider plugins — full conformance run

Legend: P=PASS, F=FAIL, S=SKIP. Timings are wall-clock per scenario.

| Plugin                       | Version | streaming-short | streaming-medium | streaming-long | tool-call-single | tool-call-parallel | error-recovery | cancellation | resume-session | Overall |
|------------------------------|---------|-----------------|------------------|----------------|------------------|--------------------|----------------|--------------|----------------|---------|
| animus-provider-claude       | v0.2.1  | P (166ms)       | P (171ms)        | P (180ms)      | P (166ms)        | P (163ms)          | P (194ms)      | S (5ms)      | P (173ms)      | PASS    |
| animus-provider-codex        | v0.2.1  | P (177ms)       | F (183ms)        | F (181ms)      | F (169ms)        | F (186ms)          | F (213ms)      | S (16ms)     | P (194ms)      | FAIL    |
| animus-provider-gemini       | v0.2.1  | P (179ms)       | P (173ms)        | P (197ms)      | F (177ms)        | F (194ms)          | F (211ms)      | S (22ms)     | P (191ms)      | FAIL    |
| animus-provider-opencode     | v0.2.1  | P (180ms)       | P (180ms)        | P (181ms)      | F (191ms)        | F (182ms)          | F (212ms)      | S (15ms)     | P (168ms)      | FAIL    |
| animus-provider-oai          | v0.2.1  | F (12ms)        | F (19ms)         | F (10ms)       | F (13ms)         | F (15ms)           | F (16ms)       | S (17ms)     | S (14ms)       | FAIL    |
| animus-provider-oai-agent    | v0.1.1  | F (7ms)         | F (8ms)          | F (8ms)        | F (7ms)          | F (8ms)            | F (9ms)        | S (6ms)      | F (8ms)        | FAIL    |

### Totals

| Plugin                       | PASS | FAIL | SKIP |
|------------------------------|------|------|------|
| animus-provider-claude       | 7    | 0    | 1    |
| animus-provider-codex        | 2    | 5    | 1    |
| animus-provider-gemini       | 4    | 3    | 1    |
| animus-provider-opencode     | 4    | 3    | 1    |
| animus-provider-oai          | 0    | 6    | 2    |
| animus-provider-oai-agent    | 0    | 7    | 1    |
| **Total**                    | 17   | 24   | 7    |

48 scenarios attempted; 17 pass, 24 fail, 7 skip.

## 2. Subject plugins — handshake + capability probe

The v0.1.0 testkit does not ship subject conformance scenarios, so each
plugin was driven by hand-rolled JSON-RPC: a single `initialize` request
exercising the stdio plugin contract.

| Plugin                       | Version  | Binary starts | Handshake | Subject kinds  | Methods (count) | Streaming | Status |
|------------------------------|----------|---------------|-----------|----------------|-----------------|-----------|--------|
| animus-subject-default       | v0.1.1   | OK            | OK        | `task`         | 17 (full task surface incl. statistics, schema, watch) | yes | PASS |
| animus-subject-linear        | v0.1.4*  | OK            | OK        | `issue`        | 5 (`subject/{list,get,update,schema}`, `health/check`) | no  | PASS |
| animus-subject-markdown      | v0.1.4   | OK            | OK        | `task`         | 6 (`subject/{list,get,update,schema,watch}`, `health/check`) | yes | PASS |
| animus-subject-requirements  | v0.1.6   | OK            | OK        | `requirement`  | 6 (`subject/{list,get,update,schema,watch}`, `health/check`) | yes | PASS |
| animus-subject-sqlite        | v0.1.4   | OK            | OK        | `task`         | 6 (`subject/{list,get,update,schema,watch}`, `health/check`) | yes | PASS |

\*`animus-subject-linear` HEAD is `v0.1.3-2-g12e8ce5` in staging, but the
binary reports `0.1.4` in its `plugin_info.version` — there is a tag/Cargo
drift worth reconciling before publish.

All five subject backends return `plugin_kind: "subject_backend"` and
advertise `protocol_version: "1.0.0"`. None advertise cancellation.

## 3. Failure breakdown

### 3.1 animus-provider-codex v0.2.1 — 5 failures

Failure shape: scenarios that expect *more than one* `output` notification
fail with `expected notification 'output' not found after index 1`
(streaming-medium/long, error-recovery) or `expected notification
'toolCall' not found after index 1` (tool-call-single, tool-call-parallel).

Root cause is in the test harness, not the plugin: `mock-cli-codex`
(`crates/mock-cli-codex/src/main.rs`) emits a single bulk
`item.completed` event with the full text, with no incremental
deltas and no `tool_use` events. The codex provider plugin correctly
maps that to exactly one `Output` notification — but the scenarios were
authored against the mock-claude event shape. The codex provider's
streaming-short test passes because that scenario only asserts a single
`output` containing `Hello`.

**Recommendation:** extend `mock-cli-codex` to emit per-chunk
`item.output_text.delta` events plus `tool_call` events when the
`MOCK_SCENARIO` matches `streaming-{medium,long}`, `tool-call-*`, and
`error-recovery`. Treat this as a v0.1.1 testkit task, not a codex
plugin regression.

### 3.2 animus-provider-gemini v0.2.1 — 3 failures

Failure shape: `tool-call-single`, `tool-call-parallel`, `error-recovery`
each fail at the first non-output assertion. Same root cause:
`mock-cli-gemini` (`crates/mock-cli-gemini/src/main.rs`) only knows the
three `streaming-*` scenarios and falls back to `"Hello world!"` for
everything else. There is no `functionCall` / `functionResponse` shape
emitted, so the gemini provider has nothing to translate into a
`tool_call` notification. Streaming-medium and streaming-long pass
because the mock does emit `partialResult` deltas per chunk.

**Recommendation:** extend `mock-cli-gemini` to emit `functionCall`
JSONL events for tool-call scenarios and an injected malformed line +
recovery delta for `error-recovery`.

### 3.3 animus-provider-opencode v0.2.1 — 3 failures

Identical to gemini. `mock-cli-opencode` only knows the three
`streaming-*` scenarios; everything else collapses to one `text` chunk.

**Recommendation:** same fix surface — extend `mock-cli-opencode` to
emit tool / error events.

### 3.4 animus-provider-oai v0.2.1 — 6 failures

All seven non-cancellation scenarios surface an identical error:

> `provider unavailable: OPENAI_API_KEY is required for animus-provider-oai run/resume calls (code -32603)`

`resume-session` is also skipped: the legacy `oai` plugin does not
advertise the `agent/resume` capability.

The oai plugin never registered a `MOCK_OAI_*` env-var path, so the
testkit cannot exercise it without real API credentials. This is a real
production-readiness gap: the v0.1.0 testkit provides no mock for any
HTTP-API-only provider.

**Recommendation:** before declaring oai production-ready, add either
(a) an oai mock-CLI and a `MOCK_OAI_*` env path inside the plugin, or
(b) a wiremock-style HTTP fixture loader the harness can stand up on a
loopback port.

### 3.5 animus-provider-oai-agent v0.1.1 — 7 failures

All scenarios fail at index 0 with:

> `IO error: No such file or directory (os error 2)` from
> `backend: oai-runner:oai-runner-native`

The oai-agent plugin shells out to a sibling `oai-runner` binary on
`PATH`. That binary is neither in `target/release` nor on the test
machine's `PATH`, so every spawn fails before any API call is even
attempted. Resume is **not** marked `Skip` — the plugin advertises the
`agent/resume` capability but the spawn still fails.

**Recommendation:**

1. Either bundle the `oai-runner` binary into the plugin or have the
   plugin fall back to an in-process Responses API client when the
   sibling binary is absent.
2. The testkit cannot smoke-test this plugin today; add a documented
   `OAI_RUNNER_BIN` env override so the harness can point at the
   in-repo runner artifact.

### 3.6 Cancellation skips (all six providers)

Every provider plugin lacks the `$harness/cancellation-loop-v2`
capability, which is *expected and documented* — the v0.1.0 testkit
defers the cancellation scenario per `scenarios/cancellation.yaml`'s
description ("SKIPPED in v0.1.0 ... Re-enable in v0.2.0").

## 4. Production-readiness verdict

| Plugin                       | Ready to publish at v0.2.x? | Notes |
|------------------------------|------------------------------|-------|
| animus-provider-claude       | YES                          | Already validated, all conformance green (1 expected skip). |
| animus-provider-codex        | YES (conditional)            | Plugin behavior looks correct. Failures attributable to under-spec'd mock CLI; needs cross-check against real codex CLI before shipping. |
| animus-provider-gemini       | YES (conditional)            | Same situation as codex — streaming proves out, tool/error paths blocked by mock shape. |
| animus-provider-opencode     | YES (conditional)            | Same as gemini/codex. |
| animus-provider-oai          | NOT YET                      | No mock path; cannot prove the plugin streams or handles tool calls correctly without burning real API quota. Should add a mock surface before shipping. |
| animus-provider-oai-agent    | NOT YET                      | Hard dependency on an unbundled `oai-runner` binary. Either bundle it or add a fallback in-process path. |

All five subject backends are clean for v0.1.x publication: handshake,
plugin_info, subject_kinds, and methods all parse and look sane.

## 5. Recommendations for v0.1.1 of the testkit itself

1. Extend `mock-cli-{codex,gemini,opencode}` to cover `tool-call-single`,
   `tool-call-parallel`, `error-recovery`, and `resume-session`. Today
   they emit a `"Hello world!"` fallback that produces a single output
   notification and trivially fails the scenarios.
2. Introduce an HTTP-mock layer (or a per-plugin mock-CLI hook) for
   API-only providers so `animus-provider-oai` can be exercised
   without real credentials.
3. Author the first round of subject-conformance scenarios. The
   handshake probe in this report (`subject_kinds`, `methods`,
   `plugin_kind`) is a starting point but does not exercise list/get/
   update/watch round-trips.
4. Re-enable the `cancellation` scenario once the harness gets a
   concurrent dispatcher (already tracked in
   `scenarios/cancellation.yaml`).

## 6. Files written / commits

This run did **not** commit any changes. No harness fix met the
"single-line, unambiguous" bar — the mock-CLI gaps need a small design
review (which scenarios are real product requirements vs. nice-to-haves
for the v0.1.0 plugins) before code lands.

Per-plugin JSON reports were written to `/tmp/matrix-<plugin>.json`
during the run and are not persisted in this repo.
