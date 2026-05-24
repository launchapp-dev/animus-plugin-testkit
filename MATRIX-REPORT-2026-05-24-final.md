# Animus Plugin Conformance Matrix — Final (2026-05-24)

Re-run of the production-readiness matrix after `animus-protocol v0.1.11`
shipped tool-call parser fixes for codex / gemini / opencode and the
three affected provider plugins were tagged at `v0.2.2`.

Test harness: `animus-plugin-testkit` v0.2.0 (HEAD of `main`,
commit `ee80498` — scenario-coverage parity across all 5 mock CLIs).

8 baseline provider scenarios were re-executed against **6 provider plugins**
at the following tags:

| Plugin                       | Tag    | Commit SHA                                  |
| ---------------------------- | ------ | ------------------------------------------- |
| animus-provider-claude       | v0.2.1 | `e6ef6a462e909c24aff4fbdda34b26403047405d` |
| animus-provider-codex        | v0.2.2 | `a730355c963407e4aeef4729b8aecd0d8699b0b4` |
| animus-provider-gemini       | v0.2.2 | `5d940f63c3b379252d36fe488db3564328560c05` |
| animus-provider-opencode     | v0.2.2 | `890b98c2f9fcfa64cb645fe78c9466119ba2806c` |
| animus-provider-oai          | v0.2.1 | `27c45abbd9da32b0d1f53c2fa76ac5fa1e4b96ee` |
| animus-provider-oai-agent    | v0.1.2 | `8fb6ea7d64ef6947e8c1800d9430a793d18f6294` |

## 1. Provider conformance — final matrix

Legend: P=PASS, F=FAIL, S=SKIP. Timings are wall-clock per scenario.

| Plugin                       | Version | cancellation | error-recovery | resume-session | streaming-long | streaming-medium | streaming-short | tool-call-parallel | tool-call-single | Overall |
| ---------------------------- | ------- | ------------ | -------------- | -------------- | -------------- | ---------------- | --------------- | ------------------ | ---------------- | ------- |
| animus-provider-claude       | v0.2.1  | S (33ms)     | P (224ms)      | P (199ms)      | P (210ms)      | P (198ms)        | P (199ms)       | P (200ms)          | P (199ms)        | PASS    |
| animus-provider-codex        | v0.2.2  | S (34ms)     | P (225ms)      | P (200ms)      | P (212ms)      | P (203ms)        | P (242ms)       | P (202ms)          | P (198ms)        | PASS    |
| animus-provider-gemini       | v0.2.2  | S (33ms)     | P (224ms)      | P (198ms)      | P (209ms)      | P (199ms)        | P (197ms)       | P (201ms)          | P (201ms)        | PASS    |
| animus-provider-opencode     | v0.2.2  | S (33ms)     | P (230ms)      | P (197ms)      | P (214ms)      | P (203ms)        | P (201ms)       | P (200ms)          | P (200ms)        | PASS    |
| animus-provider-oai          | v0.2.1  | S (35ms)     | P (36ms)       | S (35ms)       | P (49ms)       | P (36ms)         | P (34ms)        | F (36ms)           | F (34ms)         | FAIL    |
| animus-provider-oai-agent    | v0.1.2  | S (32ms)     | F (597ms)      | P (77ms)       | F (82ms)       | F (77ms)         | P (75ms)        | F (75ms)           | F (75ms)         | FAIL    |

### Per-plugin tally (P / F / S)

| Plugin                       | PASS | FAIL | SKIP |
| ---------------------------- | ---- | ---- | ---- |
| animus-provider-claude       | 7    | 0    | 1    |
| animus-provider-codex        | 7    | 0    | 1    |
| animus-provider-gemini       | 7    | 0    | 1    |
| animus-provider-opencode     | 7    | 0    | 1    |
| animus-provider-oai          | 4    | 2    | 2    |
| animus-provider-oai-agent    | 2    | 5    | 1    |
| **Total (48 scenarios)**     | **34** | **7** | **7** |

