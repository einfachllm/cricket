import { describe, expect, test } from 'vitest'
import { cacheStats } from './CacheStats'
import type { CacheInput } from './CacheStats'

function call(overrides: Partial<CacheInput>): CacheInput {
  return {
    agent_name: 'kilo',
    prompt_tokens: 100,
    cache_creation_tokens: 0,
    cache_read_tokens: 0,
    completion_tokens: 100,
    ...overrides,
  }
}

describe('cacheStats', () => {
  test('null for an empty set — zeros would read as a 0% hit rate', () => {
    expect(cacheStats([])).toBeNull()
  })

  test('computes hit rate, input share and average context', () => {
    const stats = cacheStats([
      // 900 cached + 100 fresh input, 100 output.
      call({ cache_read_tokens: 900 }),
      // Another 900/100/100 turn.
      call({ cache_read_tokens: 900 }),
    ])!

    expect(stats.overall.calls).toBe(2)
    expect(stats.overall.cacheRead).toBe(1800)
    expect(stats.overall.freshInput).toBe(200)
    expect(stats.overall.hitRate).toBeCloseTo(0.9)
    // 2000 input vs 200 output: the communication tax at work.
    expect(stats.overall.inputShare).toBeCloseTo(2000 / 2200)
    expect(stats.overall.avgInput).toBeCloseTo(1000)
  })

  test('cache-written tokens count as input but not as hits', () => {
    const stats = cacheStats([
      call({ prompt_tokens: 100, cache_creation_tokens: 400, cache_read_tokens: 0 }),
    ])!

    expect(stats.overall.hitRate).toBe(0)
    expect(stats.overall.avgInput).toBeCloseTo(500)
  })

  test('null token fields count as zero rather than poisoning the sums', () => {
    const stats = cacheStats([
      call({ prompt_tokens: null, cache_read_tokens: 900, completion_tokens: null }),
    ])!

    expect(stats.overall.hitRate).toBe(1)
    expect(stats.overall.output).toBe(0)
  })

  test('splits per agent, busiest first', () => {
    const stats = cacheStats([
      call({ agent_name: 'kilo' }),
      call({ agent_name: 'opencode' }),
      call({ agent_name: 'opencode' }),
      call({ agent_name: 'opencode' }),
    ])!

    expect(stats.perAgent.map((a) => a.agent)).toEqual(['opencode', 'kilo'])
    expect(stats.perAgent[0].calls).toBe(3)
    expect(stats.perAgent[1].calls).toBe(1)
  })

  test('agents with no recorded input show a null hit rate, not a zero', () => {
    const stats = cacheStats([call({ prompt_tokens: null, cache_read_tokens: null })])!
    expect(stats.overall.hitRate).toBeNull()
  })
})
