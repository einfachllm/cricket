import React, { useCallback, useEffect, useState } from 'react';
import { AlertTriangle, Check, CircleDot, Clock, HelpCircle, Trophy, X } from 'lucide-react';
import {
  RunComparison,
  Verdict,
  fetchJson,
  formatCost,
  formatTokens,
  humanizeSecs,
  setVerdict,
} from '../lib/api';

/// Whether a run's cost total is the whole story. A run that touched a model
/// missing from `pricing.yaml` sums to less than it really cost — at the
/// extreme, to $0.00 — so it must never be ranked against a fully priced one.
export function costIsKnown(run: RunComparison): boolean {
  return run.call_count > 0 && run.unpriced_calls === 0;
}

/// Whether the run is done spending. A run with a call still open has a
/// running tally, not a total, and ranking it against a finished one would
/// declare a winner that is still on the clock.
export function runIsFinal(run: RunComparison): boolean {
  return run.in_flight_calls === 0;
}

/// Cheapest first, but runs whose cost is only partly known sink to the
/// bottom rather than winning on an understated total.
export function sortRuns(runs: RunComparison[]): RunComparison[] {
  return [...runs].sort((a, b) => {
    const byKnown = Number(costIsKnown(b)) - Number(costIsKnown(a));
    if (byKnown !== 0) return byKnown;
    return a.total_cost - b.total_cost;
  });
}

export interface Ranking {
  /// Cheapest run that actually solved the task, or null if none can be named.
  winner: RunComparison | null;
  /// Next-cheapest solved run, for the ratio.
  runnerUp: RunComparison | null;
  /// How many times cheaper the winner was than the runner-up.
  ratio: number | null;
  /// Runs marked solved, whether or not they can be ranked.
  solved: number;
  /// Solved runs left out of the ranking because their cost is incomplete or
  /// still being spent.
  solvedButUnrankable: number;
}

/// Ranks the runs of one experiment on cost — but only among the ones that
/// actually solved the task.
///
/// Cost alone is the wrong question: the cheapest run of a hard task is
/// usually the agent that gave up after two calls. So an unjudged or failed
/// run can never win here, and with nothing marked solved there is simply no
/// answer to give yet.
export function rankRuns(runs: RunComparison[]): Ranking {
  const solved = runs.filter((run) => run.verdict === 'solved');
  const rankable = sortRuns(solved.filter((run) => costIsKnown(run) && runIsFinal(run)));

  const winner = rankable[0] ?? null;
  const runnerUp = rankable[1] ?? null;
  const ratio =
    winner && runnerUp && winner.total_cost > 0 ? runnerUp.total_cost / winner.total_cost : null;

  return {
    winner,
    runnerUp,
    ratio,
    solved: solved.length,
    solvedButUnrankable: solved.length - rankable.length,
  };
}

/// Every run one agent made at this task, folded together.
export interface AgentRollup {
  agent_name: string;
  runs: number;
  solved: number;
  failed: number;
  unjudged: number;
  /// Everything the agent spent here, solved attempts and wasted ones alike.
  totalCost: number;
  /// Spend per success — total spend divided by runs solved, so an agent
  /// that only lands one attempt in three carries the two it wasted. Null
  /// when it has never solved it, or when some of its spend is unpriced.
  costPerSolve: number | null;
  /// Median cost of the runs it solved, and their spread. Null until it has
  /// solved one with a fully known cost.
  medianSolvedCost: number | null;
  cheapestSolvedCost: number | null;
  dearestSolvedCost: number | null;
}

function median(values: number[]): number {
  const sorted = [...values].sort((a, b) => a - b);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2 === 1 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2;
}

