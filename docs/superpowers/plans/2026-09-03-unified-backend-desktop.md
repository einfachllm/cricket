# Backend ↔ Desktop Unification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make backend and desktop behave as one system: one repo command runs all checks, the frontend discovers the backend URL at runtime instead of assuming `:8081`, and analytics JSON shapes are contract-tested against the TypeScript interfaces.

**Architecture:** Root `package.json` orchestrates the existing cargo/npm suites (no new runners); `lib/api.ts` gains a resolved base (`VITE_` override → Tauri `get_backend_url` → `8081` default) while the Tauri shell probes ports `8081..8090` and reports the winner; Rust `#[cfg(test)]` key-contract tests pin analytics row shapes to the frontend interfaces. No backend API change; no new dependencies (Tauri JS API already present).

**Tech Stack:** Rust (axum backend, Tauri 2 shell) + React 18 + Vite + TypeScript 5 + vitest

## Global Constraints

- Frontend commands from `desktop/`; Rust commands from `harnesswurm/backend`; new root orchestration from repo root.
- `cargo test` (170 tests) and `npm test` (67 tests) must stay green; `cargo clippy --lib --bins` introduces no new warnings; `npm run typecheck` clean.
- Never run bare `cargo fmt`; no Rust formatting changes.
- Every changed line must trace to this plan; no drive-by refactors.
- Don't log prompt or response content anywhere new.
- Attribution precedence unchanged: `/r/…` > headers > fingerprints (untouched).

---

### Task 1: Root orchestration + AGENTS.md

**Files:**
- Create: `package.json` (repo root)
- Modify: `AGENTS.md` (Commands section only)

**Interfaces:**
- Consumes: existing `desktop/package.json` scripts, `harnesswurm/backend` cargo targets
- Produces: `npm run checks` entry point used by humans/CI; nothing else depends on it

- [ ] **Step 1: Create root `package.json`**

```json
{
  "name": "harnesswurm",
  "version": "0.1.0",
  "private": true,
  "scripts": {
    "checks": "npm run typecheck --prefix desktop && npm test --prefix desktop && cargo test --manifest-path harnesswurm/backend/Cargo.toml && cargo clippy --manifest-path harnesswurm/backend/Cargo.toml --lib --bins",
    "test": "npm test --prefix desktop && cargo test --manifest-path harnesswurm/backend/Cargo.toml",
    "typecheck": "npm run typecheck --prefix desktop",
    "dev:desktop": "npm run tauri dev --prefix desktop",
    "dev:server": "cargo run --manifest-path harnesswurm/backend/Cargo.toml"
  }
}
```

- [ ] **Step 2: Run it to verify it passes**

Run: `npm run checks`
Expected: green across all four suites (tsc clean, 67 vitest pass, 170 cargo pass, clippy with only the 3 pre-existing warnings)

- [ ] **Step 3: Point AGENTS.md at the root command**

Replace in `AGENTS.md`:
```
Backend (Rust — run from `harnesswurm/backend`):
- Build: `cargo build`
- Test: `cargo test`
```
with:
```
Backend (Rust — run from `harnesswurm/backend`, or repo root via `npm run <script>`):
- Build: `cargo build`
- Test: `cargo test`
- All suites at once (repo root): `npm run checks` (desktop typecheck + vitest, backend cargo test + clippy)
```

- [ ] **Step 4: Commit**

```bash
git add package.json AGENTS.md
git commit -m "Add root orchestration for all checks" -m "One npm run checks runs desktop typecheck plus vitest and backend cargo test plus clippy."
```

### Task 2: Runtime backend-URL discovery

**Files:**
- Modify: `desktop/src/lib/api.ts`
- Modify: `desktop/src/hooks/useSessions.tsx:66-83`
- Modify: `desktop/src/App.tsx` (boot init)
- Modify: `desktop/src-tauri/src/main.rs`
- Create: `desktop/src/lib/apiBase.test.ts`

**Interfaces:**
- Consumes: `invoke` from `@tauri-apps/api/core` (already a dependency), `import.meta.env.VITE_HARNESSWURM_API_BASE`
- Produces: `resolvedApiBase()` / `initBackendUrl()` used by `fetchJson`, `saveProviders`, `proxyBaseUrl`, SSE setup; Tauri command `get_backend_url`

- [ ] **Step 1: Write failing frontend test**

