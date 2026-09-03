# Spec §3 Analytics Drill-Down Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Click any run in an experiment comparison and see its call-by-call timeline, per-tool split, and a jump to its raw traffic — with agent/model/provider/date filters over the metrics views.

**Architecture:** Backend adds run identity (`task_id`, `agent_name`, `session_key`) to metrics rows (the comparison query already proves the join pattern); frontend adds pure filter/select helpers, a `FilterBar`, an expandable `RunDetail` row, and a `?task=` deep link into Traffic. No new endpoints; no new dependencies.

**Tech Stack:** Rust (sqlx/SQLite) + React 18 + TypeScript 5 + vitest

## Global Constraints

- Frontend commands from `desktop/` (`npm test`, `npm run typecheck`); Rust from `harnesswurm/backend` (`cargo test`, `cargo clippy --lib --bins`).
- `cargo test` (173 tests) and `npm test` (70 tests) must stay green; clippy introduces no new warnings.
- Never run bare `cargo fmt`; no formatting churn.
- Every changed line must trace to this plan; no drive-by refactors.
- Don't log prompt or response content anywhere new.
- Attribution and grouping semantics unchanged: `session` grouping matches on `session_key`, `agent` grouping on `agent_name`.

---

### Task 1: Run identity on metrics rows

**Files:**
- Modify: `harnesswurm/backend/src/db.rs` (`get_experiment_metrics` SQL + row mapping + `mod tests`)
- Modify: `desktop/src/lib/api.ts` (new `MetricPoint` interface)
- Modify: `desktop/src/components/AnalyticsDashboard.tsx` (use `MetricPoint`)

**Interfaces:**
- Consumes: `tasks.agent_id → agents` join pattern (proven in `get_recent_tasks`, `db.rs:1037`), `COALESCE(t.session_id,'')` convention (proven in comparison query, `db.rs:636`)
- Produces: `MetricPoint { task_id, agent_name, session_key, timestamp, model_name, provider, prompt_tokens, completion_tokens, cache_creation_tokens, cache_read_tokens, tool_calls_count, latency_ms, cost_estimate }` consumed by Tasks 2-3

- [ ] **Step 1: Extend the SQL + mapping in `get_experiment_metrics` (`db.rs:573-611`)**

Replace the SELECT list head:
```sql
SELECT
    t.id as task_id,
    a.name as agent_name,
    COALESCE(t.session_id, '') as session_key,
    t.timestamp,
    t.model_name,
    ...
 FROM tasks t
 JOIN agents a ON t.agent_id = a.id
 JOIN metrics m ON t.id = m.task_id
 WHERE t.experiment_id = ?
 ORDER BY t.timestamp ASC
```
Add to the `json!` mapping:
```rust
"task_id": row.get::<i64, _>("task_id"),
"agent_name": row.get::<String, _>("agent_name"),
"session_key": row.get::<String, _>("session_key"),
```
(Keep every existing key byte-identical — the unification contract test pins them.)

- [ ] **Step 2: Extend the unification contract test key list**

In the `analytics_metrics_match_dashboard` test (`db.rs` `mod tests`, added by the unification plan), extend the asserted list with `"task_id", "agent_name", "session_key"`.

- [ ] **Step 3: Share the interface on the frontend**

In `desktop/src/lib/api.ts` add:
```typescript
export interface MetricPoint {
  task_id: number;
  agent_name: string;
  session_key: string;
  timestamp: string;
  model_name: string | null;
  provider: string | null;
  prompt_tokens: number;
  completion_tokens: number;
  cache_creation_tokens: number;
  cache_read_tokens: number;
  tool_calls_count: number;
  latency_ms: number;
  cost_estimate: number | null;
}
```
In `AnalyticsDashboard.tsx` delete the local `MetricData` interface and `import type { MetricPoint }`, renaming usages (`MetricData[]` → `MetricPoint[]`, `fetchJson<MetricData[]>` → `fetchJson<MetricPoint[]>`). `tokensByModel(rows: MetricPoint[])` signature updated the same way (body unchanged — it only reads `model_name`, `prompt_tokens`, `completion_tokens`).

