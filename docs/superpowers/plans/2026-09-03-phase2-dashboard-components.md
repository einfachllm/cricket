# Phase 2 Dashboard Components Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the Phase 1 foundation into a consistent dark dev-dashboard: live backend status, dark cards, searchable traffic with readable payloads, and insightful analytics charts.

**Architecture:** Migrate the App shell and remaining views onto `DARK` tokens + `ui/` primitives first, then add traffic search/pagination/JSON-tree and analytics area/bar charts. No backend change; no new dependencies (pagination instead of virtualization, hand-rolled JSON tree, recharts already present).

**Tech Stack:** React 18 + Vite + Tailwind v3 + TypeScript 5 + recharts 3 + vitest + testing-library

## Global Constraints

- Run frontend commands from `desktop/`: `npm test` and `npm run typecheck` must pass.
- Never run bare `cargo fmt`; no Rust change in Phase 2 (no backend change).
- Every changed line must trace to this plan; no drive-by refactors.
- Don't log prompt or response content anywhere new; JSON tree renders already-captured traffic only.
- Attribution precedence unchanged: `/r/…` > headers > fingerprints (untouched).

---

### Task 1: Dark shell + live backend pill

**Files:**
- Modify: `desktop/src/App.tsx:78-90`
- Modify: `desktop/src/App.test.tsx`

**Interfaces:**
- Consumes: `useSessions()` (`error`, `loaded`) from `desktop/src/hooks/useSessions.tsx`, `DARK` from `desktop/src/theme/tokens.ts`
- Produces: `BackendPill` behavior (ready vs unreachable) relied on by nothing else; shell dark classes consumed visually by Tasks 2-4

- [ ] **Step 1: Update the backend-readiness test first (it asserts the old static pill)**

Replace `desktop/src/App.test.tsx` second test:
```typescript
test("renders backend status pill", () => {
  render(<App />)
  const statusElement = screen.getByText(/Backend (ready|unreachable)/i)
  expect(statusElement).toBeInTheDocument()
})
```
Keep the first test (`/Harnesswurm/i`) unchanged.

- [ ] **Step 2: Run test to verify it passes before and after (guard, not red)**

Run: `npm test -- src/App.test.tsx`
Expected: PASS (regex matches old static "Backend ready" too)

- [ ] **Step 3: Implement live pill + dark shell in `App.tsx`**

Replace the static pill block (lines 85-87):
```typescript
import { useSessions } from "./hooks/useSessions"

function BackendPill() {
  const { error, loaded } = useSessions();
  const unreachable = error !== null;
  return (
    <div className="hidden items-center gap-2 rounded-full border border-slate-200 bg-white px-3 py-1.5 text-xs font-medium text-slate-500 shadow-sm sm:flex">
      <span className={`h-1.5 w-1.5 rounded-full ${unreachable ? "bg-red-500" : "bg-emerald-500"}`} />
      {unreachable ? "Backend unreachable" : loaded ? "Backend ready" : "Backend…"}
    </div>
  );
}
```
Use `<BackendPill />` in place of the old div. `Layout` already renders inside `SessionsProvider`, so the hook resolves.

Darken the shell: `app-shell` div `bg-[#f7f8fb]` → `bg-[#0b0e14]`, header `bg-[#f7f8fb]/90` → `bg-[#0b0e14]/90`, header title `text-slate-900` → `text-slate-100`, eyebrow stays `text-slate-400`. Sidebar untouched.

- [ ] **Step 4: Verify**

Run: `npm test -- src/App.test.tsx`
Expected: PASS

Run: `npm run typecheck`
Expected: clean

- [ ] **Step 5: Commit**

```bash
git add desktop/src/App.tsx desktop/src/App.test.tsx
git commit -m "Add live backend pill and dark shell" -m "Header status now reflects useSessions error instead of a static label; shell moves to DARK app background."
```

### Task 2: Dark cards + Analytics nits

**Files:**
- Modify: `desktop/src/components/AgentStatusView.tsx:116-121, 152-170`
- Modify: `desktop/src/components/AnalyticsDashboard.tsx`

**Interfaces:**
- Consumes: `DARK` tokens, `formatCost` from `../lib/api`
- Produces: dark `SessionCard`, fixed cost/nit classes; no new exports

- [ ] **Step 1: Restyle `SessionCard` + footer for dark surface**

Replace the card wrapper classes (line 121) `bg-white` → `bg-[#151a23]`, `text-gray-800` → `text-slate-100`, `text-gray-400` → `text-slate-400`, `text-gray-500` → `text-slate-400`, footer `border-gray-50` → `border-white/5`, cost `text-gray-700` → `text-slate-200`. Keep `STATE_STYLES` chips, `accent` border, `attentionRing` unchanged — pastel chips stay legible on dark cards.

`QuotaBar` track `bg-gray-200` → `bg-white/10`; labels `text-gray-500` → `text-slate-400`.

