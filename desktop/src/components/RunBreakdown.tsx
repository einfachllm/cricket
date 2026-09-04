import React, { useCallback, useEffect, useState } from 'react';
import { AlertCircle, Wrench } from 'lucide-react';
import { ExperimentBreakdown, PhaseSlice, RunGrouping, ToolUsage, fetchJson, formatTokens } from '../lib/api';

/// The five slices a run's calls are cut into, named for where in the task
/// they fall. Index matches the backend's 1-based `phase`.
const PHASE_LABELS = ['Early', 'Early-mid', 'Mid', 'Late-mid', 'Late'] as const;

/// Categorical slots 1-3 of the validated palette. Fixed order, assigned by
/// series identity rather than by size — a filter that drops a series must
/// never repaint the others.
const SERIES = [
  { key: 'fresh', label: 'Fresh input', color: '#2a78d6' },
  { key: 'cached', label: 'Cached input', color: '#eb6834' },
  { key: 'output', label: 'Output', color: '#1baf7a' },
] as const;

const SURFACE = '#151a23';
/// Surface-coloured gap between stacked segments — the separator is the gap,
/// never a stroke drawn around the mark.
const SEGMENT_GAP = 2;
const BAR_WIDTH = 24;
const BAND_WIDTH = 60;
const PLOT_TOP = 8;
const BASELINE = 100;

export interface RunKey {
  agent_name: string;
  session_key: string;
}

export interface StackedPhase {
  phase: number;
  fresh: number;
  cached: number;
  output: number;
  total: number;
  calls: number;
}

/// Splits a phase's tokens into the three things actually billed at
/// different rates: input the model had to read fresh, input served from
/// cache (an order of magnitude cheaper), and what it generated.
export function toStack(slice: PhaseSlice): StackedPhase {
  const cached = Math.min(slice.cache_read_tokens, slice.input_tokens);
  const fresh = slice.input_tokens - cached;
  return {
    phase: slice.phase,
    fresh,
    cached,
    output: slice.output_tokens,
    total: fresh + cached + slice.output_tokens,
    calls: slice.calls,
  };
}

export function sameRun(a: RunKey, b: RunKey): boolean {
  return a.agent_name === b.agent_name && a.session_key === b.session_key;
}

/// Every run the breakdown covers, in the order the backend returned them.
export function runsIn(phases: PhaseSlice[]): RunKey[] {
  const runs: RunKey[] = [];
  for (const slice of phases) {
    if (!runs.some((run) => sameRun(run, slice))) {
      runs.push({ agent_name: slice.agent_name, session_key: slice.session_key });
    }
  }
  return runs;
}

/// The headline the phase chart exists to make legible: how much of the run's
/// tokens were generation at the start versus at the end. Null when either
/// end of the run is missing or empty, rather than dividing by zero.
export function outputShareShift(stacks: StackedPhase[]): { first: number; last: number } | null {
  const present = stacks.filter((stack) => stack.total > 0);
  if (present.length < 2) return null;
  const first = present[0];
  const last = present[present.length - 1];
  return { first: first.output / first.total, last: last.output / last.total };
}

/// A stacked column with a 4px rounded data-end and a square foot at the
/// baseline. Built as a path rather than a `rect` because only the topmost
/// segment's top corners are rounded.
function segmentPath(x: number, y: number, width: number, height: number, roundTop: boolean): string {
  if (height <= 0) return '';
  const radius = Math.min(4, height, width / 2);
  if (!roundTop || radius <= 0) {
    return `M${x} ${y} h${width} v${height} h${-width} Z`;
  }
  return [
    `M${x} ${y + radius}`,
    `a${radius} ${radius} 0 0 1 ${radius} ${-radius}`,
    `h${width - radius * 2}`,
    `a${radius} ${radius} 0 0 1 ${radius} ${radius}`,
    `v${height - radius}`,
    `h${-width}`,
    'Z',
  ].join(' ');
}