`desktop/src/lib/apiBase.test.ts`:
```typescript
import { expect, test, vi, afterEach } from "vitest";
import { setApiBase, resetApiBaseForTests, resolvedApiBase } from "./api";

afterEach(() => { resetApiBaseForTests(); vi.unstubAllEnvs(); });

test("explicit setter wins over everything", () => {
  vi.stubEnv("VITE_HARNESSWURM_API_BASE", "http://env:9999");
  setApiBase("http://tauri:8082");
  expect(resolvedApiBase()).toBe("http://tauri:8082");
});

test("vite env override beats the default", () => {
  vi.stubEnv("VITE_HARNESSWURM_API_BASE", "http://env:9999");
  expect(resolvedApiBase()).toBe("http://env:9999");
});

test("default is localhost 8081", () => {
  expect(resolvedApiBase()).toBe("http://localhost:8081");
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `npm test -- src/lib/apiBase.test.ts`
Expected: FAIL with "setApiBase is not a function" / "does not provide an export named"

- [ ] **Step 3: Implement resolution in `lib/api.ts`**

```typescript
export const API_BASE = 'http://localhost:8081';

let apiBaseOverride: string | null = null;

/// Highest precedence: the URL the Tauri shell reports (port it actually bound).
export function setApiBase(url: string): void {
  apiBaseOverride = url;
}

/// Test-only reset; never call from app code.
export function resetApiBaseForTests(): void {
  apiBaseOverride = null;
}

function envApiBase(): string | null {
  const viaVite = (import.meta as any)?.env?.VITE_HARNESSWURM_API_BASE as string | undefined;
  return viaVite && viaVite.length > 0 ? viaVite : null;
}

/// Resolution order: explicit (Tauri) > Vite env > compiled default.
export function resolvedApiBase(): string {
  return apiBaseOverride ?? envApiBase() ?? API_BASE;
}

/// Call once at app boot. Outside Tauri (plain Vite browser) the invoke
/// rejects and the default/env value stands — that is the fallback, not an error.
export async function initBackendUrl(): Promise<void> {
  try {
    const { invoke } = await import('@tauri-apps/api/core');
    const url = await Promise.race([
      invoke<string>('get_backend_url'),
      new Promise<never>((_, reject) => setTimeout(() => reject(new Error('backend url timeout')), 2000)),
    ]);
    if (url) setApiBase(url);
  } catch {
    // Browser dev or invoke unavailable: resolvedApiBase() falls back.
  }
}
```

Replace every internal use of `API_BASE` as a fetch prefix with `resolvedApiBase()`: `fetchJson` (`fetch(API_BASE + path)`), `saveProviders`, `proxyBaseUrl`, and the `EventSource` URL in `useSessions.tsx`. Keep the exported `API_BASE` constant (tests and `proxyBaseUrl` snapshot expectations reference the default).

`useSessions.tsx`: create the `EventSource` inside an effect that awaits `initBackendUrl()` first (polling `refresh()` starts immediately, unchanged — SSE attaches once resolved).

`App.tsx`: `await initBackendUrl()` is already covered by the `SessionsProvider` mount path — do NOT add a second call; the single call lives in `useSessions.tsx`. (If the implementer finds no clean single place, one call in `SessionsProvider`'s mount effect wins; never two.)

- [ ] **Step 4: Tauri shell reports its port (`desktop/src-tauri/src/main.rs`)**

```rust
struct BackendUrl(String);

#[tauri::command]
fn get_backend_url(state: tauri::State<BackendUrl>) -> String {
  state.0.clone()
}
```

In `setup`: probe `127.0.0.1:8081..=8090` with `std::net::TcpListener::bind` (drop the probe immediately), take the first free port, `app.manage(BackendUrl(format!("http://127.0.0.1:{port}")))`, register `.invoke_handler(tauri::generate_handler![get_backend_url])`, and spawn `harnesswurm_backend::run` with that `bind_addr`. If all ports are taken, keep the old behavior: log the error, window still opens. Zero backend-crate changes.

- [ ] **Step 5: Verify**

Run: `npm test -- src/lib/apiBase.test.ts`
Expected: PASS (3 tests)

Run: `npm run typecheck`
Expected: clean

Run: `npm test`
Expected: all PASS (existing `ProviderSettings.test.tsx` proxy-URL expectations still hold — default unchanged)

Manual (report output): `cargo run` standalone still serves `:8081`; `npm run tauri dev` opens with data flowing; with `:8081` occupied (`python3 -m http.server 8081 &`), the app binds `:8082` and the UI still loads (pill from Phase 2 shows ready).

- [ ] **Step 6: Commit**

```bash
git add desktop/src/lib/api.ts desktop/src/lib/apiBase.test.ts desktop/src/hooks/useSessions.tsx desktop/src-tauri/src/main.rs
git commit -m "Discover backend URL at runtime" -m "Tauri reports its bound port via get_backend_url with VITE override and 8081 fallback; probes 8081 to 8090 on conflict."
```

### Task 3: Analytics JSON key contracts

**Files:**
- Modify: `harnesswurm/backend/src/db.rs` (`mod tests` only)

**Interfaces:**
- Consumes: existing helpers `Database::new("sqlite::memory:")`, `get_or_create_agent`, `get_or_create_experiment`, `create_task`, `finish_task`, `log_metric`, `ok_outcome` (mirror `test_get_recent_tasks` setup at `db.rs:1412`)
- Produces: failing-on-drift key assertions; frontend `api.ts` interfaces (`SessionSummary`, `RunComparison`-via-metrics, `PhaseSlice`, `ToolUsage`, `ProviderLimits`) are the source of truth for key lists

- [ ] **Step 1: Write failing contract tests**

Append to `mod tests` (mirror `test_get_recent_tasks` setup: memory DB, agent, task with session/model/provider, `finish_task` with `ok_outcome("stop", false)`, one `log_metric` row):

```rust
fn require_keys(value: &Value, keys: &[&str], what: &str) {
    let obj = value.as_object().unwrap_or_else(|| panic!("{what} row is not an object"));
    for key in keys {
        assert!(obj.contains_key(*key), "{what} row missing key `{key}`: {value}");
    }
}

