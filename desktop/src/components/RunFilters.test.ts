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
