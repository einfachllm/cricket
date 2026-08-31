/// Shared shapes and base URL for the local backend. Centralised so the
/// views agree on one definition of a session/task rather than each carrying
/// its own drifting copy.

export const API_BASE = 'http://localhost:8081';

/// The states `session_state::derive_state` can return, mirrored here as a
/// union so a `switch` over them is exhaustive at compile time. `unknown`
/// covers rows recorded before status tracking existed.
export type AgentStateKind =
  | 'working'
  | 'waiting_for_you'
  | 'rate_limited'
  | 'overloaded'
  | 'error'
  | 'interrupted'
  | 'truncated'
  | 'stalled'
  | 'idle'
  | 'unknown';

export interface SessionSummary {
  agent_name: string;
  session_id: string | null;
  state: AgentStateKind;
  state_label: string;
  state_detail: string | null;
  needs_attention: boolean;
  call_count: number;
  first_seen: string | null;
  last_seen: string | null;
  age_seconds: number;
  idle_seconds: number;
  input_tokens: number;
  output_tokens: number;
  total_cost: number;
  unpriced_calls: number;
  busy_ms: number;
  rate_limited_calls: number;
  error_calls: number;
  last_task_id: number;
  last_task_description: string | null;
  model_name: string | null;
  provider: string | null;
  experiment_name: string | null;
  question_text: string | null;
  error_message: string | null;
  requests_remaining: number | null;
  requests_limit: number | null;
  tokens_remaining: number | null;
  tokens_limit: number | null;
}

/// Whether a run actually solved what it was given. Nothing in the proxied
/// traffic can answer this, so it comes from whoever read the diff — see
/// `set_session_verdict` in the backend for why the comparison needs it.
export type Verdict = 'solved' | 'failed';

/// One *run* — one agent working one session — folded down from its calls.
/// The unit the experiment comparison ranks on.
export interface RunComparison {
  agent_name: string;
  /// Stable key for the run, `''` for calls sent without an `X-Session-ID`.
  session_key: string;
  session_id: string | null;
  call_count: number;
  first_seen: string | null;
  last_seen: string | null;
  wall_clock_seconds: number;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  tool_calls: number;
  total_cost: number;
  unpriced_calls: number;
  busy_ms: number;
  rate_limited_calls: number;
  error_calls: number;
  /// Calls still open. Non-zero means the run is still spending, so its
  /// total is a running tally rather than a result.
  in_flight_calls: number;
  /// Comma-separated, since a run may switch models mid-task.
  models: string | null;
  providers: string | null;
  verdict: Verdict | null;
  verdict_note: string | null;
}

/// One of the five equal slices a run's calls are cut into, so runs of
/// different lengths line up against each other. `phase` is 1-based; a run
/// with fewer than five calls fills fewer slices rather than gaining empty
/// ones.
export interface PhaseSlice {
  agent_name: string;
  session_key: string;
  phase: number;
  calls: number;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  tool_calls: number;
  cost: number;
}

/// One tool's share of a run's spend. Tokens are the turn's totals split
/// across the tools that turn called — see `get_experiment_tool_usage`.
export interface ToolUsage {
  agent_name: string;
  session_key: string;
  tool_name: string;
  call_count: number;
  input_tokens: number;
  output_tokens: number;
  cost: number;
}

export interface ExperimentBreakdown {
  phases: PhaseSlice[];
  tools: ToolUsage[];
}

export interface ProviderLimits {
  provider: string | null;
  requests_limit: number | null;
  requests_remaining: number | null;
  requests_reset: string | null;
  tokens_limit: number | null;
  tokens_remaining: number | null;
  tokens_reset: string | null;
  /// Seconds still to wait, already adjusted for time elapsed since the
  /// provider said so — null once the window has passed.
  retry_after_remaining_s: number | null;
  observed_at: string | null;
  observed_seconds_ago: number;
}

export async function fetchJson<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(API_BASE + path, init);
  if (!response.ok) {
    throw new Error(`${path} responded ${response.status}`);
  }
  return response.json() as Promise<T>;
}

/// Marks (or, with `verdict: null`, un-marks) whether a run solved its task.
export async function setVerdict(
  run: Pick<RunComparison, 'agent_name' | 'session_id'>,
  verdict: Verdict | null,
): Promise<void> {
  await fetchJson<{ ok: boolean }>('/v1/analytics/sessions/verdict', {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      agent_name: run.agent_name,
      session_id: run.session_id,
      verdict,
    }),
  });
}

/// Coarse, glanceable durations — the frontend twin of the backend's
/// `humanize_secs`, used for numbers the backend hasn't already formatted
/// into a `state_detail` string.
export function humanizeSecs(secs: number): string {
  if (!Number.isFinite(secs) || secs < 0) return 'just now';
  if (secs < 60) return `${Math.floor(secs)}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m`;
  if (secs < 86400) return `${Math.floor(secs / 3600)}h`;
  return `${Math.floor(secs / 86400)}d`;
}

/// Precision scaled to magnitude: a single cheap call needs five decimals to
/// say anything at all, while a session total of $2.1810 just reads as noise.
export function formatCost(cost: number | null | undefined): string {
  if (cost === null || cost === undefined) return '–';
  if (cost === 0) return '$0.00';
  if (cost >= 1) return `$${cost.toFixed(2)}`;
  return cost < 0.01 ? `$${cost.toFixed(5)}` : `$${cost.toFixed(4)}`;
}

/// Token counts get long fast; 1.2M reads better than 1,238,402 in a card.
export function formatTokens(tokens: number | null | undefined): string {
  if (tokens === null || tokens === undefined) return '–';
  if (tokens < 1000) return `${tokens}`;
  if (tokens < 1_000_000) return `${(tokens / 1000).toFixed(1)}k`;
  return `${(tokens / 1_000_000).toFixed(2)}M`;
}