- [ ] **Step 4: Verify**

Run: `cargo test metrics`
Expected: PASS (extended contract + existing metrics tests)

Run: `cargo test`
Expected: 173+ PASS, 0 fail

Run: `npm run typecheck`
Expected: clean

Run: `npm test`
Expected: all PASS (`AnalyticsDashboard.test.ts` tokensByModel cases still pass — same shape)

- [ ] **Step 5: Commit**

```bash
git add harnesswurm/backend/src/db.rs desktop/src/lib/api.ts desktop/src/components/AnalyticsDashboard.tsx
git commit -m "Attach run identity to experiment metrics" -m "Metrics rows carry task_id, agent_name, and session_key so timelines can be cut per run; shared MetricPoint replaces the local interface."
```

### Task 2: Pure filter + select helpers with FilterBar

**Files:**
- Create: `desktop/src/components/RunFilters.ts`
- Create: `desktop/src/components/RunFilters.test.ts`
- Modify: `desktop/src/components/AnalyticsDashboard.tsx` (FilterBar UI + filtered metrics)

**Interfaces:**
- Consumes: `MetricPoint` from Task 1
- Produces: `MetricFilters`, `filterMetrics`, `selectRunCalls`, `distinctValues` consumed by Task 3

- [ ] **Step 1: Write failing tests**

`desktop/src/components/RunFilters.test.ts`:
```typescript
import { expect, test } from "vitest";
import { filterMetrics, selectRunCalls, distinctValues } from "./RunFilters";
import type { MetricPoint } from "../lib/api";

const rows = [
  { task_id: 1, agent_name: "opencode", session_key: "s1", timestamp: "2026-09-01T10:00:00", model_name: "gpt-4o", provider: "openai", prompt_tokens: 10, completion_tokens: 5, cache_creation_tokens: 0, cache_read_tokens: 0, tool_calls_count: 1, latency_ms: 100, cost_estimate: 0.001 },
  { task_id: 2, agent_name: "claude", session_key: "s2", timestamp: "2026-09-03T10:00:00", model_name: "claude-sonnet-4-5", provider: "anthropic", prompt_tokens: 20, completion_tokens: 5, cache_creation_tokens: 0, cache_read_tokens: 0, tool_calls_count: 0, latency_ms: 200, cost_estimate: 0.002 },
] as MetricPoint[];

test("filterMetrics matches agent, model, provider, and date range", () => {
  const all = { agent: "all", model: "all", provider: "all", from: "", to: "" };
  expect(filterMetrics(rows, all)).toHaveLength(2);
  expect(filterMetrics(rows, { ...all, agent: "opencode" })).toHaveLength(1);
  expect(filterMetrics(rows, { ...all, model: "gpt-4o" })).toHaveLength(1);
  expect(filterMetrics(rows, { ...all, provider: "anthropic" })).toHaveLength(1);
  expect(filterMetrics(rows, { ...all, from: "2026-09-02" })).toHaveLength(1);
  expect(filterMetrics(rows, { ...all, to: "2026-09-02" })).toHaveLength(1);
});

test("selectRunCalls follows grouping semantics", () => {
  const run = { agent_name: "opencode", session_key: "s1" } as any;
  expect(selectRunCalls(rows, run, "session")).toHaveLength(1);
  expect(selectRunCalls(rows, run, "agent")).toHaveLength(1);
  expect(selectRunCalls(rows, { agent_name: "nope", session_key: "x" } as any, "session")).toHaveLength(0);
});

test("distinctValues lists sorted present values", () => {
  expect(distinctValues(rows, "agent_name")).toEqual(["claude", "opencode"]);
});
```

- [ ] **Step 2: Run to verify it fails**

Run: `npm test -- src/components/RunFilters.test.ts`
Expected: FAIL with "does not provide an export named 'filterMetrics'"

- [ ] **Step 3: Implement `RunFilters.ts` + `FilterBar` in dashboard**

