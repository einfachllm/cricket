import { render, screen, within } from '@testing-library/react'
import { afterEach, describe, expect, test, vi } from 'vitest'
import RunBreakdown, { outputShareShift, runsIn, toStack } from './RunBreakdown'
import type { ExperimentBreakdown, PhaseSlice, ToolUsage } from '../lib/api'

function slice(overrides: Partial<PhaseSlice>): PhaseSlice {
  return {
    agent_name: 'kilo',
    session_key: 'run-a',
    phase: 1,
    calls: 2,
    input_tokens: 10_000,
    output_tokens: 500,
    cache_read_tokens: 4_000,
    tool_calls: 3,
    cost: 0.02,
    ...overrides,
  }
}

function tool(overrides: Partial<ToolUsage>): ToolUsage {
  return {
    agent_name: 'kilo',
    session_key: 'run-a',
    tool_name: 'read_file',
    call_count: 4,
    input_tokens: 8_000,
    output_tokens: 300,
    cost: 0.01,
    ...overrides,
  }
}

function mockBackend(breakdown: ExperimentBreakdown) {
  vi.stubGlobal('fetch', vi.fn(async () => ({ ok: true, status: 200, json: async () => breakdown })))
}

afterEach(() => {
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

describe('toStack', () => {
  test('splits input into what was read fresh and what came from cache', () => {
    const stack = toStack(slice({ input_tokens: 10_000, cache_read_tokens: 4_000, output_tokens: 500 }))

    expect(stack.cached).toBe(4_000)
    expect(stack.fresh).toBe(6_000)
    expect(stack.output).toBe(500)
    // The three parts are the whole, so a stacked column can't overcount.
    expect(stack.total).toBe(10_500)
  })

  test('never reports more cached tokens than input, however the provider counted', () => {
    const stack = toStack(slice({ input_tokens: 1_000, cache_read_tokens: 5_000 }))

    expect(stack.cached).toBe(1_000)
    expect(stack.fresh).toBe(0)
  })
})

describe('runsIn', () => {
  test('lists each run once, in the order the backend returned them', () => {
    const runs = runsIn([
      slice({ agent_name: 'kilo', session_key: 'a', phase: 1 }),
      slice({ agent_name: 'kilo', session_key: 'a', phase: 2 }),
      slice({ agent_name: 'aider', session_key: 'b', phase: 1 }),
    ])

    expect(runs).toEqual([
      { agent_name: 'kilo', session_key: 'a' },
      { agent_name: 'aider', session_key: 'b' },
    ])
  })

  test('keeps two runs by the same agent apart', () => {
    const runs = runsIn([
      slice({ agent_name: 'kilo', session_key: 'first' }),
      slice({ agent_name: 'kilo', session_key: 'second' }),
    ])

    expect(runs).toHaveLength(2)
  })
})

describe('outputShareShift', () => {
  test('compares the generation share at the two ends of the run', () => {
    const shift = outputShareShift([
      toStack(slice({ phase: 1, input_tokens: 9_000, cache_read_tokens: 0, output_tokens: 1_000 })),
      toStack(slice({ phase: 5, input_tokens: 1_000, cache_read_tokens: 0, output_tokens: 3_000 })),
    ])

    expect(shift?.first).toBeCloseTo(0.1)
    expect(shift?.last).toBeCloseTo(0.75)
  })

  test('has no answer for a run with only one non-empty phase', () => {
    expect(outputShareShift([toStack(slice({ phase: 1 }))])).toBeNull()
  })

  test('ignores empty phases rather than dividing by zero', () => {
    const shift = outputShareShift([
      toStack(slice({ phase: 1, input_tokens: 0, cache_read_tokens: 0, output_tokens: 0 })),
      toStack(slice({ phase: 2, input_tokens: 900, cache_read_tokens: 0, output_tokens: 100 })),
      toStack(slice({ phase: 3, input_tokens: 500, cache_read_tokens: 0, output_tokens: 500 })),
    ])

    expect(shift?.first).toBeCloseTo(0.1)
    expect(shift?.last).toBeCloseTo(0.5)
  })
})

describe('RunBreakdown', () => {
  test('renders a panel per run with the generation shift spelled out', async () => {
    mockBackend({
      phases: [
        slice({ phase: 1, input_tokens: 9_000, cache_read_tokens: 0, output_tokens: 1_000 }),
        slice({ phase: 5, input_tokens: 1_000, cache_read_tokens: 0, output_tokens: 1_000 }),
      ],
      tools: [tool({})],
    })

    render(<RunBreakdown experimentId="1" />)

    expect(await screen.findByText('Where the money went')).toBeInTheDocument()
    expect(screen.getByText('kilo')).toBeInTheDocument()
    expect(screen.getByText(/output 10% → 50% of the mix/)).toBeInTheDocument()
  })

  test('shows each tool as a share of the run tool spend', async () => {
    mockBackend({
      phases: [slice({})],
      tools: [
        tool({ tool_name: 'read_file', input_tokens: 7_500, call_count: 5 }),
        tool({ tool_name: 'bash', input_tokens: 2_500, call_count: 2 }),
      ],
    })

    render(<RunBreakdown experimentId="1" />)

    expect(await screen.findByText('read_file')).toBeInTheDocument()
    expect(screen.getByText('75%')).toBeInTheDocument()
    expect(screen.getByText('25%')).toBeInTheDocument()
    expect(screen.getByText('5 calls')).toBeInTheDocument()
  })

  test('folds a long tool tail into one Other row rather than growing forever', async () => {
    mockBackend({
      phases: [slice({})],
      tools: Array.from({ length: 9 }, (_, i) =>
        tool({ tool_name: `tool_${i}`, input_tokens: 1_000, call_count: 1 }),
      ),
    })

    render(<RunBreakdown experimentId="1" />)

    expect(await screen.findByText('Other (3)')).toBeInTheDocument()
    expect(screen.queryByText('tool_8')).not.toBeInTheDocument()
  })

  test('says so when a run has no tool calls instead of drawing an empty chart', async () => {
    mockBackend({ phases: [slice({})], tools: [] })

    render(<RunBreakdown experimentId="1" />)

    expect(await screen.findByText(/No tool calls recorded/i)).toBeInTheDocument()
  })

  test('renders nothing at all for an experiment with no calls', async () => {
    mockBackend({ phases: [], tools: [] })

    const { container } = render(<RunBreakdown experimentId="1" />)

    await vi.waitFor(() => expect(container).toBeEmptyDOMElement())
  })

  test('names the three billed parts in a legend, so identity is never colour alone', async () => {
    mockBackend({ phases: [slice({})], tools: [tool({})] })

    render(<RunBreakdown experimentId="1" />)

    const chart = await screen.findByRole('img', { name: /fresh input/i })
    expect(chart).toBeInTheDocument()
    expect(screen.getByText('Fresh input')).toBeInTheDocument()
    expect(screen.getByText('Cached input')).toBeInTheDocument()
    expect(screen.getByText('Output')).toBeInTheDocument()
  })

  test('labels every phase slot, including ones the run never reached', async () => {
    mockBackend({ phases: [slice({ phase: 1 })], tools: [] })

    render(<RunBreakdown experimentId="1" />)

    const chart = await screen.findByRole('img', { name: /fresh input/i })
    for (const label of ['Early', 'Early-mid', 'Mid', 'Late-mid', 'Late']) {
      expect(within(chart).getByText(label)).toBeInTheDocument()
    }
  })

  test('surfaces a backend error instead of an empty panel', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => { throw new Error('Failed to fetch') }))

    render(<RunBreakdown experimentId="1" />)

    expect(await screen.findByText(/Couldn't load the breakdown/i)).toBeInTheDocument()
  })
})
