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
