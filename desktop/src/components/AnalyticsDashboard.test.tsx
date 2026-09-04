import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, describe, expect, test, vi } from 'vitest'
import AnalyticsDashboard from './AnalyticsDashboard'

const experiment = { id: 1, name: 'smoke-test', description: '' }

function mockBackend() {
  const calls: { url: string; init?: RequestInit }[] = []
  let experiments: unknown[] = [experiment]
  vi.stubGlobal('fetch', vi.fn(async (url: string, init?: RequestInit) => {
    calls.push({ url: String(url), init })
    if (init?.method === 'DELETE') {
      experiments = []
      return { ok: true, status: 200, json: async () => ({ ok: true }) }
    }
    if (String(url).includes('/metrics')) return { ok: true, status: 200, json: async () => [] }
    return { ok: true, status: 200, json: async () => experiments }
  }))
  return calls
}

afterEach(() => {
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

describe('AnalyticsDashboard', () => {
  test('deleting an experiment asks for confirmation, removes the grouping, and clears the selection', async () => {
    const calls = mockBackend()
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(true)

    render(<AnalyticsDashboard />)

    fireEvent.click(await screen.findByRole('button', { name: 'Delete experiment smoke-test' }))

    await waitFor(() => {
      const del = calls.find((c) => c.init?.method === 'DELETE')
      expect(del).toBeDefined()
      expect(del!.url).toContain('/v1/analytics/experiments/1')
    })

    // The list refetches, the experiment is gone, and nothing stays selected.
    expect(await screen.findByText('No experiments found.')).toBeInTheDocument()
    expect(screen.queryByText('smoke-test')).not.toBeInTheDocument()
    expect(screen.getByText('Select an experiment to view detailed metrics')).toBeInTheDocument()
    expect(confirm).toHaveBeenCalledOnce()
  })

  test('declining the confirmation leaves the experiment in place', async () => {
    mockBackend()
    vi.spyOn(window, 'confirm').mockReturnValue(false)

    render(<AnalyticsDashboard />)

    fireEvent.click(await screen.findByRole('button', { name: 'Delete experiment smoke-test' }))

    await waitFor(() => expect(screen.queryByText('No experiments found.')).not.toBeInTheDocument())
    expect(screen.getByText('smoke-test')).toBeInTheDocument()
  })
})