/// Rolls the runs up per agent, because one run each is a sample size of one.
///
/// The number that answers "which agent is cheaper at this kind of work" is
/// not the cheapest single run but spend per success: an agent that solves
/// one attempt in three at $0.40 costs $1.20 a fix, not $0.40. With a single
/// run each this degrades to exactly the per-run comparison.
export function rollUpByAgent(runs: RunComparison[]): AgentRollup[] {
  const byAgent = new Map<string, RunComparison[]>();
  for (const run of runs) {
    byAgent.set(run.agent_name, [...(byAgent.get(run.agent_name) ?? []), run]);
  }

  const rollups = [...byAgent.entries()].map(([agent_name, agentRuns]) => {
    const solvedRuns = agentRuns.filter((run) => run.verdict === 'solved');
    const solvedCosts = solvedRuns.filter(costIsKnown).map((run) => run.total_cost);
    const totalCost = agentRuns.reduce((sum, run) => sum + run.total_cost, 0);
    // One unpriced call anywhere in the agent's attempts understates the
    // total, and an understated total would flatter it here.
    const spendIsKnown = agentRuns.every(costIsKnown);

    return {
      agent_name,
      runs: agentRuns.length,
      solved: solvedRuns.length,
      failed: agentRuns.filter((run) => run.verdict === 'failed').length,
      unjudged: agentRuns.filter((run) => run.verdict === null).length,
      totalCost,
      costPerSolve: spendIsKnown && solvedRuns.length > 0 ? totalCost / solvedRuns.length : null,
      medianSolvedCost: solvedCosts.length > 0 ? median(solvedCosts) : null,
      cheapestSolvedCost: solvedCosts.length > 0 ? Math.min(...solvedCosts) : null,
      dearestSolvedCost: solvedCosts.length > 0 ? Math.max(...solvedCosts) : null,
    };
  });

  // Agents that never solved it sort last however little they spent.
  return rollups.sort((a, b) => {
    if ((a.costPerSolve === null) !== (b.costPerSolve === null)) return a.costPerSolve === null ? 1 : -1;
    if (a.costPerSolve !== null && b.costPerSolve !== null) return a.costPerSolve - b.costPerSolve;
    return a.agent_name.localeCompare(b.agent_name);
  });
}

const VERDICT_STYLES: Record<Verdict, { chip: string; label: string; icon: typeof Check }> = {
  solved: { chip: 'bg-emerald-100 text-emerald-800 border-emerald-200', label: 'Solved', icon: Check },
  failed: { chip: 'bg-red-100 text-red-800 border-red-200', label: 'Failed', icon: X },
};

/// Three-state control: solved, failed, or not judged yet. Clicking the
/// active state clears it, so a misclick costs one more click.
function VerdictToggle({
  run,
  onChange,
}: {
  run: RunComparison;
  onChange: (run: RunComparison, verdict: Verdict | null) => void;
}) {
  const options: Verdict[] = ['solved', 'failed'];

  return (
    <div className="inline-flex rounded-lg border border-gray-200 overflow-hidden">
      {options.map((option) => {
        const active = run.verdict === option;
        const { chip, label, icon: Icon } = VERDICT_STYLES[option];
        return (
          <button
            key={option}
            type="button"
            aria-pressed={active}
            title={active ? `Clear the "${label}" verdict` : `Mark this run ${label.toLowerCase()}`}
            onClick={() => onChange(run, active ? null : option)}
            className={`flex items-center gap-1 px-2.5 py-1 text-xs font-semibold transition-colors ${
              active ? chip : 'bg-white text-gray-400 hover:bg-gray-50'
            }`}
          >
            <Icon size={12} />
            {label}
          </button>
        );
      })}
    </div>
  );
}

