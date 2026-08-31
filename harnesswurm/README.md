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
   the `BIND_ADDR` env var.

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

### Analytics API
- `GET /v1/analytics/experiments` — list experiments.
- `GET /v1/analytics/experiments/:id/metrics` — metrics for one experiment, over time.
- `GET /v1/analytics/tasks` — most recent calls across all agents (model, provider, tokens, cache tokens, tool calls, latency, cost, and a short preview of what was asked).
- `GET /v1/analytics/tasks/:id/traffic` — the full raw request/response body for one call.

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
