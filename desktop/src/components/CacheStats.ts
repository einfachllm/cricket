/// Cache-efficiency arithmetic over recorded calls — the numbers the
/// gateway-data literature (Requesty's twelve-month study, the prompt-
/// caching evaluations) says separate agents hardest: cache hit rate,
/// input share ("communication tax"), and context size per call.
///
/// The proxy stores `prompt_tokens` *exclusive* of cache tokens (see the
/// usage extraction in the backend), so a call's total input is fresh +
/// cache-written + cache-read. All arithmetic is client-side over the tasks
/// the Runs view already loads — no new backend surface.
export interface CacheInput {
  agent_name: string;
  prompt_tokens: number | null;
  cache_creation_tokens: number | null;
  cache_read_tokens: number | null;
  completion_tokens: number | null;
}

export interface AgentCacheStats {
  agent: string;
  calls: number;
  /// Tokens the model read fresh from context, wrote into the cache, and
  /// got served from the cache, across the calls.
  freshInput: number;
  cacheCreation: number;
  cacheRead: number;
  output: number;
  /// Share of all input served from cache — the lever worth up to the
  /// whole cost of fresh input. Null when no input was recorded at all.
  hitRate: number | null;
  /// Input share of all tokens. Agents re-send their context every turn,
  /// which is why the literature measures this at roughly half and calls
  /// it the communication tax. Null with no tokens at all.
  inputShare: number | null;
  /// Average input tokens per call. Null when there are no calls.
  avgInput: number | null;
}

export interface CacheStats {
  overall: Omit<AgentCacheStats, "agent">;
  /// Busiest agents first.
  perAgent: AgentCacheStats[];
}

function agentStats(agent: string, tasks: CacheInput[]): AgentCacheStats {
  let freshInput = 0;
  let cacheCreation = 0;
  let cacheRead = 0;
  let output = 0;

  for (const task of tasks) {
    freshInput += task.prompt_tokens ?? 0;
    cacheCreation += task.cache_creation_tokens ?? 0;
    cacheRead += task.cache_read_tokens ?? 0;
    output += task.completion_tokens ?? 0;
  }

  const input = freshInput + cacheCreation + cacheRead;
  const total = input + output;

  return {
    agent,
    calls: tasks.length,
    freshInput,
    cacheCreation,
    cacheRead,
    output,
    hitRate: input > 0 ? cacheRead / input : null,
    inputShare: total > 0 ? input / total : null,
    avgInput: tasks.length > 0 ? input / tasks.length : null,
  };
}

/// Cache and context statistics across the given calls, overall and per
/// agent. Null for an empty set — an empty panel is more honest than zeros,
/// which would read as "0% hit rate", the opposite of "no data".
export function cacheStats(tasks: CacheInput[]): CacheStats | null {
  if (tasks.length === 0) return null;

  const byAgent = new Map<string, CacheInput[]>();
  for (const task of tasks) {
    byAgent.set(task.agent_name, [...(byAgent.get(task.agent_name) ?? []), task]);
  }

  const perAgent = [...byAgent.entries()]
    .map(([agent, agentTasks]) => agentStats(agent, agentTasks))
    .sort((a, b) => b.calls - a.calls || a.agent.localeCompare(b.agent));

  return { overall: agentStats("all", tasks), perAgent };
}
