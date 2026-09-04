/// The cost story of one experiment's runs, in the terms the agent-economics
/// literature converged on (SWE-Effi, HAL): not "did it solve it" but "what
/// did solving cost, and what did failing cost".
///
/// Everything here is client-side arithmetic over `RunComparison`s the
/// comparison endpoint already returns — no new backend surface.
import type { RunComparison } from '../lib/api';

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

export interface SpendBreakdown {
  /// Spend bucketed by the run's verdict. Only runs whose cost is both
  /// final and fully known are counted — a run still spending has a tally,
  /// not a total, and an unpriced run's total understates reality.
  totalSpend: number;
  solvedSpend: number;
  failedSpend: number;
  unjudgedSpend: number;
  countedRuns: number;
  excludedRuns: number;
  /// How many times a failed attempt cost versus an average solved one.
  /// Null until something has both solved and failed with known cost —
  /// either half alone would make the ratio meaningless.
  failureMultiplier: number | null;
}

export function spendBreakdown(runs: RunComparison[]): SpendBreakdown {
  let totalSpend = 0;
  let solvedSpend = 0;
  let failedSpend = 0;
  let unjudgedSpend = 0;
  let countedRuns = 0;
  let excludedRuns = 0;
  let solvedCount = 0;
  let failedCount = 0;

  for (const run of runs) {
    if (!costIsKnown(run) || !runIsFinal(run)) {
      excludedRuns += 1;
      continue;
    }
    countedRuns += 1;
    totalSpend += run.total_cost;
    if (run.verdict === 'solved') {
      solvedSpend += run.total_cost;
      solvedCount += 1;
    } else if (run.verdict === 'failed') {
      failedSpend += run.total_cost;
      failedCount += 1;
    } else {
      unjudgedSpend += run.total_cost;
    }
  }

  const averageSolvedCost = solvedCount > 0 ? solvedSpend / solvedCount : null;
  const failureMultiplier =
    averageSolvedCost !== null && averageSolvedCost > 0 && failedCount > 0
      ? failedSpend / averageSolvedCost
      : null;

  return { totalSpend, solvedSpend, failedSpend, unjudgedSpend, countedRuns, excludedRuns, failureMultiplier };
}

/// Spend on failed runs that were dearer than the cheapest solved one. A
/// failure is not automatically waste — a cheap attempt that taught
/// something is what experiments are for — but a failure that cost more
/// than a known-good solve was pure loss. Null while nothing is solved,
/// because there is no bar to be dominated against.
export function dominatedFailedSpend(runs: RunComparison[]): number | null {
  const solvedCosts = runs
    .filter((run) => run.verdict === 'solved' && costIsKnown(run) && runIsFinal(run))
    .map((run) => run.total_cost);
  if (solvedCosts.length === 0) return null;

  const cheapestSolve = Math.min(...solvedCosts);
  return runs
    .filter((run) => run.verdict === 'failed' && costIsKnown(run) && runIsFinal(run) && run.total_cost >= cheapestSolve)
    .reduce((sum, run) => sum + run.total_cost, 0);
}

/// Log-scaled x position (0..1) for a cost, so $0.02 and $12 sit on one
/// readable strip. Non-positive costs clamp to the left edge — cheapest
/// possible — and runs sharing one cost all land mid-strip.
export function costPositions(costs: number[]): number[] {
  if (costs.length === 0) return [];
  const positive = costs.filter((cost) => cost > 0);
  if (positive.length === 0) return costs.map(() => 0);
  const min = Math.min(...positive);
  const max = Math.max(...positive);
  const span = Math.log10(max) - Math.log10(min);
  return costs.map((cost) =>
    cost <= 0 ? 0 : span === 0 ? 0.5 : (Math.log10(cost) - Math.log10(min)) / span,
  );
}