```typescript
import type { MetricPoint, RunComparison, RunGrouping } from "../lib/api";

export interface MetricFilters {
  agent: string;
  model: string;
  provider: string;
  from: string;
  to: string;
}

export const EMPTY_FILTERS: MetricFilters = { agent: "all", model: "all", provider: "all", from: "", to: "" };

export function distinctValues(metrics: MetricPoint[], key: "agent_name" | "model_name" | "provider"): string[] {
  const seen = new Set<string>();
  for (const m of metrics) {
    const v = m[key];
    if (v) seen.add(v);
  }
  return [...seen].sort();
}

export function filterMetrics(metrics: MetricPoint[], f: MetricFilters): MetricPoint[] {
  return metrics.filter((m) =>
    (f.agent === "all" || m.agent_name === f.agent) &&
    (f.model === "all" || m.model_name === f.model) &&
    (f.provider === "all" || m.provider === f.provider) &&
    (f.from === "" || m.timestamp.slice(0, 10) >= f.from) &&
    (f.to === "" || m.timestamp.slice(0, 10) <= f.to)
  );
}

/// Calls belonging to one comparison run. Session grouping matches the
/// run's session key within its agent; agent grouping matches the agent.
export function selectRunCalls(metrics: MetricPoint[], run: Pick<RunComparison, "agent_name" | "session_key">, grouping: RunGrouping): MetricPoint[] {
  return metrics
    .filter((m) => m.agent_name === run.agent_name && (grouping === "agent" || m.session_key === run.session_key))
    .sort((a, b) => a.timestamp.localeCompare(b.timestamp));
}
```

`FilterBar` (in `AnalyticsDashboard.tsx`, above the summary cards): four controls — agent/model/provider `<select>` (options `All` + `distinctValues(metrics, …)`), two `<input type="date">` for from/to, plus a `Clear` button resetting to `EMPTY_FILTERS`. Feed `filterMetrics(metrics, filters)` into summary, time-series, and per-model chart (comparison table stays complete — it ranks all runs deliberately).

- [ ] **Step 4: Verify**

Run: `npm test -- src/components/RunFilters.test.ts`
Expected: PASS (3 tests)

Run: `npm run typecheck`
Expected: clean

Run: `npm test`
Expected: all PASS

- [ ] **Step 5: Commit**

```bash
git add desktop/src/components/RunFilters.ts desktop/src/components/RunFilters.test.ts desktop/src/components/AnalyticsDashboard.tsx
git commit -m "Add metrics filters over agent model provider date" -m "Pure filterMetrics plus selectRunCalls with grouping semantics; dashboard summary and charts read filtered metrics."
```

### Task 3: Expandable run detail + Traffic deep link

**Files:**
- Create: `desktop/src/components/RunDetail.tsx`
- Modify: `desktop/src/components/ExperimentComparison.tsx` (RunRow select + detail row)
- Modify: `desktop/src/components/TrafficView.tsx` (read `?task=` on mount)

**Interfaces:**
- Consumes: `selectRunCalls` (Task 2), `RunComparison`/`ToolUsage`/`MetricPoint` types, breakdown `tools` already per `(agent_name, session_key)`
- Produces: nothing further (leaf UI)

- [ ] **Step 1: Create `RunDetail.tsx`**

