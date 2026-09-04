# Harnesswurm Backend

A lightweight proxy server for monitoring coding-agent telemetry (tokens, cache
usage, tool calls, latency, cost) in real-time, across OpenAI- and
Anthropic-compatible agents.

## Features
- **Proxy Mode**: Intercepts requests to LLM providers — OpenAI-style
  (`/v1/chat/completions`), Anthropic-style (`/v1/messages`), and the aux
  endpoints agents call as part of their loop (`/v1/models`,
  `/v1/messages/count_tokens`, `/v1/responses`) — and forwards them
  unmodified to the real API.
- **Agent auto-attribution**: Most agents can't send custom headers, so
  calls without `X-Agent-ID` are attributed by fingerprint instead — a
  recognizable `User-Agent`, or a system prompt that names the agent
  (`fingerprints.yaml`). Unlabelled sessions are grouped by a stable hash
  of the task's first user message. No agent cooperation required.
- **Run wrapper**: `harnesswurm run` points any agent at the proxy with the
  agent, experiment and session carried in the base URL itself — see
  [Running an agent under an experiment](#running-an-agent-under-an-experiment).
- **Configurable providers**: Where each call is forwarded to is edited in the
  desktop app's Settings tab (or in `providers.yaml`), so the hosted APIs, a
  gateway, or a model server on localhost (Ollama, vLLM, LM Studio, llama.cpp,
  LiteLLM) are all just entries. See [Providers](#providers).
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
   `pricing.yaml`, `providers.yaml`, `fingerprints.yaml`) lives in the
   current directory.

### Embedding

`backend` is a library (`src/lib.rs`) with a thin binary (`src/main.rs`) on
top, not just a binary — `../desktop/src-tauri` depends on it directly and
spawns `harnesswurm_backend::run(ServerConfig { bind_addr, data_dir })` from
its own startup, so the desktop app is a single process with no separate
`cargo run` needed. Point `data_dir` at wherever makes sense for the embedding
host (a proper per-OS app data directory for a packaged app); `agents.yaml`,
`pricing.yaml`, `providers.yaml` and `fingerprints.yaml` are seeded there from the bundled defaults
on first run if missing, and left alone (still user-editable) after that.

## Usage

Point your agent's OpenAI-compatible client at
`http://localhost:8081/v1/chat/completions`, or its Anthropic-compatible
client at `http://localhost:8081/v1/messages`. Requests are forwarded as-is
to the provider configured for that route — out of the box `api.openai.com` /
`api.anthropic.com` — carrying whatever headers your client already sends
(`Authorization`, `x-api-key`, `api-key`, `anthropic-version`, a gateway's
own), so no proxy-specific auth setup is needed. To send a call somewhere else (a local model server, a
gateway), add it to `providers.yaml` and address it by name: see
[Providers](#providers).

### Attribution — who made this call

Three sources, most specific first. Nothing is mandatory anymore; a
completely unconfigured agent still gets sensible attribution.

1. **Run prefix in the URL** — `/r/<agent>/<experiment>/<session>/v1/…` or
   `/r/<agent>/<session>/v1/…`. This is what the `harnesswurm run` wrapper
   sets, and it is the one channel every agent supports, because it needs
   nothing beyond the base URL they all let you configure.
2. **Headers** — `X-Agent-ID`, `X-Session-ID`, and the optional
   `X-Experiment-ID` (groups calls for comparison), for clients that *can*
   send custom headers.
3. **Fingerprints** — no agent cooperation needed at all. `fingerprints.yaml`
   maps recognizable `User-Agent` headers (`claude-cli/1.x` …) and
   system-prompt openings ("You are Claude Code…") to agent names;
   Claude Code is seeded, and the Traffic tab shows what your other tools
   actually send, so extending it is copy-paste. An explicit header or run
   prefix always beats a fingerprint. Without a labelled session, calls are
   grouped as `auto-<hash>` by the conversation's *first* user message —
   the turns of one task stay together, and a new task starts a new session.

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
working directory), and `agents.yaml` / `pricing.yaml` / `providers.yaml` /
`fingerprints.yaml` are seeded there on first run.

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
keeps its state (`harnesswurm.db`, `agents.yaml`, `pricing.yaml`,
`providers.yaml`, `fingerprints.yaml`) in the
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
wrong answer: the cheapest run is the one that gave up. Its turns carry a real
token arc and per-turn tool calls, so the phase and tool breakdowns have
something to show.

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

The aux endpoints are proxied on the same base URLs — `/v1/models`,
`/v1/messages/count_tokens`, `/v1/responses` — so an agent probing
connectivity or pre-counting its context gets a real answer instead of a
404 that looks like a dead provider. `/v1/responses` (the Responses API the
Codex CLI speaks) is recorded like any other call, with token, cache and
tool-call counts — `function_call` items by their tool's name, built-in
tools (`web_search_call` …) by their type. `models` and `count_tokens` are
forwarded unrecorded — they are questions *about* a call, not calls.

Those two go to the provider marked `default` for that style in
`providers.yaml`. To reach a specific provider instead, put its name in the
base URL — `http://localhost:8081/p/ollama/v1` and
`http://localhost:8081/p/ollama` respectively (the same prefix works on the
aux endpoints). See [Providers](#providers).

Most agents expose this as an environment variable or a config field
(`OPENAI_BASE_URL`, `ANTHROPIC_BASE_URL`, a `baseUrl` setting…); the exact
name varies per tool — or skip this section entirely and use
[`harnesswurm run`](#running-an-agent-under-an-experiment), which sets them
for you.

### Running the tests
```bash
cd harnesswurm/backend && cargo test     # parsing, pricing, DB, state rules
cd desktop && npm test                   # UI, and npm run typecheck
```

## Providers

Every call is forwarded to the URL of the provider it resolved to, and
`providers.yaml` says what those are.

**In the app:** Settings → Providers lists them, with the base URL to paste
into an agent next to each. Add, edit or remove entries and hit *Save
providers*; the file is rewritten and the next call uses it — no restart. A
rejected edit (nameless entry, duplicate name, a base URL that isn't one, two
defaults for one api style) changes nothing on disk or in memory and says
which entry is wrong.

**In the file:** it is seeded on first run and looks like this:

```yaml
providers:
  - name: "openai"
    api: "openai"
    base_url: "https://api.openai.com/v1"
    default: true

  - name: "anthropic"
    api: "anthropic"
    base_url: "https://api.anthropic.com"
    default: true

  - name: "ollama"
    api: "openai"
    base_url: "http://localhost:11434/v1"
```

- `name` — how the provider is addressed, and the name every call to it is
  recorded under (what the Traffic view shows, and what `provider:` in
  `pricing.yaml` matches).
- `api` — the wire format the endpoint speaks, `openai` or `anthropic`. It
  decides how the traffic is *parsed*, not where it goes: a local server
  speaking the OpenAI format is `api: openai` even though nothing about it is
  OpenAI.
- `base_url` — exactly the base URL you would otherwise configure in the
  client: for `openai` the part before `/chat/completions` (usually ending in
  `/v1`), for `anthropic` the part before `/v1/messages`. A complete endpoint
  URL is accepted as-is too, and a query string — a gateway's `api-version`
  or key — is kept at the end where it belongs.
- `default` — the provider the bare `/v1/chat/completions` and `/v1/messages`
  routes use, one per api style.

Hand edits take effect on the next start; a save from the app takes effect
immediately. Note that saving from the app rewrites the file, so comments
added by hand do not survive one. On startup the resolved target of every
provider is printed, and `GET /v1/providers` returns the same list, so where a
call would actually go is never a guess.

### Choosing a provider per call

Three ways, most specific first:

1. **Path prefix** — `POST /p/<name>/v1/chat/completions` or
   `POST /p/<name>/v1/messages`. This is the one to use for coding agents,
   since it needs nothing but the base URL they already let you set.
2. **`X-Provider: <name>` header** — for clients that can send custom headers
   but have a fixed path.
3. **Nothing** — the `default: true` provider for that route's api style.

An unknown name is refused with 404 (listing what *is* configured) and a
provider addressed on the wrong style's endpoint with 400, rather than being
quietly forwarded to the hosted API under a name you didn't ask for.

### Local models, end to end

Ollama, llama.cpp, vLLM, LM Studio and LiteLLM all serve the OpenAI format, so
they are one entry each:

```yaml
  - name: "ollama"
    api: "openai"
    base_url: "http://localhost:11434/v1"
```

```bash
curl http://localhost:8081/p/ollama/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "X-Agent-ID: manual-test" -H "X-Session-ID: local-1" \
  -d '{"model":"llama3.2","messages":[{"role":"user","content":"say hi"}]}'
```

Whatever headers the client sends are forwarded unchanged; a local server
that wants no auth simply receives none.

Local models are unpriced by default, which shows as no cost estimate rather
than $0.00. If the machine time is worth costing, add an entry to
`pricing.yaml` whose `provider` matches the provider name:

```yaml
  - name: "llama3.2"
    provider: "ollama"
    input_per_million: 0.0
    output_per_million: 0.0
```

### Which headers reach the provider

Every request header is forwarded except two groups: those describing this
hop rather than the call (`host`, `content-length`, `connection` and the
other hop-by-hop headers, plus `accept-encoding`, since responses are
returned with the provider's own headers intact), and Harnesswurm's own
`X-Agent-ID` / `X-Session-ID` / `X-Experiment-ID` / `X-Provider`, which mean
nothing upstream. A provider authenticating with `api-key`, an organization
header, or anything else a gateway expects therefore works without being
named anywhere.

### Env overrides

`HARNESSWURM_OPENAI_BASE_URL` and `HARNESSWURM_ANTHROPIC_BASE_URL` replace the
`base_url` of the default provider for that style, for a one-off run without
editing the file. The file's own value is left untouched — the Settings tab
shows the override and keeps editing the saved value, so an override can't
quietly become permanent. They are deliberately *not* named `OPENAI_BASE_URL` /
`ANTHROPIC_BASE_URL`: those are usually already set to point a client **at**
this proxy, and honoring them here would make Harnesswurm forward to itself.

`HARNESSWURM_TRAFFIC_RETENTION_DAYS` (default 30) ages captured
request/response bodies out of the database — they are the bulk of it and
the only part with privacy weight. The task rows, counts, costs and
verdicts built from them stay. `0` keeps bodies forever. Pruning runs on
startup and then every six hours.

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

States that want a human keep flagging until the run's *next call* — the
Agents view's bell button acknowledges one without deleting it, so a run you
have already dealt with stops lighting up the dashboard while its history
stays intact.

Calls left open by a killed process are closed out as *Interrupted* on the
next startup, so nothing shows as permanently "Thinking".

## Running an agent under an experiment

`harnesswurm run` is the way to point a real agent at the proxy when you
care about attribution: it carries the agent, experiment and session in the
base URL itself, sets `ANTHROPIC_BASE_URL` / `OPENAI_BASE_URL` on the child
process, and propagates its exit code.

```bash
cd harnesswurm/backend
cargo run --bin harnesswurm -- run --experiment issue-1284 --agent kilo -- kilo code
cargo run --bin harnesswurm -- run --experiment issue-1284 --agent claude-code -- claude
```

It prints the base URLs it set and spawns the command. Every call the agent
makes — either API style, models and count_tokens included — lands under
that agent, that experiment, and a generated session id like
`issue-1284-kilo-18c2f04d-9a31`, ready for the comparison view.

- `--session <id>` overrides the generated session id (use it to resume a
  labelled run).
- `--provider <name>` routes through a specific `providers.yaml` entry
  instead of the style default.
- `--addr <host:port>` targets a proxy other than
  `$HARNESSWURM_ADDR` / `127.0.0.1:8081` — the desktop app's embedded one,
  or a remote box.

The same paths work by hand, without the wrapper:
`http://localhost:8081/r/<agent>/<experiment>/<session>/v1` for an
OpenAI-compatible client (drop the `/v1` for Anthropic-compatible), and
`/r/<agent>/<session>/v1` when there is no experiment to group under.

## Comparing two agents on the same task

Wrap both agents with the same `--experiment` — or set the headers by hand
if you prefer:

```bash
# via the wrapper (recommended — needs nothing from the agent)
harnesswurm run --experiment issue-1284 --agent kilo         -- kilo code
harnesswurm run --experiment issue-1284 --agent claude-code  -- claude

# by hand, on every call
X-Agent-ID: kilo         X-Session-ID: issue-1284-kilo    X-Experiment-ID: issue-1284
X-Agent-ID: claude-code  X-Session-ID: issue-1284-claude  X-Experiment-ID: issue-1284
```

Open **Analytics → the experiment**. Each agent+session becomes one *run*, and
the table shows what each spent, in tokens, calls, tool calls and wall clock.

### What counts as one run

`X-Session-ID` does not mean the same thing to every agent, so the comparison
lets you say which it is:

- **Per session** (default) — one run per session id. Right when that id is
  stable for the length of a task, and the only way to sit several deliberate
  repeats of the same task side by side, which is what taking more than one
  sample needs.
- **Per agent** — everything one agent did under the experiment is a single
  run. Right for agents that mint a fresh session id per *session* rather than
  per task: a restart, a context compaction, reopening the editor. Per-session
  those shatter one attempt into a row per fragment, and the cheapest fragment
  gets crowned.

The view spots the second case for you: if an agent has more than one run in an
experiment it says so and offers the switch, rather than leaving you to notice.
Merging changes the unit everywhere — the ranking, the roll-up, the phase
slices and the tool split all recount. Verdicts stay attached to real sessions
underneath, so a merged run counts as solved when any of its sessions did, and
judging a merged run marks every session under it.

Both are also available on the API as `?group=session` (default) or
`?group=agent`.

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

### Where the money went

Under the comparison, each run is broken down two ways — both computable only
because the proxy sees the traffic, not just what an agent chose to log:

- **Across the arc of the task.** A run's calls are cut into five slices of
  equal length, and each slice is split into the three things billed at
  different rates: input read fresh, input served from cache (an order of
  magnitude cheaper), and tokens generated. Early slices are the agent reading
  its way in; later ones are generation. Two runs that cost the same can have
  completely different shapes, and the shape says *where* the money went.
  Panels are scaled to their own peak — compare shapes here, totals above. A
  run of fewer than five calls fills fewer slices rather than gaining empty
  ones.
- **Through which tools.** A turn's tokens are split across the tool calls that
  turn made, so a turn calling `read_file` twice and `bash` once gives
  `read_file` two thirds of it. Turns that called no tool are attributed to
  nothing.

Two honest limits: the real price of a tool's result is paid by the *next*
turn, which carries that result in its context, so read the tool split as what
a run leaned on rather than an exact ledger; and tool names are recorded as
calls are proxied, so traffic captured before this existed shows no tools.

### Analytics API
- `GET /v1/analytics/experiments` — list experiments.
- `DELETE /v1/analytics/experiments/:id` — delete an experiment. Its calls
  survive, ungrouped: the calls belong to their agents, the experiment is
  only the label. Cannot be undone.
- `DELETE /v1/analytics/agents/:name` — delete an agent **and** every call
  recorded for it: metrics, captured traffic bodies, rate-limit readings,
  tool tallies, verdicts and dismissals. Cannot be undone.
- `GET /v1/analytics/experiments/:id/metrics` — metrics for one experiment, over time.
- `GET /v1/analytics/experiments/:id/comparison[?group=session|agent]` — one
  row per run in the experiment: totals for cost, tokens, cache reads, tool
  calls, wall clock, failed/rate-limited/still-open calls, how many session ids
  the run covers, and the verdict.
- `GET /v1/analytics/experiments/:id/breakdown[?group=session|agent]` — `{ phases, tools }`: each
  run's five phase slices (tokens, cache reads, tool calls, cost per slice) and
  its spend attributed per tool.
- `PUT /v1/analytics/sessions/verdict` — mark a run solved or failed:
  `{"agent_name": "kilo", "session_id": "issue-1284-kilo", "verdict": "solved", "note": "tests pass"}`.
  Send `experiment_id` instead of `session_id` to judge a merged run, which
  applies the verdict to every session that agent has in the experiment;
  sending both is a 400. A `verdict` of `null` clears it.
- `GET /v1/analytics/tasks` — most recent calls across all agents (model, provider, tokens, cache tokens, tool calls, latency, cost, call status, and a short preview of what was asked).
- `GET /v1/analytics/tasks/:id/traffic` — the full raw request/response body for one call.
- `GET /v1/analytics/sessions` — one row per agent+session: derived state, totals, spend, and the last question asked.
- `PUT /v1/analytics/sessions/dismiss` — acknowledge a run's attention state:
  `{"agent_name": "kilo", "session_id": "issue-1284-kilo"}`. The row keeps its
  truthful state text but reports `needs_attention: false` and
  `dismissed: true` until the run's next call, which re-arms the badge on its
  own — there is no un-dismiss.
- `GET /v1/analytics/limits` — current quota per provider, folded from the most recent reading of each header.
- `GET /v1/providers` — the configured providers and the URL each one forwards
  to, plus `env_override` when a variable is supplying the base URL in effect.
- `PUT /v1/providers` — replace the list (`{"providers": [...]}`), which is
  what the Settings tab sends: validated, written to `providers.yaml`, and in
  effect for the next call. Rejected edits change nothing.
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
