import { fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest'
import AgentStatusView, { sortSessions } from './AgentStatusView'
import { SessionsProvider } from '../hooks/useSessions'
import type { SessionSummary } from '../lib/api'

function session(overrides: Partial<SessionSummary>): SessionSummary {
  return {
    agent_name: 'kilo',
    session_id: 'sess-1',
    state: 'idle',
    state_label: 'Idle',
    state_detail: null,
    needs_attention: false,
    dismissed: false,
    call_count: 1,
    first_seen: '2026-08-31 12:00:00',
    last_seen: '2026-08-31 12:00:00',
    age_seconds: 5,
    idle_seconds: 5,
    input_tokens: 1000,
    output_tokens: 200,
    total_cost: 0.02,
    unpriced_calls: 0,
    busy_ms: 1200,
    rate_limited_calls: 0,
    error_calls: 0,
    last_task_id: 1,
    last_task_description: null,
    model_name: 'claude-opus-5',
    provider: 'anthropic',
    experiment_name: null,
    question_text: null,
    error_message: null,
    requests_remaining: null,
    requests_limit: null,
    tokens_remaining: null,
    tokens_limit: null,
    ...overrides,
  }
}

/// Serves the two endpoints the provider fetches, so the view renders from
/// realistic payloads rather than injected props.
function mockBackend(sessions: SessionSummary[], limits: unknown[] = []) {
  vi.stubGlobal('fetch', vi.fn(async (url: string) => ({
    ok: true,
    status: 200,
    json: async () => (String(url).includes('/limits') ? limits : sessions),
  })))
}

function renderView() {
  return render(
    <SessionsProvider>
      <AgentStatusView />
    </SessionsProvider>,
  )
}

beforeEach(() => {
  // jsdom has no EventSource; the provider must fall back to polling rather
  // than throwing, which is also the real behaviour in a browser without it.
  vi.stubGlobal('EventSource', undefined)
})

afterEach(() => {
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

describe('sortSessions', () => {
  test('puts sessions that need a human above ones that are merely busy', () => {
    const ordered = sortSessions([
      session({ session_id: 'idle', state: 'idle' }),
      session({ session_id: 'busy', state: 'working' }),
      session({ session_id: 'asking', state: 'waiting_for_you', needs_attention: true }),
      session({ session_id: 'blocked', state: 'rate_limited', needs_attention: true }),
    ])

    expect(ordered.map((s) => s.session_id)).toEqual(['asking', 'blocked', 'busy', 'idle'])
  })

  test('breaks ties by how recently the session was active', () => {
    const ordered = sortSessions([
      session({ session_id: 'stale', state: 'idle', idle_seconds: 900 }),
      session({ session_id: 'fresh', state: 'idle', idle_seconds: 10 }),
    ])

    expect(ordered.map((s) => s.session_id)).toEqual(['fresh', 'stale'])
  })

  test('a dismissed run sorts at rest even while its state wants a human', () => {
    const ordered = sortSessions([
      session({ session_id: 'seen', state: 'waiting_for_you', needs_attention: false, dismissed: true }),
      session({ session_id: 'busy', state: 'working' }),
    ])

    expect(ordered.map((s) => s.session_id)).toEqual(['busy', 'seen'])
  })

  test('does not mutate the array it was given', () => {
    const input = [session({ session_id: 'a', state: 'idle' }), session({ session_id: 'b', state: 'working' })]
    sortSessions(input)
    expect(input.map((s) => s.session_id)).toEqual(['a', 'b'])
  })
})

describe('AgentStatusView', () => {
  test('shows a waiting agent with the question it asked', async () => {
    mockBackend([
      session({
        agent_name: 'opencode',
        state: 'waiting_for_you',
        state_label: 'Waiting for you',
        state_detail: 'Should I also update the tests?',
        needs_attention: true,
      }),
    ])

    renderView()

    expect(await screen.findByText('Waiting for you')).toBeInTheDocument()
    expect(screen.getByText('Should I also update the tests?')).toBeInTheDocument()
    expect(screen.getByText('opencode')).toBeInTheDocument()
  })

  test('counts only the sessions that need a human in the "Need you" tile', async () => {
    mockBackend([
      session({ session_id: 'a', state: 'waiting_for_you', needs_attention: true }),
      session({ session_id: 'b', state: 'rate_limited', needs_attention: true }),
      session({ session_id: 'c', state: 'working' }),
    ])

    renderView()

    const needYou = (await screen.findByText('Need you')).closest('div')!
    expect(within(needYou).getByText('2')).toBeInTheDocument()
    const working = screen.getByText('Working').closest('div')!
    expect(within(working).getByText('1')).toBeInTheDocument()
  })

  test('renders a quota bar only when the provider reported real numbers', async () => {
    mockBackend(
      [session({})],
      [
        { provider: 'anthropic', requests_limit: 1000, requests_remaining: 250, requests_reset: null,
          tokens_limit: null, tokens_remaining: null, tokens_reset: null, retry_after_remaining_s: null,
          observed_at: '2026-08-31 12:00:00', observed_seconds_ago: 30 },
      ],
    )

    renderView()

    expect(await screen.findByText('Provider quota')).toBeInTheDocument()
    expect(screen.getByText('Requests')).toBeInTheDocument()
    expect(screen.getByText('250 / 1.0k')).toBeInTheDocument()
    // Tokens were not reported for this provider, so no bar is invented.
    expect(screen.queryByText('Tokens')).not.toBeInTheDocument()
  })

  test('surfaces a backend that is not running instead of showing an empty dashboard', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => { throw new Error('Failed to fetch') }))

    renderView()

    expect(await screen.findByText(/Can't reach the Harnesswurm backend/i)).toBeInTheDocument()
  })

  test('tells the user it is polling when live updates are unavailable', async () => {
    mockBackend([])

    renderView()

    await waitFor(() => expect(screen.getByText('Polling')).toBeInTheDocument())
    expect(await screen.findByText(/No agent sessions yet/i)).toBeInTheDocument()
  })

  test('dismissing a run quiets its badge and refetches', async () => {
    const attention = session({
      agent_name: 'opencode',
      state: 'waiting_for_you',
      state_label: 'Waiting for you',
      needs_attention: true,
    })
    const dismissed = { ...attention, needs_attention: false, dismissed: true }
    const getPayloads = [[attention], [dismissed]]
    const calls: { url: string; init?: RequestInit }[] = []
    vi.stubGlobal('fetch', vi.fn(async (url: string, init?: RequestInit) => {
      calls.push({ url: String(url), init })
      if (String(url).includes('/dismiss')) {
        return { ok: true, status: 200, json: async () => ({ ok: true }) }
      }
      if (String(url).includes('/limits')) return { ok: true, status: 200, json: async () => [] }
      return { ok: true, status: 200, json: async () => getPayloads.shift() ?? [] }
    }))

    renderView()

    fireEvent.click(await screen.findByRole('button', { name: /dismiss attention on opencode/i }))

    await waitFor(() => {
      const put = calls.find((c) => c.url.includes('/dismiss'))
      expect(put).toBeDefined()
      expect(put!.init?.method).toBe('PUT')
      expect(JSON.parse(String(put!.init?.body))).toEqual({ agent_name: 'opencode', session_id: 'sess-1' })
    })

    // The refetched run reports dismissed: the chip goes muted, the alarm
    // button disappears, and the "Need you" tile no longer counts it.
    const chip = await screen.findByText('Waiting for you')
    expect(chip.closest('span')).toHaveClass('bg-gray-100')
    expect(screen.queryByRole('button', { name: /dismiss attention/i })).not.toBeInTheDocument()
    await waitFor(() => {
      const needYou = screen.getByText('Need you').closest('div')!
      expect(within(needYou).getByText('0')).toBeInTheDocument()
    })
  })

  test('a dismissed run shows no dismiss button', async () => {
    mockBackend([session({ state: 'waiting_for_you', state_label: 'Waiting for you', needs_attention: false, dismissed: true })])

    renderView()

    expect(await screen.findByText('Waiting for you')).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: /dismiss attention/i })).not.toBeInTheDocument()
  })
})