```tsx
import React from "react";
import { Link } from "react-router-dom";
import type { MetricPoint, RunComparison, ToolUsage } from "../lib/api";
import { formatCost, formatTimestampUtc, formatTokens } from "../lib/api";

export function RunDetail({ run, calls, tools }: { run: RunComparison; calls: MetricPoint[]; tools: ToolUsage[] }) {
  const runTools = tools.filter((t) => t.agent_name === run.agent_name && t.session_key === run.session_key);
  return (
    <div className="space-y-4 px-4 py-4">
      {calls.length === 0 ? (
        <p className="text-sm text-slate-400">No calls recorded for this run yet.</p>
      ) : (
        <ol className="space-y-1">
          {calls.map((c) => (
            <li key={c.task_id} className="flex flex-wrap items-center gap-x-4 gap-y-0.5 text-xs text-slate-400">
              <span className="font-mono">{formatTimestampUtc(c.timestamp)}</span>
              <span className="font-mono text-slate-300">{c.model_name ?? "unknown model"}</span>
              <span>{formatTokens(c.prompt_tokens)} in / {formatTokens(c.completion_tokens)} out</span>
              {c.tool_calls_count > 0 && <span>{c.tool_calls_count} tool call{c.tool_calls_count === 1 ? "" : "s"}</span>}
              <span>{c.latency_ms}ms</span>
              <span className="font-semibold text-slate-200">{formatCost(c.cost_estimate)}</span>
              <Link to={`/traffic?task=${c.task_id}`} className="text-indigo-400 hover:text-indigo-300">
                task #{c.task_id} in Traffic
              </Link>
            </li>
          ))}
        </ol>
      )}
      {runTools.length > 0 && (
        <div className="flex flex-wrap gap-2">
          {runTools.map((t) => (
            <span key={t.tool_name} className="rounded-full bg-white/5 px-2.5 py-1 text-xs text-slate-300" title={`${formatTokens(t.input_tokens)} in / ${formatTokens(t.output_tokens)} out`}>
              {t.tool_name} × {t.call_count}
            </span>
          ))}
        </div>
      )}
    </div>
  );
}
export default RunDetail;
```

- [ ] **Step 2: Wire selection into `ExperimentComparison.tsx`**

`RunRow` (line 381) gains `selected: boolean; onSelect: (run: RunComparison) => void` props; the agent-name cell content wraps in a `<button onClick={() => onSelect(run)} aria-expanded={selected}>` with a chevron (`ChevronDown`/`ChevronRight` from lucide, already a dependency — check the file's existing lucide imports and extend that line). Parent holds `const [selectedKey, setSelectedKey] = useState<string | null>(null)` keyed `${agent_name}::${session_key}`, toggles on select, and renders below a selected row:
```tsx
{selectedKey === `${run.agent_name}::${run.session_key}` && (
  <tr><td colSpan={COLUMNS.length}>
    <RunDetail run={run} calls={selectRunCalls(metricsForDetail, run, grouping)} tools={breakdownTools} />
  </td></tr>
)}
```
Data: `ExperimentComparison` already fetches `runs`; it must also fetch `metrics` (`fetchJson<MetricPoint[]>(`/v1/analytics/experiments/${experimentId}/metrics`)`) and `tools` (`fetchJson<ExperimentBreakdown>(…/breakdown?group=${grouping})` → `.tools`) alongside — the same two calls `AnalyticsDashboard`/`RunBreakdown` already make. Keep `grouping`/`onGroupingChange`/verdict logic untouched.

- [ ] **Step 3: Traffic `?task=` deep link**

In `TrafficView.tsx`: initialize `selectedId` from `new URLSearchParams(window.location.search).get("task")` (numeric, else `null`); keep the existing staleness-guarded traffic fetch untouched. No router changes (`BrowserRouter` already serves `/traffic`).

- [ ] **Step 4: Verify**

Run: `npm run typecheck`
Expected: clean

Run: `npm test`
Expected: all PASS (no test changes — render-only wiring; helpers covered by Task 2 tests)

Manual (report): pick an experiment with 2+ agents, expand a run, confirm timeline order + tool chips + Traffic link lands on the task; apply each filter, confirm summary/charts/detail agree.

- [ ] **Step 5: Commit**

```bash
git add desktop/src/components/RunDetail.tsx desktop/src/components/ExperimentComparison.tsx desktop/src/components/TrafficView.tsx
git commit -m "Add expandable run detail with traffic links" -m "Per-run call timeline plus tool chips under comparison rows; filters feed the timeline; task deep-links into Traffic."
```

## Deferred (explicitly not in this plan)

- Quality-adjusted winner / cost-per-solve ranking (comparison stays cost-ordered; verdicts already visible per run).
- Custom chart tooltips beyond the formatted recharts `Tooltip` + native `<title>`s (Phase 2 scope).
- Filtering the comparison table itself (deliberately complete — filters cover metrics views only).