function Headline({ ranking, runCount }: { ranking: Ranking; runCount: number }) {
  const { winner, runnerUp, ratio, solved, solvedButUnrankable } = ranking;

  if (runCount === 0) {
    return (
      <p className="text-sm text-gray-500">
        No calls recorded under this experiment yet. Point each agent at the proxy with the same{' '}
        <code className="font-mono text-xs bg-gray-100 px-1 rounded">X-Experiment-ID</code> and a
        different <code className="font-mono text-xs bg-gray-100 px-1 rounded">X-Agent-ID</code>.
      </p>
    );
  }

  if (solved === 0) {
    return (
      <div className="flex items-start gap-3 text-sm">
        <HelpCircle size={18} className="text-amber-500 shrink-0 mt-0.5" />
        <p className="text-gray-600">
          <span className="font-semibold text-gray-800">Nothing is marked solved yet.</span> The
          proxy can see what each run spent, but not whether its work was any good — mark that
          below. Ranking on cost alone would crown whichever agent gave up first.
        </p>
      </div>
    );
  }

  if (!winner) {
    return (
      <div className="flex items-start gap-3 text-sm">
        <AlertTriangle size={18} className="text-amber-500 shrink-0 mt-0.5" />
        <p className="text-gray-600">
          {solved} run{solved === 1 ? '' : 's'} solved it, but{' '}
          {solved === 1 ? 'its total is' : 'their totals are'} not final —{' '}
          {solved === 1 ? 'it is' : 'they are'} still running, or still spending on a model with no
          entry in <code className="font-mono text-xs">pricing.yaml</code>.
        </p>
      </div>
    );
  }

  return (
    <div className="space-y-2">
      <div className="flex items-baseline gap-2 flex-wrap">
        <Trophy size={18} className="text-amber-500 shrink-0 self-center" />
        <span className="text-lg font-bold text-gray-800">{winner.agent_name}</span>
        <span className="text-gray-600">solved it for</span>
        <span className="text-lg font-bold text-emerald-700">{formatCost(winner.total_cost)}</span>
        {runnerUp && (
          <span className="text-gray-600">
            {ratio !== null && <span className="font-semibold text-gray-800">{ratio.toFixed(1)}× cheaper </span>}
            than {runnerUp.agent_name} at {formatCost(runnerUp.total_cost)}
          </span>
        )}
      </div>
      {!runnerUp && (
        <p className="text-sm text-gray-500">
          Only one run solved it — nothing to compare against yet. Run the same task under another
          agent with the same experiment id.
        </p>
      )}
      {solvedButUnrankable > 0 && (
        <p className="text-sm text-amber-700">
          {solvedButUnrankable} solved run{solvedButUnrankable === 1 ? ' is' : 's are'} left out of
          the ranking: still running, or still spending on a model with no entry in{' '}
          <code className="font-mono text-xs">pricing.yaml</code>.
        </p>
      )}
    </div>
  );
}

