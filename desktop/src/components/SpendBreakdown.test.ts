import { describe, expect, test } from 'vitest'
import { costPositions, dominatedFailedSpend, spendBreakdown } from './SpendBreakdown'
import type { RunComparison } from '../lib/api'

function run(overrides: Partial<RunComparison>): RunComparison {
  return {
    agent_name: 'kilo',
    session_key: 'sess-1',
    session_id: 'sess-1',
    sessions: 1,
    call_count: 4,
    first_seen: '2026-09-05 10:00:00',
    last_seen: '2026-09-05 10:05:00',
    wall_clock_seconds: 300,
    input_tokens: 1000,
    output_tokens: 100,
    cache_read_tokens: 500,
    tool_calls: 3,
    total_cost: 0.5,
    unpriced_calls: 0,
    busy_ms: 60_000,
    rate_limited_calls: 0,
    error_calls: 0,
    in_flight_calls: 0,
    verdict: null,
    verdict_note: null,
    ...overrides,
  } as RunComparison
}

describe('spendBreakdown', () => {
  test('buckets spend by verdict', () => {
    const spend = spendBreakdown([
      run({ session_key: 'a', verdict: 'solved', total_cost: 0.2 }),
      run({ session_key: 'b', verdict: 'solved', total_cost: 0.4 }),
      run({ session_key: 'c', verdict: 'failed', total_cost: 0.9 }),
      run({ session_key: 'd', verdict: null, total_cost: 0.1 }),
    ])

    expect(spend.totalSpend).toBeCloseTo(1.6)
    expect(spend.solvedSpend).toBeCloseTo(0.6)
    expect(spend.failedSpend).toBeCloseTo(0.9)
    expect(spend.unjudgedSpend).toBeCloseTo(0.1)
    expect(spend.countedRuns).toBe(4)
    expect(spend.excludedRuns).toBe(0)
  })

  test('a run still spending is excluded — its total is a tally, not a result', () => {
    const spend = spendBreakdown([
      run({ session_key: 'a', verdict: 'solved', total_cost: 0.2 }),
      run({ session_key: 'b', verdict: null, total_cost: 99, in_flight_calls: 1 }),
    ])

    expect(spend.countedRuns).toBe(1)
    expect(spend.excludedRuns).toBe(1)
    expect(spend.totalSpend).toBeCloseTo(0.2)
  })

  test('a run on an unpriced model is excluded — its total understates reality', () => {
    const spend = spendBreakdown([
      run({ session_key: 'a', verdict: 'failed', total_cost: 0.0, unpriced_calls: 2 }),
    ])

    expect(spend.countedRuns).toBe(0)
    expect(spend.excludedRuns).toBe(1)
  })

  test('the failure multiplier compares failed spend to an average solve', () => {
    // Two solves at $0.20/$0.40 (average $0.30) against $0.90 of failures.
    const spend = spendBreakdown([
      run({ session_key: 'a', verdict: 'solved', total_cost: 0.2 }),
      run({ session_key: 'b', verdict: 'solved', total_cost: 0.4 }),
      run({ session_key: 'c', verdict: 'failed', total_cost: 0.9 }),
    ])

    expect(spend.failureMultiplier).not.toBeNull()
    expect(spend.failureMultiplier!).toBeCloseTo(3)
  })

  test('the multiplier is null when either half of the comparison is missing', () => {
    expect(spendBreakdown([run({ verdict: 'failed', total_cost: 0.9 })]).failureMultiplier).toBeNull()
    expect(spendBreakdown([run({ verdict: 'solved', total_cost: 0.2 })]).failureMultiplier).toBeNull()
  })
})

describe('dominatedFailedSpend', () => {
  test('failed runs dearer than the cheapest solve are pure loss', () => {
    const dominated = dominatedFailedSpend([
      run({ session_key: 'a', verdict: 'solved', total_cost: 0.3 }),
      run({ session_key: 'b', verdict: 'failed', total_cost: 0.9 }),
      run({ session_key: 'c', verdict: 'failed', total_cost: 0.1 }),
    ])

    expect(dominated).toBeCloseTo(0.9)
  })

  test('null while nothing is solved — there is no bar to be dominated against', () => {
    expect(dominatedFailedSpend([run({ verdict: 'failed', total_cost: 0.9 })])).toBeNull()
  })

  test('a failure cheaper than the solve is an honest experiment, not waste', () => {
    const dominated = dominatedFailedSpend([
      run({ session_key: 'a', verdict: 'solved', total_cost: 0.5 }),
      run({ session_key: 'b', verdict: 'failed', total_cost: 0.1 }),
    ])

    expect(dominated).toBe(0)
  })
})

describe('costPositions', () => {
  test('maps costs onto a log scale between 0 and 1', () => {
    const positions = costPositions([0.01, 1, 100])
    // 0.01 and 100 are the extremes; 1 is the geometric middle.
    expect(positions[0]).toBe(0)
    expect(positions[1]).toBeCloseTo(0.5)
    expect(positions[2]).toBe(1)
  })

  test('identical costs land mid-strip instead of dividing by zero', () => {
    expect(costPositions([0.5, 0.5])).toEqual([0.5, 0.5])
  })

  test('handles empty input and non-positive costs', () => {
    expect(costPositions([])).toEqual([])
    expect(costPositions([0])).toEqual([0])
  })
})
