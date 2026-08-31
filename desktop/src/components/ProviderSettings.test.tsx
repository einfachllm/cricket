import { fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest'
import ProviderSettings, { previewTargetUrl } from './ProviderSettings'
import type { ProviderConfig } from '../lib/api'

function provider(overrides: Partial<ProviderConfig> = {}): ProviderConfig {
  return {
    name: 'openai',
    api: 'openai',
    base_url: 'https://api.openai.com/v1',
    target_url: 'https://api.openai.com/v1/chat/completions',
    default: true,
    env_override: null,
    ...overrides,
  }
}

const DEFAULTS = [
  provider(),
  provider({
    name: 'anthropic', api: 'anthropic', base_url: 'https://api.anthropic.com',
    target_url: 'https://api.anthropic.com/v1/messages',
  }),
]

/// Serves GET /v1/providers, and records what a save sends so the test can
/// assert on the request rather than on the component's internals.
function mockBackend(options: {
  providers?: ProviderConfig[];
  saveStatus?: number;
  saveBody?: unknown;
} = {}) {
  const calls: { method: string; body: unknown }[] = []
  const fetchMock = vi.fn(async (_url: string, init?: RequestInit) => {
    const method = init?.method ?? 'GET'
    calls.push({ method, body: init?.body ? JSON.parse(String(init.body)) : null })
    if (method === 'PUT') {
      const status = options.saveStatus ?? 200
      return {
        ok: status < 400,
        status,
        json: async () => options.saveBody ?? { providers: options.providers ?? DEFAULTS },
      }
    }
    return { ok: true, status: 200, json: async () => ({ providers: options.providers ?? DEFAULTS }) }
  })
  vi.stubGlobal('fetch', fetchMock)
  return calls
}

beforeEach(() => {
  vi.stubGlobal('navigator', { ...navigator, clipboard: { writeText: vi.fn(async () => {}) } })
})

afterEach(() => {
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

describe('ProviderSettings', () => {
  test('renders the configured providers with the URL to point an agent at', async () => {
    mockBackend()
    render(<ProviderSettings />)

    expect(await screen.findByDisplayValue('https://api.openai.com/v1')).toBeInTheDocument()
    // The path prefix is the whole point of the view: it is what gets pasted
    // into an agent's config, and it differs per api style.
    expect(screen.getByText('http://localhost:8081/p/openai/v1')).toBeInTheDocument()
    expect(screen.getByText('http://localhost:8081/p/anthropic')).toBeInTheDocument()
  })

  test('adding a local provider saves the whole list', async () => {
    const calls = mockBackend()
    render(<ProviderSettings />)
    await screen.findByDisplayValue('https://api.openai.com/v1')

    fireEvent.click(screen.getByRole('button', { name: /add provider/i }))
    const nameFields = screen.getAllByLabelText('Provider name')
    fireEvent.change(nameFields[nameFields.length - 1], { target: { value: 'ollama' } })
    fireEvent.click(screen.getByRole('button', { name: /save providers/i }))

    await waitFor(() => expect(calls.some((c) => c.method === 'PUT')).toBe(true))
    const saved = calls.find((c) => c.method === 'PUT')!.body as { providers: unknown[] }
    expect(saved.providers).toHaveLength(3)
    expect(saved.providers[2]).toEqual({
      name: 'ollama',
      api: 'openai',
      base_url: 'http://localhost:11434/v1',
      default: false,
    })
  })

  test('a rejected save shows the backend’s own reason', async () => {
    mockBackend({
      saveStatus: 400,
      saveBody: { error: { message: "Provider 'ollama' has no base URL." } },
    })
    render(<ProviderSettings />)
    await screen.findByDisplayValue('https://api.openai.com/v1')

    fireEvent.click(screen.getByRole('button', { name: /add provider/i }))
    fireEvent.click(screen.getByRole('button', { name: /save providers/i }))

    const alert = await screen.findByRole('alert')
    expect(within(alert).getByText(/has no base URL/)).toBeInTheDocument()
  })

  test('marking one provider default clears the previous one of that style only', async () => {
    mockBackend({
      providers: [
        provider(),
        provider({ name: 'ollama', base_url: 'http://localhost:11434/v1', default: false }),
        provider({ name: 'anthropic', api: 'anthropic', base_url: 'https://api.anthropic.com' }),
      ],
    })
    render(<ProviderSettings />)
    await screen.findByDisplayValue('http://localhost:11434/v1')

    const radios = screen.getAllByRole('radio') as HTMLInputElement[]
    fireEvent.click(radios[1])

    expect(radios[0].checked).toBe(false)
    expect(radios[1].checked).toBe(true)
    // The anthropic default is a different route and must not be disturbed.
    expect(radios[2].checked).toBe(true)
  })

  test('an env override is called out, because the file value is not the one in use', async () => {
    mockBackend({
      providers: [provider({ env_override: 'HARNESSWURM_OPENAI_BASE_URL' })],
    })
    render(<ProviderSettings />)

    expect(await screen.findByText(/HARNESSWURM_OPENAI_BASE_URL/)).toBeInTheDocument()
  })

  test('the target preview mirrors the backend rule while typing', () => {
    expect(previewTargetUrl({ api: 'openai', base_url: 'http://localhost:11434/v1/' }))
      .toBe('http://localhost:11434/v1/chat/completions')
    expect(previewTargetUrl({ api: 'anthropic', base_url: 'http://localhost:4000' }))
      .toBe('http://localhost:4000/v1/messages')
    // A complete endpoint URL pasted from another tool is left alone.
    expect(previewTargetUrl({ api: 'openai', base_url: 'http://x/v1/chat/completions' }))
      .toBe('http://x/v1/chat/completions')
    expect(previewTargetUrl({ api: 'openai', base_url: '  ' })).toBe('')
  })
})