/// Per-agent totals. The headline names the cheapest winning *run*; this is
/// the question underneath it — which agent is cheaper at this kind of work —
/// and it is a different answer as soon as an agent needs more than one try.
function AgentRollupTable({ rollups }: { rollups: AgentRollup[] }) {
  // With one attempt each, "cost per solve" is just that run's cost again,
  // and saying so is more honest than implying a rate was measured.
  const singleAttempts = rollups.every((rollup) => rollup.runs <= 1);

  return (
    <div className="px-6 py-5 space-y-2 border-t border-gray-100 bg-gray-50/40">
      <div className="flex items-baseline justify-between gap-3 flex-wrap">
        <h4 className="text-sm font-semibold text-gray-700">Per agent</h4>
        {singleAttempts && (
          <p className="text-xs text-gray-400">
            One attempt each — a single run says little about how often an agent gets there.
          </p>
        )}
      </div>
      <table className="w-full text-sm" aria-label="Per agent">
        <thead className="text-xs uppercase text-gray-400">
          <tr>
            <th className="py-1 text-left font-semibold">Agent</th>
            <th className="py-1 text-right font-semibold">Solved</th>
            <th className="py-1 text-right font-semibold" title="Everything this agent spent here, failed attempts included">
              Total spend
            </th>
            <th
              className="py-1 text-right font-semibold"
              title="Total spend divided by the runs that solved it — a failed attempt is paid for by the next success"
            >
              Per solve
            </th>
            <th className="py-1 text-right font-semibold" title="Median cost of the runs it solved, with the spread">
              Solved runs
            </th>
          </tr>
        </thead>
        <tbody className="divide-y divide-gray-100">
          {rollups.map((rollup) => (
            <tr key={rollup.agent_name}>
              <td className="py-2 font-medium text-gray-800">{rollup.agent_name}</td>
              <td className="py-2 text-right text-gray-600">
                {rollup.solved}/{rollup.runs}
                {rollup.unjudged > 0 && (
                  <span className="text-amber-600 text-xs" title={`${rollup.unjudged} run(s) not judged yet`}>
                    {' '}
                    (+{rollup.unjudged}?)
                  </span>
                )}
              </td>
              <td className="py-2 text-right text-gray-600">{formatCost(rollup.totalCost)}</td>
              <td className="py-2 text-right font-semibold text-gray-800">
                {rollup.costPerSolve === null ? (
                  <span className="text-gray-300" title="Never solved it here, or some of its spend is unpriced">
                    –
                  </span>
                ) : (
                  formatCost(rollup.costPerSolve)
                )}
              </td>
              <td className="py-2 text-right text-gray-500 text-xs">
                {rollup.medianSolvedCost === null
                  ? '–'
                  : rollup.cheapestSolvedCost === rollup.dearestSolvedCost
                    ? formatCost(rollup.medianSolvedCost)
                    : `${formatCost(rollup.medianSolvedCost)} (${formatCost(rollup.cheapestSolvedCost)}–${formatCost(rollup.dearestSolvedCost)})`}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function RunRow({
  run,
  isWinner,
  onVerdict,
}: {
  run: RunComparison;
  isWinner: boolean;
  onVerdict: (run: RunComparison, verdict: Verdict | null) => void;
}) {
  const issues = run.error_calls + run.rate_limited_calls;

  return (
    <tr className={isWinner ? 'bg-emerald-50/60' : undefined}>
      <td className="px-4 py-3">
        <div className="flex items-center gap-2">
          {isWinner ? (
            <Trophy size={14} className="text-amber-500 shrink-0" />
          ) : (
            <CircleDot size={14} className="text-gray-300 shrink-0" />
          )}
          <span className="font-semibold text-gray-800">{run.agent_name}</span>
        </div>
        <p className="text-xs text-gray-400 font-mono truncate max-w-[16rem]" title={run.session_id ?? ''}>
          {run.session_id ?? 'no session id'}
        </p>
        <p className="text-xs text-gray-400 truncate max-w-[16rem]" title={run.models ?? ''}>
          {run.models ?? 'unknown model'}
        </p>
      </td>
      <td className="px-4 py-3">
        <VerdictToggle run={run} onChange={onVerdict} />
        {run.verdict_note && <p className="text-xs text-gray-500 mt-1">{run.verdict_note}</p>}
      </td>
      <td className="px-4 py-3 text-right">
        {!runIsFinal(run) ? (
          <span
            className="text-blue-600 text-xs"
            title={`${run.in_flight_calls} call(s) still open — this run is still spending`}
          >
            {formatCost(run.total_cost)} so far
          </span>
        ) : costIsKnown(run) ? (
          <span className="font-semibold text-gray-800">{formatCost(run.total_cost)}</span>
        ) : (
          <span
            className="text-amber-600 text-xs"
            title={`${run.unpriced_calls} of ${run.call_count} calls used a model with no price in pricing.yaml`}
          >
            ≥ {formatCost(run.total_cost)}
          </span>
        )}
      </td>
      <td className="px-4 py-3 text-right text-gray-600 whitespace-nowrap">
        {formatTokens(run.input_tokens)} <span className="text-gray-400">in</span>
        <br />
        {formatTokens(run.output_tokens)} <span className="text-gray-400">out</span>
      </td>
      <td className="px-4 py-3 text-right text-gray-600" title="Input tokens served from the prompt cache">
        {formatTokens(run.cache_read_tokens)}
      </td>
      <td className="px-4 py-3 text-right text-gray-600">{run.call_count}</td>
      <td className="px-4 py-3 text-right text-gray-600">{run.tool_calls}</td>
      <td className="px-4 py-3 text-right text-gray-600 whitespace-nowrap">
        <span className="inline-flex items-center gap-1">
          <Clock size={12} className="text-gray-400" />
          {humanizeSecs(run.wall_clock_seconds)}
        </span>
      </td>
      <td className="px-4 py-3 text-right">
        {issues > 0 ? (
          <span className="text-red-600 text-xs">
            {run.error_calls > 0 && `${run.error_calls} failed`}
            {run.error_calls > 0 && run.rate_limited_calls > 0 && ', '}
            {run.rate_limited_calls > 0 && `${run.rate_limited_calls} limited`}
          </span>
        ) : (
          <span className="text-gray-300">–</span>
        )}
      </td>
    </tr>
  );
}

const COLUMNS = [
  { label: 'Run', align: 'text-left' },
  { label: 'Solved it?', align: 'text-left' },
  { label: 'Cost', align: 'text-right' },
  { label: 'Tokens', align: 'text-right' },
  { label: 'Cached', align: 'text-right' },
  { label: 'Calls', align: 'text-right' },
  { label: 'Tools', align: 'text-right' },
  { label: 'Wall clock', align: 'text-right' },
  { label: 'Issues', align: 'text-right' },
] as const;

/// Side-by-side of every run in one experiment. The Analytics time series
/// answers "what did this experiment cost"; this answers the question that
/// actually decides which agent to reach for next time — who solved it, and
/// what did solving it cost each of them.
const ExperimentComparison = ({ experimentId }: { experimentId: string }) => {
  const [runs, setRuns] = useState<RunComparison[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);

  const load = useCallback(async () => {
    try {
      setRuns(await fetchJson<RunComparison[]>(`/v1/analytics/experiments/${experimentId}/comparison`));
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoaded(true);
    }
  }, [experimentId]);

  useEffect(() => {
    setLoaded(false);
    load();
  }, [load]);

  const onVerdict = useCallback(
    async (run: RunComparison, verdict: Verdict | null) => {
      // Optimistic: the toggle should feel instant, and `load()` right after
      // replaces the guess with whatever the backend actually stored.
      setRuns((current) =>
        current.map((r) =>
          r.agent_name === run.agent_name && r.session_key === run.session_key ? { ...r, verdict } : r,
        ),
      );
      try {
        await setVerdict(run, verdict);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      }
      load();
    },
    [load],
  );

  const ordered = sortRuns(runs);
  const ranking = rankRuns(runs);

  return (
    <div className="bg-white rounded-xl shadow-sm border border-gray-100 overflow-hidden">
      <div className="p-6 pb-4 space-y-3">
        <h3 className="text-lg font-semibold text-gray-700">Which run solved it cheaper?</h3>
        {error ? (
          <p className="text-sm text-red-700">Couldn't load the comparison: {error}</p>
        ) : loaded ? (
          <Headline ranking={ranking} runCount={runs.length} />
        ) : (
          <p className="text-sm text-gray-400">Loading runs…</p>
        )}
      </div>

      {ordered.length > 0 && (
        <div className="overflow-x-auto">
          <table className="w-full text-sm" aria-label="Runs">
            <thead className="bg-gray-50 text-xs uppercase text-gray-500">
              <tr>
                {COLUMNS.map((column) => (
                  <th key={column.label} className={`px-4 py-2 font-semibold ${column.align}`}>
                    {column.label}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody className="divide-y divide-gray-100">
              {ordered.map((run) => (
                <RunRow
                  key={`${run.agent_name}:${run.session_key}`}
                  run={run}
                  isWinner={
                    ranking.winner?.agent_name === run.agent_name &&
                    ranking.winner?.session_key === run.session_key
                  }
                  onVerdict={onVerdict}
                />
              ))}
            </tbody>
          </table>
        </div>
      )}

      {ordered.length > 0 && <AgentRollupTable rollups={rollUpByAgent(runs)} />}
    </div>
  );
};

export default ExperimentComparison;
