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
- **Run comparison**: Two agents on the same issue, folded to one row each and
  ranked on what solving it actually cost. See [Comparing two agents on the
  same task](#comparing-two-agents-on-the-same-task).

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

## Running it locally

### Just the app (one command)

The desktop app **embeds the backend in its own process** — there is no
separate server to start:

```bash
cd desktop
npm install
npm run tauri dev
```

That opens the window with the proxy already listening on
`127.0.0.1:8081`. State lives in the OS app-data directory (not the current
working directory), and `agents.yaml` / `pricing.yaml` are seeded there on
first run.

To get an app you can just double-click, with no toolchain at all:

```bash
npm run tauri build          # → an installer/bundle in src-tauri/target/release/bundle
```

Both need the Tauri system dependencies for your OS —
https://v2.tauri.app/start/prerequisites/ — plus Rust and Node.

### Backend and UI separately (only for frontend work)

Two terminals is a **development** convenience, not how the app ships: it
gives you Vite's hot reload in a browser without rebuilding the Rust shell.
Use it when iterating on the UI; otherwise use `npm run tauri dev` above.

```bash
cd harnesswurm/backend && cargo run     # http://127.0.0.1:8081
cd desktop && npm run dev               # http://localhost:5173
```

Run `cargo run` from the same directory each time — the standalone binary
keeps its state (`harnesswurm.db`, `agents.yaml`, `pricing.yaml`) in the
current directory, which is a *different* database from the one the packaged
app uses. Don't run both at once: whichever grabs port 8081 first wins, and
the UI will quietly show that one's data.

### See it working — without an API key
```bash
python3 harnesswurm/demo_seed.py --db <path-to-harnesswurm.db>
```
It writes one session per status. Pass `--db` the database the running app is
using — for `cargo run` that's `harnesswurm/backend/harnesswurm.db` (the
default, so `cd harnesswurm/backend && python3 ../demo_seed.py` is enough);
for the packaged app it's under your OS app-data directory, which the app logs
on startup.

Open the Agents view and every state should be visible: thinking, running a
tool, waiting for you, rate limited, auth failed, idle, plus provider quota
bars. `--clear` removes them again — demo rows are prefixed `demo-` and are
never confused with real captured traffic.

It also seeds one experiment, `demo-issue-1284`, with four runs across three
agents — visible under Analytics. It is arranged so that spend alone gives the
wrong answer: the cheapest run is the one that gave up.

Seed *after* the app is running: it closes out open calls on startup, so a
seeded "Thinking" session would otherwise be reaped to *Interrupted* — which
is itself the reaper working correctly.

### Send a real call through the proxy
With an API key, anything that speaks either API works — the proxy forwards
whatever auth headers you already send:
```bash
curl http://localhost:8081/v1/chat/completions \
  -H "Authorization: Bearer $OPENAI_API_KEY" \
  -H "Content-Type: application/json" \
  -H "X-Agent-ID: manual-test" -H "X-Session-ID: my-first-session" \
  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"say hi"}]}'
```
Watch the Agents view while it runs: the session appears as **Thinking**, then
settles to **Waiting for you** once the turn ends. Add `"stream": true` to
exercise the streaming path.

### Point a real coding agent at it
Set the agent's API base URL to the proxy. The two styles differ in how much
of the path the client appends:

| Client style | Base URL to configure |
| --- | --- |
| OpenAI-compatible (appends `/chat/completions`) | `http://localhost:8081/v1` |
| Anthropic-compatible (appends `/v1/messages`) | `http://localhost:8081` |

Most agents expose this as an environment variable or a config field
(`OPENAI_BASE_URL`, `ANTHROPIC_BASE_URL`, a `baseUrl` setting…); the exact name
varies per tool.

**Known gap:** `X-Agent-ID` and `X-Session-ID` are how calls are attributed,
and most agents send neither — those calls all collapse into a single
`unknown_agent` / `default_session` bucket. If your agent can set custom
headers, set them; if not, per-agent attribution isn't available yet.

### Running the tests
```bash
cd harnesswurm/backend && cargo test     # parsing, pricing, DB, state rules
cd desktop && npm test                   # UI, and npm run typecheck
```

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

## Comparing two agents on the same task

Point both agents at the proxy with the **same** `X-Experiment-ID` and a
different `X-Agent-ID` / `X-Session-ID`:

```bash
# terminal 1
X-Agent-ID: kilo         X-Session-ID: issue-1284-kilo    X-Experiment-ID: issue-1284
# terminal 2
X-Agent-ID: claude-code  X-Session-ID: issue-1284-claude  X-Experiment-ID: issue-1284
```

Open **Analytics → the experiment**. Each agent+session becomes one *run*, and
the table shows what each spent, in tokens, calls, tool calls and wall clock.

### Cost alone answers the wrong question

The proxy sees what a run cost. It cannot see whether the diff works — so
ranking on spend rewards the agent that gave up after two calls, which is
reliably the cheapest run of a hard task. Mark each run **Solved** or
**Failed** yourself (the toggle in the table); only solved runs are ranked,
and until at least one is marked, the view declines to name a winner.

Three further things the headline number would otherwise hide:

- **Unpriced models.** A model with no entry in `pricing.yaml` contributes
  $0.00, so a run that touched one shows as `≥ $x` and is kept out of the
  ranking rather than winning on an understated total.
- **Runs still going.** A run with a call still open shows `$x so far` and is
  not ranked against runs that have finished spending.
- **One run each is a sample size of one.** The **Per agent** roll-up divides
  everything an agent spent by the runs it solved, so an agent that lands one
  attempt in three is charged for all three. Run the same task a few times per
  agent and that number, not the cheapest single run, is the one to trust.

### Analytics API
- `GET /v1/analytics/experiments` — list experiments.
- `GET /v1/analytics/experiments/:id/metrics` — metrics for one experiment, over time.
- `GET /v1/analytics/experiments/:id/comparison` — one row per run (agent +
  session) in the experiment: totals for cost, tokens, cache reads, tool
  calls, wall clock, failed/rate-limited/still-open calls, and the verdict.
- `PUT /v1/analytics/sessions/verdict` — mark a run solved or failed:
  `{"agent_name": "kilo", "session_id": "issue-1284-kilo", "verdict": "solved", "note": "tests pass"}`.
  A `verdict` of `null` clears it.
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
