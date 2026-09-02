# AGENTS.md

Operating instructions for coding agents working in this repository.
Read this file before every task.

**Working code only. Finish the job. Plausibility is not correctness.**

## Rules

- State the plan in one or two sentences before editing; for non-trivial
  work, a numbered list with a verification step for each.
- Read the files you touch and the files that call them. Use subagents for
  broad exploration so the main context stays clean.
- Every changed line must trace to the request. No drive-by refactors, no
  reformatting: this repo is not rustfmt-clean globally, so never run bare
  `cargo fmt` — format only files you created in this change.
- Surface assumptions out loud before implementing them. If the task has two
  plausible readings that change the output, ask instead of picking.
- Never fabricate file paths, APIs, or test results. Run the command, or say
  you don't know.
- Address root causes, not symptoms; a suppressed error is not a fixed error.
- After two failed corrections on the same issue, stop and ask for a sharper
  prompt instead of a third attempt.

## Project

Harnesswurm — a local proxy that monitors coding-agent LLM traffic (tokens,
cache, tool calls, cost, live per-agent status) and compares agents on the
same task. A Rust backend is embedded in a Tauri desktop app; there is no
server to deploy.

## Commands

Backend (Rust — run from `harnesswurm/backend`):
- Build: `cargo build`
- Test: `cargo test`
- Lint: `cargo clippy --lib --bins` (three warnings are pre-existing on
  main; fix only warnings you introduced)
- Standalone server: `BIND_ADDR=127.0.0.1:8091 cargo run` (state lives in
  the current directory)
- Agent wrapper: `cargo run --bin harnesswurm -- run --agent <name>
  --experiment <id> -- <command>`

Desktop (run from `desktop`):
- `npm install`
- `npm test` and `npm run typecheck`
- Full app: `npm run tauri dev`; UI-only hot reload: `npm run dev`

## Layout

- `harnesswurm/backend/src/` — the proxy. `lib.rs` (routing, attribution,
  request/response parsing), `fingerprints.rs`, `providers.rs`, `pricing.rs`,
  `session_state.rs`, `agent_question.rs`, `rate_limits.rs`, `db.rs`.
- `harnesswurm/backend/*.yaml` — first-run defaults, bundled via
  `include_str!` and seeded into the data dir only if missing.
- `harnesswurm/backend/src/bin/harnesswurm.rs` — the `run` wrapper CLI.
- `desktop/src/` — React + Tailwind frontend; `desktop/src-tauri/` embeds
  the backend crate in the app process.
- Runtime state (`harnesswurm.db`, the yaml files) lives in the OS app-data
  dir for the desktop app, or the CWD for the standalone binary. Never
  commit it.

## Conventions

- Tests live in the same file under `#[cfg(test)]`. Prefer pure functions
  over DB/HTTP; DB tests use `sqlite::memory:`.
- Proxy routing: one fallback dispatcher (`proxy_dispatch`) parses the path
  via `parse_proxy_path` — new endpoints or prefixes extend the parser and
  its tests, not the route table.
- Attribution precedence: `/r/…` run prefix > `X-*-ID` headers >
  fingerprints. Never let a fingerprint beat an explicit label.
- `/v1/models` and `/v1/messages/count_tokens` are forwarded but not
  recorded. `/v1/responses` is recorded with usage and tool calls —
  `function_call` items by their `name`, other `*_call` items by type
  (`responses_tool_name`).
- Providers (`providers.yaml`) are hot-editable via `PUT /v1/providers`;
  every other yaml is seeded once, then hand-edited (restart to apply).
- DB migrations are `add_column_if_missing` calls in `Database::new` — there
  are no migration files.
- Don't log prompt or response content anywhere new; the traffic capture is
  the only place bodies belong, and it ages out
  (`HARNESSWURM_TRAFFIC_RETENTION_DAYS`, default 30).

## Workflow

- One branch per task off the latest `origin/main`: `feature/<kebab>` or
  `bugfix/<kebab>`.
- Full checks before opening a PR: `cargo test`, `cargo clippy --lib
  --bins`, and in `desktop`: `npm test`, `npm run typecheck`. End the PR
  title with the issue ref `(#NN)`. Keep addressing review comments until
  the PR is merged.
- Commit messages: subject under 72 chars, body explains the why. No agent
  attribution lines (`Co-Authored-By`, session ids, generated branch names
  as subjects) — CI rejects them.

## Project learnings

Rules accumulated from corrections; append one line per correction, prune
lines whose underlying issue is gone.

- Background test servers must be started with the tool's background mode —
  `cmd &` inside a Bash call gets killed when the call ends.