## 2. Comparison to 2026-05-24 baseline

The original matrix run (see `MATRIX-REPORT-2026-05-24.md`) totalled
**17 PASS / 24 FAIL / 7 SKIP** over the same 48 scenarios.

| Metric         | Baseline (2026-05-24) | Final (post-v0.1.11) | Delta            |
| -------------- | --------------------- | -------------------- | ---------------- |
| PASS           | 17                    | 34                   | **+17** (+100%)  |
| FAIL           | 24                    | 7                    | **-17** (-70.8%) |
| SKIP           | 7                     | 7                    |  0               |
| Overall PASS rate | 35.4 %             | **70.8 %**           | **+35.4 pp**     |

### Tool-call scenarios specifically (the v0.1.11 fix target)

| Plugin    | Baseline tool-call-single | Final | Baseline tool-call-parallel | Final |
| --------- | ------------------------- | ----- | --------------------------- | ----- |
| claude    | PASS                      | PASS  | PASS                        | PASS  |
| codex     | FAIL                      | **PASS** | FAIL                     | **PASS** |
| gemini    | FAIL                      | **PASS** | FAIL                     | **PASS** |
| opencode  | FAIL                      | **PASS** | FAIL                     | **PASS** |
| oai       | FAIL                      | FAIL  | FAIL                        | FAIL  |
| oai-agent | FAIL                      | FAIL  | FAIL                        | FAIL  |

Tool-call PASS count: 2 / 12 → **8 / 12** (+6 PASSes, +50 pp).

For the three plugins targeted by the parser fix (codex / gemini /
opencode) tool-call coverage moved from **0 / 6 → 6 / 6** — a complete
close-out of the gap.

## 3. Remaining gaps

### `animus-provider-oai` (4P / 2F / 2S)

`tool-call-{single,parallel}` still fail with:

```
expected notification `toolResult` not found after index N
```

Architectural — the legacy `oai` provider drives the OpenAI HTTP API
directly and does not emit `toolResult` notifications between
`toolCall` and the next `output`. The scenario assertions assume the
CLI-style flow used by claude / codex / gemini / opencode. **Next step:
either (a) author an `oai` scenario variant whose tool-call expectation
stops at `toolCall` + final `output`, or (b) teach the oai plugin to
synthesise a `toolResult` for each call.** Resume is also legitimately
unsupported (`agent/resume` capability not advertised) — that SKIP is
expected.

### `animus-provider-oai-agent` (2P / 5F / 1S)

Five FAILs all share the same shape: scenarios expect multiple
`output` notifications but only one arrives, and tool-call scenarios
expect `toolCall` after index 3 but the agent surface terminates
early. This is the same class of architectural mismatch as `oai` (HTTP
agent surface vs CLI streaming wire). The oai-agent variant of the
scenarios was not in scope for v0.1.11 and is unaffected by the parser
fix. **Next step: same as oai — ship scenario variants or extend the
plugin to emit the expected notifications.**

### `cancellation` (SKIP across all plugins)

All six plugins skip `cancellation` with
`plugin lacks capability '$harness/cancellation-loop-v2'`. This is the
intended path until plugins opt in to the v2 cancellation loop. No
action required for v0.1.11.

## 4. Verdict

- The v0.1.11 parser fix closed the codex / gemini / opencode tool-call
  gap completely (0 / 6 → 6 / 6 PASS on the relevant scenarios).
- Overall matrix health doubled: 17 → 34 PASS / 24 → 7 FAIL.
- The four CLI-style provider plugins (claude / codex / gemini /
  opencode) are now uniformly **PASS** on every non-cancellation
  scenario.
- The two HTTP-style plugins (oai, oai-agent) remain on the
  known-architectural backlog described above.

## 5. Artifacts

Per-plugin JSON reports written to
`/tmp/matrix-reports-2026-05-24-final/`:

- `claude.json`
- `codex.json`
- `gemini.json`
- `opencode.json`
- `oai.json`
- `oai-agent.json`