function PhaseColumns({ stacks }: { stacks: StackedPhase[] }) {
  const max = Math.max(...stacks.map((stack) => stack.total), 1);
  const plotHeight = BASELINE - PLOT_TOP;

  return (
    <svg
      viewBox={`0 0 ${BAND_WIDTH * PHASE_LABELS.length} 122`}
      className="w-full"
      role="img"
      aria-label="Tokens per phase, split into fresh input, cached input and output"
    >
      {PHASE_LABELS.map((label, index) => {
        const stack = stacks.find((s) => s.phase === index + 1);
        const x = index * BAND_WIDTH + (BAND_WIDTH - BAR_WIDTH) / 2;
        // Bottom to top, so the gap always sits under the segment above it.
        const parts = stack
          ? SERIES.map((series) => ({ ...series, value: stack[series.key] })).filter((p) => p.value > 0)
          : [];

        let cumulative = 0;
        return (
          <g key={label}>
            {parts.map((part, partIndex) => {
              const full = (part.value / max) * plotHeight;
              const isTop = partIndex === parts.length - 1;
              const height = isTop ? full : Math.max(full - SEGMENT_GAP, 0);
              const y = BASELINE - cumulative - full;
              cumulative += full;
              const path = segmentPath(x, y, BAR_WIDTH, height, isTop);
              if (!path) return null;
              return (
                <path key={part.key} d={path} fill={part.color}>
                  <title>
                    {`${label} · ${part.label}: ${formatTokens(part.value)} tokens (${Math.round((part.value / (stack?.total ?? 1)) * 100)}%) across ${stack?.calls ?? 0} call${(stack?.calls ?? 0) === 1 ? "" : "s"}`}
                  </title>
                </path>
              );
            })}
            {!stack && (
              <line
                x1={x}
                y1={BASELINE}
                x2={x + BAR_WIDTH}
                y2={BASELINE}
                stroke="rgba(255,255,255,0.15)"
                strokeWidth={1}
              />
            )}
            <text
              x={index * BAND_WIDTH + BAND_WIDTH / 2}
              y={114}
              textAnchor="middle"
              fontSize={9}
              fill="#94a3b8"
            >
              {label}
            </text>
          </g>
        );
      })}
      <line x1={0} y1={BASELINE} x2={BAND_WIDTH * PHASE_LABELS.length} y2={BASELINE} stroke="rgba(255,255,255,0.15)" strokeWidth={1} />
    </svg>
  );
}

function ToolBars({ tools }: { tools: ToolUsage[] }) {
  if (tools.length === 0) {
    return (
      <p className="text-xs text-slate-500">
        No tool calls recorded. Older calls captured before tool names were tracked show nothing here.
      </p>
    );
  }

  const attributed = tools.reduce((sum, tool) => sum + tool.input_tokens, 0) || 1;
  const shown = tools.slice(0, 6);
  const rest = tools.slice(6);
  const restTokens = rest.reduce((sum, tool) => sum + tool.input_tokens, 0);
  const rows = rest.length > 0
    ? [...shown, { tool_name: `Other (${rest.length})`, input_tokens: restTokens, call_count: rest.reduce((s, t) => s + t.call_count, 0) }]
    : shown;

  return (
    <div className="space-y-1.5">
      {rows.map((tool) => {
        const share = tool.input_tokens / attributed;
        return (
          <div key={tool.tool_name} className="flex items-center gap-2 text-xs">
            <span className="w-28 shrink-0 truncate font-mono text-slate-400" title={tool.tool_name}>
              {tool.tool_name}
            </span>
            <div
              className="flex-1 h-2 bg-white/[0.06] rounded-sm overflow-hidden"
              title={`${tool.tool_name}: ${formatTokens(tool.input_tokens)} tokens across ${tool.call_count} call${tool.call_count === 1 ? "" : "s"}`}
            >
              {/* One hue for every bar: length carries the magnitude, so
                  shading by size would spend the colour channel twice. */}
              <div className="h-full rounded-sm" style={{ width: `${share * 100}%`, backgroundColor: SERIES[0].color }} />
            </div>
            <span className="w-14 shrink-0 text-right tabular-nums text-slate-300">
              {Math.round(share * 100)}%
            </span>
            <span className="w-16 shrink-0 text-right tabular-nums text-slate-500">
              {tool.call_count} call{tool.call_count === 1 ? '' : 's'}
            </span>
          </div>
        );
      })}
    </div>
  );
}

