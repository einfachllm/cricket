# Harnesswurm Backend

A lightweight proxy server for monitoring coding-agent telemetry (tokens, cache
usage, tool calls, latency, cost) in real-time, across OpenAI- and
Anthropic-compatible agents.

## Features
- **Proxy Mode**: Intercepts requests to LLM providers — OpenAI-style
  (`/v1/chat/completions`) and Anthropic-style (`/v1/messages`) — and forwards
  them unmodified to the real API.
- **Telemetry**: Extracts token usage (including cache read/write tokens),
  tool call counts, and latency, for both unary and streaming responses.
- **Live agent status**: Derives what each agent session is *doing right now*
  — thinking, running a tool, waiting on you, rate limited, errored — and
  pushes changes to the UI over SSE. See [Agent status](#agent-status) below.
- **Quota tracking**: Records the rate-limit headers both providers send, so
  remaining requests/tokens are visible before a 429 rather than after.
- **Traffic capture**: Stores the full request/response body per call, so you
  can inspect exactly what was sent and returned, not just the counts.
- **Cost estimation**: Prices each call from `pricing.yaml` (per-model $/1M
  tokens). Unpriced models simply show no cost instead of a guessed one.
- **Persistence**: Stores everything in a local SQLite database
  (`harnesswurm.db`, gitignored — it's local state, not something to commit).
- **Multi-Agent Support**: Uses headers to distinguish between different
  agents, sessions, and experiments, so you can compare configurations (e.g.
  the same task with and without a skill enabled) side by side.

## Setup

1. **Install Rust**: Ensure you have `cargo` installed.
2. **Run the Backend**:
   ```bash
   cd backend
   cargo run
   ```
   The server listens on `http://127.0.0.1:8081` by default; override with
   the `BIND_ADDR` env var. State (the database, `agents.yaml`,
   `pricing.yaml`) lives in the current directory.

### Embedding

`backend` is a library (`src/lib.rs`) with a thin binary (`src/main.rs`) on
top, not just a binary — `../desktop/src-tauri` depends on it directly and
spawns `harnesswurm_backend::run(ServerConfig { bind_addr, data_dir })` from
its own startup, so the desktop app is a single process with no separate
`cargo run` needed. Point `data_dir` at wherever makes sense for the embedding
host (a proper per-OS app data directory for a packaged app); `agents.yaml`
and `pricing.yaml` are seeded there from the bundled defaults on first run if
missing, and left alone (still user-editable) after that.

## Usage

Point your agent's OpenAI-compatible client at
`http://localhost:8081/v1/chat/completions`, or its Anthropic-compatible
client at `http://localhost:8081/v1/messages`. Requests are forwarded as-is
to `api.openai.com` / `api.anthropic.com` using whatever `Authorization` /
`x-api-key` / `anthropic-version` headers your client already sends — no
proxy-specific auth setup needed.

### Required/optional headers
- `X-Agent-ID`: Name of the agent (e.g., `kilo`, `opencode`).
- `X-Session-ID`: Unique identifier for the current task/session.
- `X-Experiment-ID` *(optional)*: Groups calls for comparison, e.g. run the
  same task under `X-Experiment-ID: baseline` and again under
  `X-Experiment-ID: with-smart-skills`, then compare their metrics via the
  analytics API or the desktop app's Analytics/Traffic tabs.

### Cost pricing
`pricing.yaml` ships with a small starter set of models. It is **not** an
authoritative price list — verify against your provider's current pricing
page and edit the file to match what you actually use; add or remove models
freely. A model with no entry (or missing input/output prices) just shows no
cost estimate rather than a wrong one.

## Agent status

Different agents (Claude Code, Kilo, opencode, aider, Cursor…) share no
common status protocol, and none of them report to a sidecar. Harnesswurm
therefore *infers* state from the shape of the traffic it already proxies,
which works because every harness runs the same loop underneath.

| What the proxy sees | State shown | Why |
| --- | --- | --- |
| Request forwarded, no response yet | **Thinking** | The agent is blocked on the model |
| …and still nothing after 10 minutes | **Stalled** | Almost certainly hung, not still generating |
| Turn ended with `tool_use` / `tool_calls` | **Running a tool** | The agent went off to do work; it isn't waiting on you |
| …with no follow-up call for 3 minutes | **Idle** | The loop stopped mid-cycle — the agent never came back |
| Turn ended with `end_turn` / `stop` | **Waiting for you** | The agent produced its answer and handed the turn back |
| A question tool was called (`AskUserQuestion`, `ask_followup_question`, …) | **Waiting for you** | Shown with the actual question text |
| HTTP 429 | **Rate limited** | Counts down the provider's own `retry-after`, and stops once it lapses |
| HTTP 503 / 529, or a mid-stream `overloaded_error` | **Provider overloaded** | The agent will usually retry on its own |
| HTTP 4xx/5xx, or the provider was unreachable | **Error** | Labelled by kind — auth failed, request rejected, context full |
| A stream that ended without its terminal event | **Interrupted** | Cancelled, or the connection dropped mid-answer |
| Turn ended with `max_tokens` / `length` | **Hit output limit** | The last answer is truncated |

States that want a human (waiting, rate limited, stalled, error, truncated)
are flagged `needs_attention`, which drives the badge on the sidebar so a
blocked agent is visible from any tab.

Two caveats worth knowing:

- **The states are inferred, not reported.** An agent that stops calling the
  API for its own reasons (crashed, or waiting on a long shell command) is
  indistinguishable from one that finished its tool loop, so it reads as
  *Idle* rather than *Crashed*.
- **Only proxied calls are visible.** An agent talking to a provider
  Harnesswurm doesn't front, or using a subscription rather than API keys,
  contributes nothing.

Calls left open by a killed process are closed out as *Interrupted* on the
next startup, so nothing shows as permanently "Thinking".

### Analytics API
- `GET /v1/analytics/experiments` — list experiments.
- `GET /v1/analytics/experiments/:id/metrics` — metrics for one experiment, over time.
- `GET /v1/analytics/tasks` — most recent calls across all agents (model, provider, tokens, cache tokens, tool calls, latency, cost, call status, and a short preview of what was asked).
- `GET /v1/analytics/tasks/:id/traffic` — the full raw request/response body for one call.
- `GET /v1/analytics/sessions` — one row per agent+session: derived state, totals, spend, and the last question asked.
- `GET /v1/analytics/limits` — current quota per provider, folded from the most recent reading of each header.
- `GET /v1/analytics/events` — SSE feed pinging on every call start and finish, so a dashboard updates immediately instead of on a poll.

### Timing
Each call records both `ttfb_ms` (time to response headers) and `duration_ms`
(until the response was fully consumed). For a streamed call these differ by
the entire generation time, and `duration_ms` is the one that answers "how
long was the agent actually blocked". `metrics.latency_ms` is kept as-is for
backward compatibility and still means time-to-headers.

## Testing

Backend unit tests (parsing, pricing, DB):
```bash
cd backend
cargo test
```

Manual smoke test against a real provider (after adding your API key):
```bash
python3 test_client.py             # OpenAI-style
python3 test_client_anthropic.py   # Anthropic-style
```
