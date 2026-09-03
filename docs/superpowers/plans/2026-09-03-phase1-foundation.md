# Phase 1 Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Unify desktop fetch path and design tokens so Traffic/Analytics render via shared primitives with no hardcoded host.

**Architecture:** Add `theme/tokens` + `ui/` primitives first, then migrate the two drifted views onto `lib/api.ts fetchJson` and `page-wrap/surface`. No backend change in Phase 1.

**Tech Stack:** React 18 + Vite + Tailwind v3 + TypeScript 5 + vitest + testing-library

## Global Constraints

- Run frontend commands from `desktop/`: `npm test` and `npm run typecheck` must pass.
- Never run bare `cargo fmt`; format only files created in this change (no Rust change in Phase 1 anyway).
- Every changed line must trace to this plan; no drive-by refactors.
- Don't log prompt or response content anywhere new.
- Attribution precedence unchanged: `/r/…` > headers > fingerprints (untouched in Phase 1).

---

### Task 1: Theme tokens + timestamp helper

**Files:**
- Create: `desktop/src/theme/tokens.ts`
- Create: `desktop/src/theme/tokens.test.ts`
- Modify: `desktop/src/lib/api.ts`

**Interfaces:**
- Consumes: nothing
- Produces: `DARK` token map, `formatTimestampUtc(iso: string): string` from `lib/api.ts` used by Task 4

- [ ] **Step 1: Write failing test for tokens + timestamp**

```typescript
import { expect, test } from "vitest";
import { DARK } from "./tokens";
import { formatTimestampUtc } from "../lib/api";

test("dark tokens expose shell surfaces", () => {
  expect(DARK.appBg).toBe("#0b0e14");
  expect(DARK.cardBg).toBe("#151a23");
  expect(DARK.sidebarBg).toBe("#11131a");
});

test("timestamp helper formats UTC without Z hack at call site", () => {
  expect(formatTimestampUtc("2026-09-03T10:20:30")).toBe(
    new Date("2026-09-03T10:20:30Z").toLocaleTimeString()
  );
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `npm test -- src/theme/tokens.test.ts`
Expected: FAIL with "Cannot find module './tokens'" / "formatTimestampUtc is not a function"

- [ ] **Step 3: Write minimal implementation**

`desktop/src/theme/tokens.ts`:
```typescript
export const DARK = {
  appBg: "#0b0e14",
  cardBg: "#151a23",
  sidebarBg: "#11131a",
  accent: "#6366f1",
  accentCyan: "#22d3ee",
} as const;
```

Append to `desktop/src/lib/api.ts`:
```typescript
export function formatTimestampUtc(iso: string | null | undefined): string {
  if (!iso) return "–";
  const normalized = iso.endsWith("Z") ? iso : `${iso}Z`;
  return new Date(normalized).toLocaleTimeString();
}
```

- [ ] **Step 4: Run tests to verify pass**

Run: `npm test -- src/theme/tokens.test.ts`
Expected: PASS (2 tests)

- [ ] **Step 5: Commit**

```bash
git add desktop/src/theme/tokens.ts desktop/src/theme/tokens.test.ts desktop/src/lib/api.ts
git commit -m "Add dark tokens and UTC timestamp helper" -m "Phase 1 foundation: single token map and timestamp helper so views stop hand-rolling colors and Z hacks."
```

### Task 2: Shared UI primitives

**Files:**
- Create: `desktop/src/components/ui/Card.tsx`
- Create: `desktop/src/components/ui/Empty.tsx`
- Create: `desktop/src/components/ui/Skeleton.tsx`
- Create: `desktop/src/components/ui/ui.test.tsx`

**Interfaces:**
- Consumes: `DARK` tokens for classnames (visual only, no import required)
- Produces: `Card`, `Empty`, `Skeleton` used by Tasks 3-4

- [ ] **Step 1: Write failing test**

```typescript
import { render, screen } from "@testing-library/react";
import { expect, test } from "vitest";
import { Card } from "./Card";
import { Empty } from "./Empty";

test("card renders surface children", () => {
  render(<Card><span>hello card</span></Card>);
  expect(screen.getByText("hello card")).toBeInTheDocument();
});