#[tokio::test]
async fn analytics_sessions_match_frontend_session_summary() -> Result<()> {
    let db = seeded_one_finished_task().await?;
    let rows = db.get_sessions(50).await?;
    assert!(!rows.is_empty());
    for row in &rows {
        require_keys(row, &["agent_name", "session_id", "state", "state_label", "needs_attention", "call_count", "input_tokens", "output_tokens", "total_cost", "model_name", "provider"], "sessions");
    }
    Ok(())
}

#[tokio::test]
async fn analytics_recent_tasks_match_traffic_view() -> Result<()> {
    let db = seeded_one_finished_task().await?;
    let rows = db.get_recent_tasks(50).await?;
    assert!(!rows.is_empty());
    for row in &rows {
        require_keys(row, &["task_id", "agent_name", "model_name", "provider", "session_id", "timestamp", "prompt_tokens", "completion_tokens", "tool_calls_count", "cost_estimate", "status"], "recent_tasks");
    }
    Ok(())
}

#[tokio::test]
async fn analytics_metrics_match_dashboard() -> Result<()> {
    let db = seeded_one_finished_task().await?;
    let experiments = db.get_all_experiments().await?;
    let rows = db.get_experiment_metrics(experiments[0].0).await?;
    assert!(!rows.is_empty());
    for row in &rows {
        require_keys(row, &["timestamp", "model_name", "provider", "prompt_tokens", "completion_tokens", "cache_creation_tokens", "cache_read_tokens", "tool_calls_count", "latency_ms", "cost_estimate"], "metrics");
    }
    Ok(())
}
```

With helper (built only from helpers verified to exist):
```rust
async fn seeded_one_finished_task() -> Result<Database> {
    let db = Database::new("sqlite::memory:").await?;
    let agent = db.get_or_create_agent("contract").await?;
    let exp = db.get_or_create_experiment("contract-exp", None).await?;
    let task = db.create_task(agent, Some(exp), Some("contract task".into()), Some("s1".into()), Some("gpt-4o".into()), Some("openai".into())).await?;
    db.finish_task(task, &ok_outcome("stop", false)).await?;
    db.log_metric(task, 100, 50, 0, 0, 1, 1200, Some(0.0001)).await?;
    Ok(db)
}
```

Key lists are verbatim the `desktop/src/lib/api.ts` interfaces (`SessionSummary`, TrafficView `TaskSummary`, dashboard `MetricData`) plus `task_id`/`timestamp`/`status` already selected by the queries. If a query legitimately omits a listed key, the implementer does NOT weaken the test silently: rename the test with `_documents_missing_` + comment citing the frontend fallback, and report it as a concern.

- [ ] **Step 2: Run to verify they fail-then-pass honestly**

Run: `cargo test analytics_`
Expected: first run FAILs only if a key is genuinely missing (good — real drift signal); if all pass immediately, confirm by temporarily asserting a bogus key (`require_keys(row, &["__bogus"], ...)`) that it FAILs, then remove the bogus line. Report which of the two happened.

- [ ] **Step 3: Full suite + clippy**

Run: `cargo test`
Expected: 173 pass (170 + 3 new), 0 fail

Run: `cargo clippy --lib --bins`
Expected: only the 3 pre-existing warnings

- [ ] **Step 4: Commit**

```bash
git add harnesswurm/backend/src/db.rs
git commit -m "Pin analytics JSON keys to frontend contracts" -m "Sessions, recent tasks, and metrics rows assert the keys SessionSummary, TrafficView, and the dashboard read."
```

## Deferred (explicitly not in this plan)

- **Full ts-rs codegen (U2):** analytics handlers return `serde_json::Value`, so generated types require first refactoring `db.rs` queries into `Serialize` structs — a large, worthy follow-up. These contract tests are the cheap guard until then.
- **Port-0 / socket passing:** probe-then-bind has a benign local TOCTOU (worst case = today's log-and-open behavior). Passing a bound socket into `run()` would need a backend API change.
- **`saveProviders` raw fetch:** stays on `fetch(API_BASE…)` deliberately (needs the error body); now via `resolvedApiBase()`.