function RunPanel({
  run,
  phases,
  tools,
  grouping,
}: {
  run: RunKey;
  phases: PhaseSlice[];
  tools: ToolUsage[];
  grouping: RunGrouping;
}) {
  const stacks = phases.filter((slice) => sameRun(slice, run)).map(toStack);
  const shift = outputShareShift(stacks);
  const peak = Math.max(...stacks.map((stack) => stack.total), 0);
  const runTools = tools.filter((tool) => sameRun(tool, run));

  return (
    <div className="space-y-3">
      <div className="flex items-baseline justify-between gap-2">
        <span className="font-semibold text-slate-200 text-sm">{run.agent_name}</span>
        <span className="text-xs text-slate-500 font-mono truncate max-w-[10rem]" title={run.session_key}>
          {run.session_key || (grouping === 'agent' ? 'all sessions' : 'no session id')}
        </span>
      </div>

      <PhaseColumns stacks={stacks} />

      <p className="text-xs text-slate-500">
        Peak phase {formatTokens(peak)} tokens
        {shift && (
          <>
            {' · '}output {Math.round(shift.first * 100)}% → {Math.round(shift.last * 100)}% of the mix
          </>
        )}
      </p>

      <div className="pt-1 space-y-2">
        <p className="text-xs font-semibold text-slate-500 flex items-center gap-1">
          <Wrench size={11} /> Spend by tool
        </p>
        <ToolBars tools={runTools} />
      </div>
    </div>
  );
}

function Legend() {
  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-slate-400">
      {SERIES.map((series) => (
        <span key={series.key} className="inline-flex items-center gap-1.5">
          <span className="w-2.5 h-2.5 rounded-sm" style={{ backgroundColor: series.color }} />
          {series.label}
        </span>
      ))}
    </div>
  );
}

/// Where each run's money went, in two views the single total cannot give:
/// across the arc of the task, and through which tools.
///
/// Both are computable only from proxied traffic — a tool reading an agent's
/// own logs sees neither the per-call token split over time nor which tool
/// each turn actually called.
const RunBreakdown = ({
  experimentId,
  grouping,
}: {
  experimentId: string;
  grouping: RunGrouping;
}) => {
  const [breakdown, setBreakdown] = useState<ExperimentBreakdown>({ phases: [], tools: [] });
  const [error, setError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);

  const load = useCallback(async () => {
    try {
      setBreakdown(await fetchJson<ExperimentBreakdown>(
        `/v1/analytics/experiments/${experimentId}/breakdown?group=${grouping}`,
      ));
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoaded(true);
    }
  }, [experimentId, grouping]);

  useEffect(() => {
    setLoaded(false);
    load();
  }, [load]);

  const runs = runsIn(breakdown.phases);

  if (error) {
    return (
      <div className="bg-[#151a23] rounded-2xl border border-white/10 p-4">
        <p className="text-sm text-red-300">Couldn't load the breakdown: {error}</p>
      </div>
    );
  }

  if (loaded && runs.length === 0) return null;

  return (
    <div className="bg-[#151a23] rounded-2xl border border-white/10 p-4 space-y-4">
      <div className="flex items-start justify-between gap-4 flex-wrap">
        <div>
          <h3 className="text-base font-semibold text-slate-200">Where the money went</h3>
          <p className="text-sm text-slate-400 mt-1 max-w-2xl">
            Each run's calls cut into five slices of equal length. Early slices are the agent reading
            its way in; later ones are generation. Panels are scaled to their own peak, so compare the
            shape here and the totals above.
          </p>
        </div>
        <Legend />
      </div>

      {loaded ? (
        <div className="grid grid-cols-1 gap-x-8 gap-y-7">
          {runs.map((run) => (
            <RunPanel
              key={`${run.agent_name}:${run.session_key}`}
              run={run}
              phases={breakdown.phases}
              tools={breakdown.tools}
              grouping={grouping}
            />
          ))}
        </div>
      ) : (
        <p className="text-sm text-slate-500">Loading breakdown…</p>
      )}

      <p className="text-xs text-slate-500 flex items-start gap-1.5 pt-1 border-t border-white/5">
        <AlertCircle size={12} className="shrink-0 mt-0.5" />
        A turn's tokens are split across the tools it called, so shares sum to the spend of turns that
        used a tool. The true price of a tool's result is paid by the following turn, which carries it
        in context — read these as what a run leaned on, not an exact ledger.
      </p>
    </div>
  );
};

export default RunBreakdown;
