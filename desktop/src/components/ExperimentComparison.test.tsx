import { fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { afterEach, describe, expect, test, vi } from 'vitest'
import ExperimentComparison, {
  costIsKnown,
  fragmentedAgents,
  rankRuns,
  rollUpByAgent,
  runIsFinal,
  sortRuns,
} from './ExperimentComparison'
import type { RunComparison } from '../lib/api'

function run(overrides: Partial<RunComparison>): RunComparison {
  return {
    agent_name: 'kilo',
    session_key: 'run-a',
    session_id: 'run-a',
    call_count: 4,
    first_seen: '2026-08-31 12:00:00',
    last_seen: '2026-08-31 12:04:00',
    wall_clock_seconds: 240,
    input_tokens: 120_000,
    output_tokens: 4_000,
    cache_read_tokens: 90_000,
    tool_calls: 9,
    total_cost: 0.4,
    unpriced_calls: 0,
    busy_ms: 42_000,
    rate_limited_calls: 0,
    error_calls: 0,
    in_flight_calls: 0,
    sessions: 1,
    models: 'gpt-4o',
    providers: 'openai',
    verdict: null,
    verdict_note: null,
    ...overrides,
  }
}

function mockBackend(runs: RunComparison[]) {
  const fetchMock = vi.fn(async () => ({ ok: true, status: 200, json: async () => runs }))
  vi.stubGlobal('fetch', fetchMock)
  return fetchMock
}

afterEach(() => {
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

describe('rankRuns', () => {
  test('names the cheapest run that actually solved the task', () => {
    const { winner, runnerUp, ratio } = rankRuns([
      run({ agent_name: 'claude-code', session_key: 'b', total_cost: 1.24, verdict: 'solved' }),
      run({ agent_name: 'kilo', session_key: 'a', total_cost: 0.4, verdict: 'solved' }),
    ])

    expect(winner?.agent_name).toBe('kilo')
    expect(runnerUp?.agent_name).toBe('claude-code')
    expect(ratio).toBeCloseTo(3.1)
  })

  test('never crowns a run that gave up, however little it spent', () => {
    const { winner, solved } = rankRuns([
      run({ agent_name: 'quitter', session_key: 'a', total_cost: 0.01, verdict: 'failed' }),
      run({ agent_name: 'kilo', session_key: 'b', total_cost: 0.9, verdict: 'solved' }),
    ])

    expect(winner?.agent_name).toBe('kilo')
    expect(solved).toBe(1)
  })

  test('has no answer until at least one run is judged', () => {
    const ranking = rankRuns([
      run({ session_key: 'a', total_cost: 0.4 }),
      run({ session_key: 'b', total_cost: 1.2 }),
    ])

    expect(ranking.winner).toBeNull()
    expect(ranking.solved).toBe(0)
  })

  test('excludes a solved run whose cost is only partly priced, and says so', () => {
    const ranking = rankRuns([
      run({ agent_name: 'unpriced', session_key: 'a', total_cost: 0, unpriced_calls: 4, verdict: 'solved' }),
      run({ agent_name: 'kilo', session_key: 'b', total_cost: 0.9, verdict: 'solved' }),
    ])

    expect(ranking.winner?.agent_name).toBe('kilo')
    expect(ranking.solvedButUnrankable).toBe(1)
  })

  test('will not crown a run that is still spending', () => {
    const ranking = rankRuns([
      run({ agent_name: 'still-going', session_key: 'a', total_cost: 0.1, in_flight_calls: 1, verdict: 'solved' }),
      run({ agent_name: 'kilo', session_key: 'b', total_cost: 0.9, verdict: 'solved' }),
    ])

    expect(ranking.winner?.agent_name).toBe('kilo')
    expect(ranking.solvedButUnrankable).toBe(1)
  })

  test('reports no ratio when the only solved run is free or alone', () => {
    expect(rankRuns([run({ verdict: 'solved' })]).ratio).toBeNull()
    expect(
      rankRuns([
        run({ session_key: 'a', total_cost: 0, verdict: 'solved' }),
        run({ session_key: 'b', total_cost: 1, verdict: 'solved' }),
      ]).ratio,
    ).toBeNull()
  })
})

describe('sortRuns', () => {
  test('sinks runs with an incomplete price below fully priced ones', () => {
    const ordered = sortRuns([
      run({ session_key: 'partly', total_cost: 0.0, unpriced_calls: 2 }),
      run({ session_key: 'dear', total_cost: 1.5 }),
      run({ session_key: 'cheap', total_cost: 0.2 }),
    ])

    expect(ordered.map((r) => r.session_key)).toEqual(['cheap', 'dear', 'partly'])
  })

  test('does not mutate the array it was given', () => {
    const input = [run({ session_key: 'b', total_cost: 2 }), run({ session_key: 'a', total_cost: 1 })]
    sortRuns(input)
    expect(input.map((r) => r.session_key)).toEqual(['b', 'a'])
  })
})

describe('rollUpByAgent', () => {
  test('charges an agent for the attempts it wasted before landing one', () => {
    const [kilo] = rollUpByAgent([
      run({ agent_name: 'kilo', session_key: 'a', total_cost: 0.4, verdict: 'failed' }),
      run({ agent_name: 'kilo', session_key: 'b', total_cost: 0.4, verdict: 'failed' }),
      run({ agent_name: 'kilo', session_key: 'c', total_cost: 0.4, verdict: 'solved' }),
    ])

    expect(kilo.runs).toBe(3)
    expect(kilo.solved).toBe(1)
    expect(kilo.failed).toBe(2)
    expect(kilo.totalCost).toBeCloseTo(1.2)
    // One fix in three attempts costs three attempts, not one.
    expect(kilo.costPerSolve).toBeCloseTo(1.2)
    expect(kilo.medianSolvedCost).toBeCloseTo(0.4)
  })

  test('ranks a first-time solver above a cheaper agent that never got there', () => {
    const ordered = rollUpByAgent([
      run({ agent_name: 'quitter', session_key: 'a', total_cost: 0.02, verdict: 'failed' }),
      run({ agent_name: 'kilo', session_key: 'b', total_cost: 0.9, verdict: 'solved' }),
    ])

    expect(ordered.map((r) => r.agent_name)).toEqual(['kilo', 'quitter'])
    expect(ordered[1].costPerSolve).toBeNull()
  })

  test('reports the median and spread across an agent’s solved runs', () => {
    const [kilo] = rollUpByAgent([
      run({ agent_name: 'kilo', session_key: 'a', total_cost: 0.2, verdict: 'solved' }),
      run({ agent_name: 'kilo', session_key: 'b', total_cost: 0.5, verdict: 'solved' }),
      run({ agent_name: 'kilo', session_key: 'c', total_cost: 1.1, verdict: 'solved' }),
    ])

    expect(kilo.medianSolvedCost).toBeCloseTo(0.5)
    expect(kilo.cheapestSolvedCost).toBeCloseTo(0.2)
    expect(kilo.dearestSolvedCost).toBeCloseTo(1.1)
  })

  test('withholds a per-solve figure while any of the spend is unpriced', () => {
    const [kilo] = rollUpByAgent([
      run({ agent_name: 'kilo', session_key: 'a', total_cost: 0.2, unpriced_calls: 1, verdict: 'solved' }),
    ])

    expect(kilo.costPerSolve).toBeNull()
  })

  test('counts runs nobody has judged yet separately', () => {
    const [kilo] = rollUpByAgent([
      run({ agent_name: 'kilo', session_key: 'a', total_cost: 0.2, verdict: 'solved' }),
      run({ agent_name: 'kilo', session_key: 'b', total_cost: 0.3 }),
    ])

    expect(kilo.unjudged).toBe(1)
    expect(kilo.solved).toBe(1)
  })
})

describe('fragmentedAgents', () => {
  test('spots an agent whose attempt is spread over several runs', () => {
    expect(fragmentedAgents([
      run({ agent_name: 'kilo', session_key: 's1' }),
      run({ agent_name: 'kilo', session_key: 's2' }),
      run({ agent_name: 'kilo', session_key: 's3' }),
      run({ agent_name: 'aider', session_key: 'a1' }),
    ])).toEqual([{ agent_name: 'kilo', runs: 3 }])
  })

  test('says nothing when every agent has exactly one run', () => {
    expect(fragmentedAgents([
      run({ agent_name: 'kilo', session_key: 's1' }),
      run({ agent_name: 'aider', session_key: 'a1' }),
    ])).toEqual([])
  })
})

describe('costIsKnown', () => {
  test('is false while any call in the run went unpriced', () => {
    expect(costIsKnown(run({ unpriced_calls: 0 }))).toBe(true)
    expect(costIsKnown(run({ unpriced_calls: 1 }))).toBe(false)
    expect(costIsKnown(run({ call_count: 0, unpriced_calls: 0 }))).toBe(false)
  })
})

describe('runIsFinal', () => {
  test('is false while a call is still open', () => {
    expect(runIsFinal(run({ in_flight_calls: 0 }))).toBe(true)
    expect(runIsFinal(run({ in_flight_calls: 1 }))).toBe(false)
  })
})

describe('ExperimentComparison', () => {
  test('leads with the cheaper solved run and how much cheaper it was', async () => {
    mockBackend([
      run({ agent_name: 'claude-code', session_key: 'b', total_cost: 1.24, verdict: 'solved' }),
      run({ agent_name: 'kilo', session_key: 'a', total_cost: 0.4, verdict: 'solved' }),
    ])

    render(<ExperimentComparison experimentId="1" grouping="session" onGroupingChange={() => {}} />)

    // Sub-dollar totals keep four decimals.
    expect(await screen.findByText(/3\.1× cheaper/)).toBeInTheDocument()
    expect(screen.getByText(/than claude-code at \$1\.24/)).toBeInTheDocument()
    const runs = within(screen.getByRole('table', { name: 'Runs' }))
    expect(runs.getByText('$0.4000')).toBeInTheDocument()
  })

  test('asks for a verdict rather than ranking unjudged runs on spend alone', async () => {
    mockBackend([run({ session_key: 'a' }), run({ agent_name: 'aider', session_key: 'b', total_cost: 1.1 })])

    render(<ExperimentComparison experimentId="1" grouping="session" onGroupingChange={() => {}} />)

    expect(await screen.findByText(/Nothing is marked solved yet/i)).toBeInTheDocument()
  })

  test('marks a run solved through the backend and reloads', async () => {
    const fetchMock = mockBackend([run({ agent_name: 'kilo', session_key: 'a', session_id: 'a' })])

    render(<ExperimentComparison experimentId="1" grouping="session" onGroupingChange={() => {}} />)

    fireEvent.click(await screen.findByTitle('Mark this run solved'))

    await waitFor(() => {
      const put = fetchMock.mock.calls.find(([, init]: any[]) => init?.method === 'PUT')
      expect(put).toBeDefined()
      expect(JSON.parse((put as any[])[1].body)).toEqual({
        agent_name: 'kilo',
        session_id: 'a',
        verdict: 'solved',
      })
    })
  })

  test('offers per-agent grouping when one agent has several runs', async () => {
    const onGroupingChange = vi.fn()
    mockBackend([
      run({ agent_name: 'kilo', session_key: 's1' }),
      run({ agent_name: 'kilo', session_key: 's2' }),
    ])

    render(
      <ExperimentComparison experimentId="1" grouping="session" onGroupingChange={onGroupingChange} />,
    )

    expect(await screen.findByText(/kilo has 2 runs here/)).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: 'one run per agent' }))
    expect(onGroupingChange).toHaveBeenCalledWith('agent')
  })

  test('does not nag about fragmentation once runs are already merged', async () => {
    mockBackend([
      run({ agent_name: 'kilo', session_key: 's1' }),
      run({ agent_name: 'kilo', session_key: 's2' }),
    ])

    render(<ExperimentComparison experimentId="1" grouping="agent" onGroupingChange={() => {}} />)

    await screen.findByRole('table', { name: 'Runs' })
    expect(screen.queryByText(/has 2 runs here/)).not.toBeInTheDocument()
  })

  test('asks the backend for the grouping it was given', async () => {
    const fetchMock = mockBackend([run({})])

    render(<ExperimentComparison experimentId="7" grouping="agent" onGroupingChange={() => {}} />)

    await waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith(
        expect.stringContaining('/v1/analytics/experiments/7/comparison?group=agent'),
        undefined,
      ),
    )
  })

  test('judges a merged run by experiment, since it has no single session', async () => {
    const fetchMock = mockBackend([
      run({ agent_name: 'kilo', session_key: '', session_id: null, sessions: 3 }),
    ])

    render(<ExperimentComparison experimentId="7" grouping="agent" onGroupingChange={() => {}} />)

    fireEvent.click(await screen.findByTitle('Mark this run solved'))

    await waitFor(() => {
      const put = fetchMock.mock.calls.find(([, init]: any[]) => init?.method === 'PUT')
      expect(JSON.parse((put as any[])[1].body)).toEqual({
        agent_name: 'kilo',
        experiment_id: 7,
        verdict: 'solved',
      })
    })
  })

  test('names the merged session count in place of an id it does not have', async () => {
    mockBackend([run({ agent_name: 'kilo', session_key: '', session_id: null, sessions: 4 })])

    render(<ExperimentComparison experimentId="1" grouping="agent" onGroupingChange={() => {}} />)

    expect(await screen.findByText('4 sessions merged')).toBeInTheDocument()
  })

  test('tells you how to attribute runs when the experiment has none', async () => {
    mockBackend([])

    render(<ExperimentComparison experimentId="1" grouping="session" onGroupingChange={() => {}} />)

    expect(await screen.findByText(/No calls recorded under this experiment yet/i)).toBeInTheDocument()
    expect(screen.getByText('X-Experiment-ID')).toBeInTheDocument()
  })

  test('shows a running total as provisional rather than a result', async () => {
    mockBackend([run({ agent_name: 'kilo', session_key: 'a', total_cost: 0.2, in_flight_calls: 1 })])

    render(<ExperimentComparison experimentId="1" grouping="session" onGroupingChange={() => {}} />)

    expect(await screen.findByText('$0.2000 so far')).toBeInTheDocument()
  })

  test('charges wasted attempts to the agent in the per-agent roll-up', async () => {
    mockBackend([
      run({ agent_name: 'kilo', session_key: 'a', total_cost: 0.4, verdict: 'failed' }),
      run({ agent_name: 'kilo', session_key: 'b', total_cost: 0.4, verdict: 'solved' }),
    ])

    render(<ExperimentComparison experimentId="1" grouping="session" onGroupingChange={() => {}} />)

    const rollup = within(await screen.findByRole('table', { name: 'Per agent' }))
    // $0.80 spent for one fix, not the $0.40 the winning run shows — as both
    // the total spend and the per-solve figure.
    expect(rollup.getAllByText('$0.8000')).toHaveLength(2)
    expect(rollup.getByText('1/2')).toBeInTheDocument()
  })

  test('shows an incomplete cost as a floor rather than a total', async () => {
    mockBackend([run({ agent_name: 'kilo', session_key: 'a', total_cost: 0.3, unpriced_calls: 2 })])

    render(<ExperimentComparison experimentId="1" grouping="session" onGroupingChange={() => {}} />)

    expect(await screen.findByText('≥ $0.3000')).toBeInTheDocument()
  })
})
