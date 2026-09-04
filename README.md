# Harnesswurm

[![CI](https://github.com/einfachllm/harnesswurm/actions/workflows/ci.yml/badge.svg)](https://github.com/einfachllm/harnesswurm/actions/workflows/ci.yml)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![Status: alpha](https://img.shields.io/badge/status-alpha-orange.svg)](#project-status)

**See what your coding agents actually cost — and which one actually solved
the task.**

Harnesswurm is a local desktop app that sits between your coding agents
(Claude Code, Codex, Kilo, opencode, aider, Cursor…) and the LLM providers
they call. It records every request: tokens, cache hits, tool calls, latency
and cost, plus what each agent session is doing *right now*. Then it lets you
put two agents on the same task and compare them on what the task really cost.

Everything runs on your machine. There is no account, no server to deploy and
no telemetry leaving the box — the database is a SQLite file in your own
app-data directory.

---

## Table of contents

- [Why](#why)
- [Install](#install)
- [Quickstart](#quickstart)
- [Connecting your agent](#connecting-your-agent)
- [Comparing two agents on the same task](#comparing-two-agents-on-the-same-task)
- [Using a different provider or a local model](#using-a-different-provider-or-a-local-model)
- [Where your data lives](#where-your-data-lives)
- [Troubleshooting](#troubleshooting)
- [Documentation](#documentation)
- [Project status](#project-status)
- [Contributing](#contributing)
- [Security](#security)
- [License](#license)

---

## Why

Agent dashboards usually show what an agent *chose* to report. A proxy sees
what actually went over the wire, so Harnesswurm can answer questions the
agent itself can't:

- **What did this task cost?** Per call, per session, per agent, per
  experiment — with cache reads priced separately from fresh input, because
  they are billed an order of magnitude apart.
- **Which agent is cheaper *per solved task*?** Ranking on spend alone crowns
  the agent that gave up after two calls. Harnesswurm makes you mark each run
  solved or failed, and only ranks the ones that worked.
- **Where did the money go?** Spend is broken down across the arc of a task
  (reading in vs. generating) and across the tools the agent called.
- **Is anything stuck?** Live per-session status — thinking, running a tool,
  waiting on you, rate limited, stalled, errored — inferred from the traffic,
  with no cooperation required from the agent.

**Non-goals.** Harnesswurm does not evaluate correctness for you, does not
modify the requests it forwards, and does not replace your provider's billing
as a source of truth. Costs are estimates from a price list you control.

## Install

You need [Rust](https://rustup.rs), [Node.js 18+](https://nodejs.org), and the
[Tauri system dependencies](https://v2.tauri.app/start/prerequisites/) for your
OS.

```bash
git clone https://github.com/einfachllm/harnesswurm.git
cd harnesswurm/desktop
npm install
npm run tauri dev
```

That opens the app with the proxy already listening on `127.0.0.1:8081` — the
backend is embedded in the app process, so there is no second thing to start.

To get a double-clickable app instead:

```bash
npm run tauri build   # installer/bundle in src-tauri/target/release/bundle
```

> There are no pre-built binaries yet — see [Project status](#project-status).

## Quickstart

### 1. See it working, without an API key

With the app running, seed some demo data:

```bash
python3 harnesswurm/demo_seed.py --db <path-to-harnesswurm.db>
```

The app logs the path to its database on startup. **Seed after the app is
running** — on startup it closes out calls that were left open, so a demo
session seeded first would be reaped as *Interrupted*.

Open the **Agents** view: every status should be visible (thinking, running a
tool, waiting for you, rate limited, auth failed, idle) with provider quota
bars. **Analytics** shows a seeded experiment, `demo-issue-1284`, with four
runs across three agents — deliberately arranged so that spend alone gives the
wrong answer: the cheapest run is the one that gave up.

`--clear` removes the demo rows again. They are all prefixed `demo-` and never
mix with real captured traffic.

### 2. Send a real call through it

```bash
curl http://localhost:8081/v1/chat/completions \
  -H "Authorization: Bearer $OPENAI_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"say hi"}]}'
```

Watch the Agents view: the session appears as **Thinking**, then settles to
**Waiting for you**. Your API key is forwarded to the provider and never
stored.

### 3. Point a real agent at it

The simplest way is the `harnesswurm run` wrapper, which sets the environment
variables for you:

```bash
cd harnesswurm/backend
cargo run --bin harnesswurm -- run --agent claude-code -- claude
```

Everything that agent does now shows up in the app, labelled.

## Connecting your agent

If you would rather configure the agent yourself, set its API base URL to the
proxy. How much of the path the client appends decides which URL to use:

| Client style | Base URL to configure | Typical env var |
| --- | --- | --- |
| OpenAI-compatible (appends `/chat/completions`) | `http://localhost:8081/v1` | `OPENAI_BASE_URL` |
| Anthropic-compatible (appends `/v1/messages`) | `http://localhost:8081` | `ANTHROPIC_BASE_URL` |

Requests are forwarded unmodified, carrying whatever auth headers your client
already sends, so there is no proxy-specific auth to set up.

**Who made this call?** Harnesswurm figures that out three ways, most specific
first:

1. **A run prefix in the URL** — `/r/<agent>/<experiment>/<session>/v1/…`.
   This is what `harnesswurm run` sets, and it works with every agent, because
   it needs nothing beyond the base URL they all let you configure.
2. **Headers** — `X-Agent-ID`, `X-Session-ID`, and the optional
   `X-Experiment-ID`, for clients that can send custom headers.
3. **Fingerprints** — no cooperation at all. A recognizable `User-Agent` or a
   system prompt that names the agent is matched against `fingerprints.yaml`
   (Claude Code ships seeded). Unlabelled sessions are grouped by a hash of the
   task's first user message, so the turns of one task stay together.

An explicit header or run prefix always wins over a fingerprint.

## Comparing two agents on the same task

Run both under the same experiment id:

```bash
harnesswurm run --experiment issue-1284 --agent kilo        -- kilo code
harnesswurm run --experiment issue-1284 --agent claude-code -- claude
```

Then open **Analytics → the experiment**. Each agent+session becomes one *run*,
and the table shows what each spent in tokens, calls, tool calls and wall
clock.

Three things worth knowing before you trust a number:

- **Mark runs solved or failed.** The proxy cannot see whether the diff works.
  Only solved runs are ranked, and until you have marked at least one, the view
  refuses to name a winner.
- **One run each is a sample size of one.** The per-agent roll-up divides
  everything an agent spent by the runs it *solved*, so an agent that lands one
  attempt in three is charged for all three. Run each task a few times.
- **Unpriced models and unfinished runs are excluded**, shown as `≥ $x` and
  `$x so far` rather than quietly winning on an understated total.

## Using a different provider or a local model

Anything that speaks the OpenAI or Anthropic wire format is one entry in
**Settings → Providers** (or in `providers.yaml`): a hosted API, a gateway, or
a model server on localhost — Ollama, vLLM, LM Studio, llama.cpp, LiteLLM.

```yaml
providers:
  - name: "ollama"
    api: "openai"              # the wire format, not the vendor
    base_url: "http://localhost:11434/v1"
```

Address it by putting its name in the base URL —
`http://localhost:8081/p/ollama/v1` — or leave it out to use the provider
marked `default` for that wire format. Saves from the Settings tab take effect
on the next call, with no restart.

## Where your data lives

Everything is local:

- **`harnesswurm.db`** — a SQLite database in your OS app-data directory (the
  app logs the path on startup); the current working directory if you run the
  standalone backend.
- **`agents.yaml`, `pricing.yaml`, `providers.yaml`, `fingerprints.yaml`** —
  seeded next to it on first run, then yours to edit.

**Privacy.** Harnesswurm stores the full request and response body of each
proxied call, so you can inspect exactly what was sent — which means prompts
and completions are on disk. Bodies age out after 30 days
(`HARNESSWURM_TRAFFIC_RETENTION_DAYS`; `0` keeps them forever); the counts,
costs and verdicts derived from them stay. **API keys are forwarded to the
provider and never written to the database.**

## Troubleshooting

| Symptom | Likely cause |
| --- | --- |
| The UI is empty while an agent is clearly running | Only proxied calls are visible. Check the agent really points at `localhost:8081`, and that it isn't using a subscription instead of API keys. |
| The UI shows stale or missing data | Two backends are running. The standalone `cargo run` and the desktop app use *different* databases; whichever grabbed port 8081 first wins. Run one. |
| A session sits at **Thinking** forever | It flips to **Stalled** after 10 minutes. Calls left open by a killed process are closed out as *Interrupted* on the next startup. |
| A session shows **Idle**, not **Crashed** | States are inferred, not reported. An agent that stops calling the API — crashed, or waiting on a long shell command — looks the same from the wire. |
| A call shows no cost | The model has no entry in `pricing.yaml`. That is deliberate: no estimate beats a wrong one. Add the model and its per-million prices. |
| An agent is attributed as `auto-<hash>` | It sent no label and matched no fingerprint. Use `harnesswurm run`, or add its `User-Agent` to `fingerprints.yaml` — the Traffic tab shows what it actually sends. |

## Documentation

- **[harnesswurm/README.md](harnesswurm/README.md)** — the full reference:
  every status rule, the provider and pricing formats, run attribution, the
  cost breakdowns, and the complete HTTP API.
- **[CONTRIBUTING.md](CONTRIBUTING.md)** — how to build, test and submit
  changes.
- **[AGENTS.md](AGENTS.md)** — operating instructions for coding agents working
  in this repository.

## Project status

**Alpha.** It is used daily and the core works, but the schema, the HTTP API
and the config formats can still change between commits, and there are no
tagged releases or pre-built binaries yet. Expect to build from source, and to
read a diff before pulling.

Cost figures are estimates computed from a price list *you* maintain
(`pricing.yaml`); they are not authoritative and are not a substitute for your
provider's billing.

## Contributing

Issues and pull requests are welcome — bug reports, fingerprints for agents we
haven't seen, pricing entries, and docs fixes all count. Start with
**[CONTRIBUTING.md](CONTRIBUTING.md)**; by taking part you agree to the
[Code of Conduct](CODE_OF_CONDUCT.md).

## Security

Harnesswurm handles API keys in transit and stores prompt content at rest.
Please report vulnerabilities privately rather than in a public issue — see
**[SECURITY.md](SECURITY.md)**.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this project shall be licensed as above, without any
additional terms or conditions.
