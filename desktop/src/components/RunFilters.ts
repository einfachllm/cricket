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