test("empty renders title and hint", () => {
  render(<Empty title="No traffic" hint="Point an agent at the proxy" />);
  expect(screen.getByText("No traffic")).toBeInTheDocument();
  expect(screen.getByText("Point an agent at the proxy")).toBeInTheDocument();
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `npm test -- src/components/ui/ui.test.tsx`
Expected: FAIL with "Cannot find module './Card'"

- [ ] **Step 3: Write minimal implementation**

`desktop/src/components/ui/Card.tsx`:
```typescript
import React from "react";

export function Card({ children, className = "" }: { children: React.ReactNode; className?: string }) {
  return <div className={`surface p-6 ${className}`}>{children}</div>;
}
export default Card;
```

`desktop/src/components/ui/Empty.tsx`:
```typescript
import React from "react";

export function Empty({ title, hint }: { title: string; hint?: string }) {
  return (
    <div className="empty-panel">
      <h2>{title}</h2>
      {hint ? <p>{hint}</p> : null}
    </div>
  );
}
export default Empty;
```

`desktop/src/components/ui/Skeleton.tsx`:
```typescript
import React from "react";

export function Skeleton({ className = "h-64" }: { className?: string }) {
  return <div className={`animate-pulse rounded-xl bg-slate-200/70 ${className}`} aria-label="Loading" />;
}
export default Skeleton;
```

- [ ] **Step 4: Run tests to verify pass**

Run: `npm test -- src/components/ui/ui.test.tsx`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add desktop/src/components/ui/
git commit -m "Add shared Card Empty Skeleton primitives" -m "Phase 1 needs one surface and loading language before migrating drifted views."
```

### Task 3: Migrate AnalyticsDashboard onto fetchJson + surface

**Files:**
- Modify: `desktop/src/components/AnalyticsDashboard.tsx`

**Interfaces:**
- Consumes: `fetchJson` from `../lib/api`, `Card`/`Empty`/`Skeleton` from `./ui/*`
- Produces: identical rendered output with no hardcoded host, `page-wrap` wrapper

- [ ] **Step 1: Write failing check (no test file — grep gate)**

```bash
grep -rn "localhost:8081" desktop/src/components/AnalyticsDashboard.tsx
```

Expected: two hits (lines 49, 60) — proves hardcoded host still present.

- [ ] **Step 2: Replace fetch + wrapper + remove unused Bar imports**

Replace lines 1-6 import block:
```typescript
import React, { useEffect, useState } from 'react';
import { LineChart, Line, XAxis, YAxis, CartesianGrid, Tooltip, Legend, ResponsiveContainer } from 'recharts';
import { Activity, TrendingUp, AlertCircle, DollarSign } from 'lucide-react';
import ExperimentComparison from './ExperimentComparison';
import RunBreakdown from './RunBreakdown';
import type { RunGrouping } from '../lib/api';
import { fetchJson } from '../lib/api';
import { Card } from './ui/Card';
import { Empty } from './ui/Empty';
import { Skeleton } from './ui/Skeleton';
```

Replace `fetchExperiments` body:
```typescript
const fetchExperiments = async () => {
  try {
    const data = await fetchJson<Experiment[]>('/v1/analytics/experiments');
    setExperiments(data);
  } catch (error) {
    console.error('Error fetching experiments:', error);
  }
};
```

Replace `fetchMetrics` body:
```typescript
const fetchMetrics = async (id: string) => {
  setLoading(true);
  try {
    const data = await fetchJson<MetricData[]>(`/v1/analytics/experiments/${id}/metrics`);
    setMetrics(data);
  } catch (error) {
    console.error('Error fetching metrics:', error);
  } finally {
    setLoading(false);
  }
};
```

Replace outer `<div className="p-6 space-y-6">` with `<div className="page-wrap">`.
Replace each `bg-white p-6 rounded-xl shadow-sm border border-gray-100` summary/section div with `<Card>` (keep inner content unchanged).
Replace loading spinner block with `<Skeleton className="h-64" />` and empty state with `<Empty title="Select an experiment" hint="Select an experiment to view detailed metrics" />`.

- [ ] **Step 3: Verify host gone + typecheck + tests**

Run: `grep -rn "localhost:8081" desktop/src/components/AnalyticsDashboard.tsx`
Expected: no output

Run: `npm run typecheck`
Expected: clean

Run: `npm test -- src/components/ExperimentComparison.test.tsx src/components/RunBreakdown.test.tsx`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add desktop/src/components/AnalyticsDashboard.tsx
git commit -m "Route AnalyticsDashboard via fetchJson and surface" -m "Removes hardcoded 8081 host and style drift; output identical, now on shared Card and Empty states."
```

### Task 4: Migrate TrafficView onto fetchJson + surface + timestamp helper

**Files:**
- Modify: `desktop/src/components/TrafficView.tsx`

**Interfaces:**
- Consumes: `fetchJson`, `formatTimestampUtc` from `../lib/api`
- Produces: identical table with centralized fetch and UTC timestamp

- [ ] **Step 1: Replace imports and fetchTasks**

Replace line 3:
```typescript
import { API_BASE, fetchJson, formatCost, formatTimestampUtc } from '../lib/api';
```

Replace `fetchTasks`:
```typescript
const fetchTasks = async () => {
  setLoading(true);
  try {
    const data = await fetchJson<TaskSummary[]>('/v1/analytics/tasks');
    setTasks(data);
  } catch (error) {
    console.error('Error fetching tasks:', error);
  } finally {
    setLoading(false);
  }
};
```

Replace traffic fetch `fetch(`${API_BASE}/v1/analytics/tasks/${selectedId}/traffic`, ...)` with `fetchJson<TaskTraffic>(`/v1/analytics/tasks/${selectedId}/traffic`)` — keep AbortController guard by passing `signal` through `fetchJson` second arg `{ signal: controller.signal }`.

Replace timestamp cell `{new Date(task.timestamp + 'Z').toLocaleTimeString()}` with `{formatTimestampUtc(task.timestamp)}`.

Replace outer `<div className="p-6 space-y-6">` with `<div className="page-wrap">` and `bg-white rounded-xl shadow-sm border border-gray-100` wrappers with `surface` class (keep table markup unchanged).

- [ ] **Step 2: Verify + typecheck + tests**

Run: `grep -rn "API_BASE}/v1/analytics" desktop/src/components/TrafficView.tsx`
Expected: no output (all via fetchJson path strings)

Run: `npm run typecheck`
Expected: clean

Run: `npm test`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add desktop/src/components/TrafficView.tsx desktop/src/lib/api.ts
git commit -m "Route TrafficView via fetchJson and timestamp helper" -m "Centralizes backend URL so Tauri and custom ports work; removes Z hack at call site."
```