- [ ] **Step 2: Fix Analytics nits (same task, same theme)**

In `AnalyticsDashboard.tsx`: `text-gray/70` → `text-slate-500`; cost cell `{summary.totalCost < 0.01 ? ...}` → `{formatCost(summary.totalCost)}` (import `formatCost` from `../lib/api`); section headings `text-gray-800` → `text-slate-100` to match dark `Card`.

- [ ] **Step 3: Verify**

Run: `npm test -- src/components/AgentStatusView.test.tsx`
Expected: PASS (sort/state logic untouched)

Run: `npm run typecheck`
Expected: clean

Run: `npm test`
Expected: 64+ PASS (count grows only if earlier tasks added tests)

- [ ] **Step 4: Commit**

```bash
git add desktop/src/components/AgentStatusView.tsx desktop/src/components/AnalyticsDashboard.tsx
git commit -m "Darken session cards and fix analytics nits" -m "Cards move to DARK surface with slate text; chips and sort order untouched. Fixes invalid text-gray/70 and routes cost via formatCost."
```

### Task 3: Traffic search + pagination + JSON tree

**Files:**
- Create: `desktop/src/components/CollapsibleJson.tsx`
- Create: `desktop/src/components/CollapsibleJson.test.tsx`
- Modify: `desktop/src/components/TrafficView.tsx`

**Interfaces:**
- Consumes: `formatJson` behavior (moved, not duplicated); `TaskSummary`/`TaskTraffic` types stay in `TrafficView.tsx`
- Produces: `CollapsibleJson({ raw: string | null })`, `PAGE_SIZE = 50`, `filterTasks(tasks, query, agentFilter, questionsOnly)` pure helper exported from `TrafficView.tsx` for tests

- [ ] **Step 1: Write failing test for filter helper + JSON tree**

```typescript
import { expect, test } from "vitest";
import { render, screen } from "@testing-library/react";
import { filterTasks } from "./TrafficView";
import { CollapsibleJson } from "./CollapsibleJson";

const rows = [
  { task_id: 1, agent_name: "opencode", task_description: "fix login", agent_question_text: null },
  { task_id: 2, agent_name: "claude", task_description: "refactor db", agent_question_text: "which table?" },
] as any;

test("filter matches agent, description, and questionsOnly", () => {
  expect(filterTasks(rows, "login", "all", false)).toHaveLength(1);
  expect(filterTasks(rows, "", "claude", false)).toHaveLength(1);
  expect(filterTasks(rows, "", "all", true)).toHaveLength(1);
});

test("json tree renders top-level keys collapsed-safe", () => {
  render(<CollapsibleJson raw='{"a":1,"b":{"c":[1,2]}}' />);
  expect(screen.getByText("a")).toBeInTheDocument();
  expect(screen.getByText("b")).toBeInTheDocument();
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `npm test -- src/components/CollapsibleJson.test.tsx`
Expected: FAIL with "Cannot find module './CollapsibleJson'" / "filterTasks is not a function"

- [ ] **Step 3: Implement `CollapsibleJson.tsx` (recursive, `<details>`-based, depth-capped)**

```typescript
import React from "react";

function Node({ name, value, depth }: { name: string; value: unknown; depth: number }) {
  if (value === null) return <div><span className="text-slate-400">{name}</span>: <span className="text-slate-500">null</span></div>;
  if (Array.isArray(value)) return (
    <details open={depth < 1}>
      <summary className="cursor-pointer text-slate-300">{name} <span className="text-slate-500">[{value.length}]</span></summary>
      <div className="pl-4">{value.map((v, i) => <Node key={i} name={String(i)} value={v} depth={depth + 1} />)}</div>
    </details>
  );
  if (typeof value === "object") {
    const entries = Object.entries(value as Record<string, unknown>);
    return (
      <details open={depth < 1}>
        <summary className="cursor-pointer text-slate-300">{name} <span className="text-slate-500">{"{...}"}</span></summary>
        <div className="pl-4">{entries.map(([k, v]) => <Node key={k} name={k} value={v} depth={depth + 1} />)}</div>
      </details>
    );
  }
  const str = typeof value === "string" ? value : JSON.stringify(value);
  return <div className="break-words"><span className="text-sky-300">{name}</span>: <span className="text-slate-200">{str.length > 500 ? str.slice(0, 500) + "…" : str}</span></div>;
}

