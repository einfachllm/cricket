import { render, screen } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest'
import TrafficView from './TrafficView'

function task(overrides: Record<string, unknown>) {
  return {
    task_id: 1,
    agent_name: 'opencode',
    model_name: 'qwen/qwen3.8-max',
    provider: 'openrouter',
    session_id: 's1',
    timestamp: '2026-09-03 20:45:57',
    task_description: 'Fix the login redirect',
    experiment_name: 'smoke-test',
    prompt_tokens: 12039,
    completion_tokens: 69,
    cache_creation_tokens: 0,
    cache_read_tokens: 0,
    tool_calls_count: 0,
    latency_ms: 1601,
    cost_estimate: null,
    agent_question_tool: null,
    agent_question_text: null,
    status: 'ok',
    http_status: 200,
    error_type: null,
    error_message: null,
    stop_reason: 'stop',
    awaiting_input: false,
    ttfb_ms: 500,
    duration_ms: 1601,
    ...overrides,
  }
}

beforeEach(() => {
  vi.stubGlobal('fetch', vi.fn(async () => ({
    ok: true,
    status: 200,
    json: async () => [
      task({}),
      task({ task_id: 2, agent_name: 'kilo', session_id: null, task_description: 'Ungrouped call' }),
    ],
  })))
})

afterEach(() => {
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

describe('TrafficView', () => {
  test('shows which session each call belongs to', async () => {
    render(<TrafficView />)

    expect(await screen.findByText('s1')).toBeInTheDocument()
    const row = screen.getByText('s1').closest('tr')!
    expect(row.textContent).toContain('opencode')
  })

  test('marks a call without a session id instead of leaving the cell blank', async () => {
    render(<TrafficView />)

    const row = await screen.findByText('Ungrouped call').then((el) => el.closest('tr')!)
    expect(row.textContent).toContain('–')
  })
})
