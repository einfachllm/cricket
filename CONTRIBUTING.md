# Contributing to Harnesswurm

Thanks for taking the time. Harnesswurm is a small project, so the process is
short: open an issue if the change is more than a fix, work on a branch, run
the checks, open a pull request.

By participating you agree to the [Code of Conduct](CODE_OF_CONDUCT.md).

---

## Ways to help

Not everything useful is Rust:

- **Bug reports.** What you did, what happened, what you expected. The
  [bug report template](.github/ISSUE_TEMPLATE/bug_report.md) asks for the
  right details.
- **Agent fingerprints.** Harnesswurm attributes calls by `User-Agent` and
  system-prompt openings (`harnesswurm/backend/fingerprints.yaml`). If your
  tool shows up as `auto-<hash>`, the Traffic tab shows exactly what it sends
  — a fingerprint entry is a one-line PR that helps everyone using that tool.
- **Pricing entries.** `harnesswurm/backend/pricing.yaml` ships a starter set.
  Additions are welcome; cite the provider's pricing page in the PR so it can
  be verified.
- **Documentation.** If something in the README sent you the wrong way, that
  is a bug.
- **Code.** See below.

## Getting set up

You need [Rust](https://rustup.rs) (stable), [Node.js 18+](https://nodejs.org),
and the [Tauri system dependencies](https://v2.tauri.app/start/prerequisites/)
for your OS.

```bash
git clone https://github.com/einfachllm/harnesswurm.git
cd harnesswurm

# the whole app, backend embedded — this is how it ships
cd desktop && npm install && npm run tauri dev
```

For frontend-only work, two terminals gives you Vite's hot reload without
rebuilding the Rust shell:

```bash
cd harnesswurm/backend && cargo run   # http://127.0.0.1:8081
cd desktop && npm run dev             # http://localhost:5173
```

Don't run the standalone backend and the desktop app at the same time — they
keep separate databases, and whichever grabs port 8081 first wins, so the UI
will quietly show that one's data.

No API key? `python3 harnesswurm/demo_seed.py --db <path>` seeds one session
per status plus a demo experiment. Seed *after* the app is running.

## Where things live

| Path | What it is |
| --- | --- |
| `harnesswurm/backend/src/lib.rs` | The proxy: routing, attribution, request/response parsing |
| `harnesswurm/backend/src/` | `fingerprints.rs`, `providers.rs`, `pricing.rs`, `session_state.rs`, `agent_question.rs`, `rate_limits.rs`, `db.rs` |
| `harnesswurm/backend/src/bin/harnesswurm.rs` | The `harnesswurm run` wrapper CLI |
| `harnesswurm/backend/*.yaml` | First-run defaults, bundled with `include_str!` and seeded into the data dir only if missing |
| `desktop/src/` | React + Tailwind frontend |
| `desktop/src-tauri/` | The Tauri shell; depends on the backend crate and runs it in-process |

Runtime state (`harnesswurm.db` and the yaml files) lives in the OS app-data
directory for the desktop app, or the working directory for the standalone
binary. It is local state — never commit it.

## Running the checks

Run all four before opening a pull request. They are exactly what CI runs on
your PR, so a green run here means a green run there.

```bash
cd harnesswurm/backend
cargo test --locked

cd ../..
python3 scripts/clippy_baseline.py   # clippy, compared against the baseline

cd desktop
npm ci
npm run typecheck
npm test
```

**About clippy.** The repository is not clippy-clean — a few warnings predate
the lint being enabled, and fixing them is a separate change from yours.
`-D warnings` would therefore be red on every PR and get ignored, so instead
each known warning is recorded per lint in `scripts/clippy_baseline.json`, and
the check fails only on warnings your change *adds*. If you fix one, the script
says so; lower the baseline in the same PR with
`python3 scripts/clippy_baseline.py --update`. Raising the baseline is allowed
but should be a deliberate, explained diff.

**Don't run bare `cargo fmt`**: this repository is not rustfmt-clean globally,
and a blanket reformat buries the actual change. Format only files you
created.

## Conventions

These are the ones that come up in review most often. The full set — including
the rules a coding agent working here has to follow — is in
[AGENTS.md](AGENTS.md).

- **Every changed line traces to the change.** No drive-by refactors, no
  reformatting of untouched code.
- **Tests live beside the code** in the same file under `#[cfg(test)]`. Prefer
  testing pure functions over DB or HTTP paths; DB tests use `sqlite::memory:`.
- **Proxy routing** goes through one fallback dispatcher (`proxy_dispatch`),
  which parses the path with `parse_proxy_path`. New endpoints or prefixes
  extend the parser and its tests, not the route table.
- **Attribution precedence** is `/r/…` run prefix > `X-*-ID` headers >
  fingerprints. A fingerprint must never beat an explicit label.
- **DB migrations** are `add_column_if_missing` calls in `Database::new`. There
  are no migration files.
- **Don't log prompt or response content anywhere new.** The traffic capture is
  the only place bodies belong, and it ages out
  (`HARNESSWURM_TRAFFIC_RETENTION_DAYS`, default 30). Never log API keys or
  other credentials, not even at debug level.
- **Providers** (`providers.yaml`) are hot-editable via `PUT /v1/providers`;
  every other yaml is seeded once and then hand-edited, so a change there needs
  a restart to apply.

## Branches, commits and pull requests

- **One branch per change**, off the latest `origin/main`: `feature/<kebab>` or
  `bugfix/<kebab>`.
- **Commit messages**: a subject under 72 characters in the imperative mood,
  and a body that explains *why* rather than restating the diff. Please leave
  out generated attribution trailers (`Co-Authored-By:` lines for tools,
  session ids, tool-generated branch names as subjects) — CI rejects them.
- **Pull requests**: fill in the template, describe what you changed and how
  you verified it, and end the title with the issue reference `(#NN)` when
  there is one. Keep the PR focused; unrelated fixes belong in their own PR.
- Keep addressing review comments until the PR is merged. A PR is done when it
  is green and a reviewer says so.

Contributions are accepted under the [Apache License 2.0](LICENSE). Unless you
state otherwise, anything you submit for inclusion is licensed under those
terms, with no additional conditions.

## Reporting a security issue

Please don't open a public issue for a vulnerability. See
[SECURITY.md](SECURITY.md) for how to report one privately.