export function CollapsibleJson({ raw }: { raw: string | null }) {
  if (!raw) return <p className="text-sm text-slate-500">(no data captured)</p>;
  let parsed: unknown;
  try { parsed = JSON.parse(raw); } catch { return <pre className="whitespace-pre-wrap break-words text-xs text-slate-200">{raw.slice(0, 4000)}</pre>; }
  if (typeof parsed !== "object" || parsed === null) return <pre className="text-xs text-slate-200">{JSON.stringify(parsed)}</pre>;
  return <div className="font-mono text-xs space-y-0.5">{Object.entries(parsed as Record<string, unknown>).map(([k, v]) => <Node key={k} name={k} value={v} depth={0} />)}</div>;
}
export default CollapsibleJson;
```

In `TrafficView.tsx`: export `filterTasks`, add `query` + `page` state, search input in the toolbar, `PAGE_SIZE = 50` slice with Prev/Next + "Page X of Y", reset page on filter change, replace both `<pre>` payload blocks with `<CollapsibleJson raw={...} />` inside the existing dark containers. Keep `AbortController` guard and `formatTimestampUtc` untouched.

- [ ] **Step 4: Verify**

Run: `npm test -- src/components/CollapsibleJson.test.tsx`
Expected: PASS

Run: `npm run typecheck`
Expected: clean

Run: `npm test`
Expected: all PASS

- [ ] **Step 5: Commit**

```bash
git add desktop/src/components/CollapsibleJson.tsx desktop/src/components/CollapsibleJson.test.tsx desktop/src/components/TrafficView.tsx
git commit -m "Add traffic search pagination and JSON tree" -m "50-row pages with search keep 1k-row histories responsive; collapsible payloads replace raw pre blocks."
```

### Task 4: Analytics area + per-model charts with tooltips

**Files:**
- Modify: `desktop/src/components/AnalyticsDashboard.tsx`

**Interfaces:**
- Consumes: `MetricData` (add `model_name` aggregation), `Card`/`Skeleton`/`Empty` from Task lineage, recharts `AreaChart/Area/BarChart/Bar`
- Produces: `tokensByModel(metrics)` pure helper exported for tests; no backend change

- [ ] **Step 1: Write failing test for per-model aggregation**

Create `desktop/src/components/AnalyticsDashboard.test.ts`:
```typescript
import { expect, test } from "vitest";
import { tokensByModel } from "./AnalyticsDashboard";

test("tokensByModel sums prompt+completion per model, nulls to unknown", () => {
  const rows = [
    { model_name: "gpt-4o", prompt_tokens: 100, completion_tokens: 50 },
    { model_name: "gpt-4o", prompt_tokens: 0, completion_tokens: 10 },
    { model_name: null, prompt_tokens: 5, completion_tokens: 5 },
  ] as any;
  expect(tokensByModel(rows)).toEqual([
    { model: "gpt-4o", tokens: 160 },
    { model: "unknown", tokens: 10 },
  ]);
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `npm test -- src/components/AnalyticsDashboard.test.ts`
Expected: FAIL with "tokensByModel is not a function"

- [ ] **Step 3: Implement helper + charts**

```typescript
export function tokensByModel(metrics: MetricData[]): { model: string; tokens: number }[] {
  const sums = new Map<string, number>();
  for (const m of metrics) {
    const key = m.model_name ?? "unknown";
    sums.set(key, (sums.get(key) ?? 0) + m.prompt_tokens + m.completion_tokens);
  }
  return [...sums.entries()].map(([model, tokens]) => ({ model, tokens })).sort((a, b) => b.tokens - a.tokens);
}
```

Charts: convert token `LineChart` to `AreaChart` (prompt_tokens `#3b82f6`, completion_tokens `#10b981`, cache_read_tokens `#a855f7`, `dot={false}`, formatted `Tooltip` with `toLocaleString()` values); add per-model horizontal `BarChart` (`layout="vertical"`, `XAxis type="number"`, `YAxis type="category" dataKey="model" width={120}`) fed by `tokensByModel(metrics)`; keep `ExperimentComparison` + `RunBreakdown` wiring unchanged. Re-add `BarChart, Bar` to the recharts import (removed in Phase 1 as unused — now used).

Also in `desktop/src/components/RunBreakdown.tsx`: add native `<title>` tooltips to `PhaseColumns` segments and `ToolBars` bars so hover shows values (existing `<title>`-only coverage becomes value-bearing):
```tsx
<title>{`${label}: ${formatTokens(value)} tokens across ${calls} call${calls === 1 ? "" : "s"}`}</title>
```
`formatTokens` is already imported there. No new test (render-only; existing `RunBreakdown.test.tsx` pure-function tests must keep passing). Include the file in the Task 4 commit.

- [ ] **Step 4: Verify**

Run: `npm test -- src/components/AnalyticsDashboard.test.ts`
Expected: PASS

Run: `npm run typecheck`
Expected: clean

Run: `npm test`
Expected: all PASS

- [ ] **Step 5: Commit**

```bash
git add desktop/src/components/AnalyticsDashboard.tsx desktop/src/components/AnalyticsDashboard.test.ts desktop/src/components/RunBreakdown.tsx
git commit -m "Add token area and per-model charts" -m "Area chart splits prompt/completion/cache; per-model bars reuse tokensByModel aggregation."
```
