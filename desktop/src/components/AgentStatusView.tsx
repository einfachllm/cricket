import React, { useState } from 'react';
import {
  AlertTriangle,
  Ban,
  BellOff,
  Bot,
  CircleSlash,
  Clock,
  CloudOff,
  HelpCircle,
  Loader2,
  MessageCircleQuestion,
  Moon,
  Scissors,
  Wifi,
  WifiOff,
} from 'lucide-react';
import { AgentStateKind, dismissSession, ProviderLimits, SessionSummary, formatCost, formatTokens, humanizeSecs } from '../lib/api';
import { useSessions } from '../hooks/useSessions';

/// Per-state presentation. Kept as one table rather than scattered
/// conditionals so adding a state to the backend is a single edit here, and
/// so the ordering below has one place to read priorities from.
///
/// `rank` decides sort order: whatever wants a human comes first, then
/// whatever is actively running, then everything at rest. A dashboard sorted
/// purely by time buries the one session that is blocked on you.
const STATE_STYLES: Record<AgentStateKind, {
  rank: number;
  chip: string;
  accent: string;
  icon: React.ComponentType<{ size?: number | string; className?: string }>;
  spin?: boolean;
}> = {
  waiting_for_you: { rank: 0, chip: 'bg-amber-100 text-amber-800 border-amber-200', accent: 'border-l-amber-400', icon: MessageCircleQuestion },
  rate_limited: { rank: 1, chip: 'bg-red-100 text-red-800 border-red-200', accent: 'border-l-red-500', icon: Ban },
  stalled: { rank: 2, chip: 'bg-red-100 text-red-800 border-red-200', accent: 'border-l-red-500', icon: AlertTriangle },
  error: { rank: 3, chip: 'bg-red-100 text-red-800 border-red-200', accent: 'border-l-red-500', icon: AlertTriangle },
  truncated: { rank: 4, chip: 'bg-purple-100 text-purple-800 border-purple-200', accent: 'border-l-purple-400', icon: Scissors },
  working: { rank: 5, chip: 'bg-blue-100 text-blue-800 border-blue-200', accent: 'border-l-blue-500', icon: Loader2, spin: true },
  overloaded: { rank: 6, chip: 'bg-orange-100 text-orange-800 border-orange-200', accent: 'border-l-orange-400', icon: CloudOff },
  interrupted: { rank: 7, chip: 'bg-slate-100 text-slate-700 border-slate-200', accent: 'border-l-slate-400', icon: CircleSlash },
  idle: { rank: 8, chip: 'bg-gray-100 text-gray-600 border-gray-200', accent: 'border-l-gray-300', icon: Moon },
  unknown: { rank: 9, chip: 'bg-gray-100 text-gray-500 border-gray-200', accent: 'border-l-gray-200', icon: HelpCircle },
};

function styleFor(state: AgentStateKind) {
  return STATE_STYLES[state] ?? STATE_STYLES.unknown;
}

export function sortSessions(sessions: SessionSummary[]): SessionSummary[] {
  // A dismissed run sits at rest even if its underlying state still wants a
  // human — it only stops being sorted up front, never re-sorts itself.
  const rank = (s: SessionSummary) => (s.dismissed ? STATE_STYLES.idle.rank : styleFor(s.state).rank);
  return [...sessions].sort((a, b) => {
    const byState = rank(a) - rank(b);
    if (byState !== 0) return byState;
    return (a.idle_seconds ?? 0) - (b.idle_seconds ?? 0);
  });
}

function StateChip({ session }: { session: SessionSummary }) {
  const { chip, icon: Icon, spin } = styleFor(session.state);
  // A dismissed state keeps its truthful label but loses the alarm colors:
  // someone has seen it, and the run's next call re-arms the badge.
  const palette = session.dismissed ? 'bg-gray-100 text-gray-500 border-gray-200' : chip;
  return (
    <span className={`inline-flex items-center gap-1.5 px-2.5 py-1 rounded-full text-xs font-semibold border ${palette}`}>
      <Icon size={13} className={spin && !session.dismissed ? 'animate-spin' : ''} />
      {session.state_label}
    </span>
  );
}

/// A quota bar, shown only when the provider actually reported the numbers —
/// an empty bar would read as "zero left", which is the opposite of "unknown".
function QuotaBar({ label, remaining, limit }: { label: string; remaining: number | null; limit: number | null }) {
  if (remaining === null || limit === null || limit <= 0) return null;
  const ratio = Math.max(0, Math.min(1, remaining / limit));
  const color = ratio > 0.5 ? 'bg-emerald-500' : ratio > 0.15 ? 'bg-amber-500' : 'bg-red-500';

  return (
    <div className="space-y-1 min-w-[9rem]">
      <div className="flex justify-between text-xs text-gray-500">
        <span>{label}</span>
        <span className="font-mono">{formatTokens(remaining)} / {formatTokens(limit)}</span>
      </div>
      <div className="h-1.5 w-full rounded-full bg-gray-200 overflow-hidden">
        <div className={`h-full rounded-full ${color}`} style={{ width: `${ratio * 100}%` }} />
      </div>
    </div>
  );
}

function ProviderLimitsCard({ limits }: { limits: ProviderLimits[] }) {
  const reporting = limits.filter((l) => l.requests_limit !== null || l.tokens_limit !== null);
  if (reporting.length === 0) return null;

  return (
    <div className="surface p-5">
      <h3 className="text-sm font-semibold text-gray-700 mb-4">Provider quota</h3>
      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-6">
        {reporting.map((limit) => (
          <div key={limit.provider ?? 'unknown'} className="space-y-3">
            <div className="flex items-baseline justify-between">
              <span className="font-medium text-gray-800 capitalize">{limit.provider ?? 'unknown'}</span>
              <span className="text-xs text-gray-400">read {humanizeSecs(limit.observed_seconds_ago)} ago</span>
            </div>
            <QuotaBar label="Requests" remaining={limit.requests_remaining} limit={limit.requests_limit} />
            <QuotaBar label="Tokens" remaining={limit.tokens_remaining} limit={limit.tokens_limit} />
            {limit.retry_after_remaining_s !== null && (
              <p className="text-xs text-red-600 font-medium">
                Blocked — retry in {humanizeSecs(limit.retry_after_remaining_s)}
              </p>
            )}
          </div>
        ))}
      </div>
    </div>
  );
}

function SessionCard({ session }: { session: SessionSummary }) {
  const { accent } = styleFor(session.state);
  const attentionRing = session.needs_attention ? 'ring-1 ring-amber-200' : '';
  const { refresh } = useSessions();
  const [dismissing, setDismissing] = useState(false);
  const [dismissError, setDismissError] = useState<string | null>(null);

  const dismiss = async () => {
    setDismissing(true);
    setDismissError(null);
    try {
      await dismissSession(session);
      await refresh();
    } catch (err) {
      setDismissError(err instanceof Error ? err.message : String(err));
    } finally {
      setDismissing(false);
    }
  };

  return (
    <div className={`rounded-2xl bg-white border border-slate-200/80 border-l-[3px] ${accent} ${attentionRing} p-5 space-y-3 shadow-[0_1px_2px_rgba(15,23,42,0.03)] transition-all hover:-translate-y-0.5 hover:shadow-[0_10px_30px_rgba(15,23,42,0.07)]`}>
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <Bot size={16} className="text-gray-400 shrink-0" />
            <span className="font-semibold text-gray-800 truncate">{session.agent_name}</span>
            {session.experiment_name && (
              <span className="px-2 py-0.5 rounded-full bg-indigo-50 text-indigo-700 text-xs shrink-0">
                {session.experiment_name}
              </span>
            )}
          </div>
          <p className="text-xs text-gray-400 font-mono truncate mt-1" title={session.session_id ?? ''}>
            {session.session_id ?? 'no session id'}
          </p>
        </div>
        <div className="flex items-center gap-1.5 shrink-0">
          <StateChip session={session} />
          {session.needs_attention && (
            <button
              type="button"
              onClick={dismiss}
              disabled={dismissing}
              title="Dismiss — I've seen this. Re-arms on the run's next call."
              aria-label={`Dismiss attention on ${session.agent_name} ${session.session_id ?? ''}`.trimEnd()}
              className="text-gray-400 hover:text-gray-700 disabled:opacity-50 p-1 rounded-full hover:bg-gray-100 transition-colors"
            >
              <BellOff size={14} />
            </button>
          )}
        </div>
      </div>

      {dismissError && (
        <p role="alert" className="text-xs text-red-600">
          Could not dismiss: {dismissError}
        </p>
      )}

      {session.state_detail && (
        <p className={`text-sm ${session.needs_attention ? 'text-gray-800' : 'text-gray-500'}`}>
          {session.state_detail}
        </p>
      )}

      {session.last_task_description && (
        <p className="text-xs text-gray-400 line-clamp-2" title={session.last_task_description}>
          Last turn: {session.last_task_description}
        </p>
      )}

      <div className="flex flex-wrap items-center gap-x-5 gap-y-1 text-xs text-gray-500 pt-2 border-t border-gray-50">
        <span className="font-mono text-gray-600">{session.model_name ?? 'unknown model'}</span>
        <span>{session.call_count} call{session.call_count === 1 ? '' : 's'}</span>
        <span title="Input / output tokens across the session">
          {formatTokens(session.input_tokens)} in / {formatTokens(session.output_tokens)} out
        </span>
        <span className="font-semibold text-gray-700">
          {session.unpriced_calls === session.call_count ? '–' : formatCost(session.total_cost)}
        </span>
        <span className="flex items-center gap-1">
          <Clock size={12} />
          {humanizeSecs(session.idle_seconds)} ago
        </span>
        {session.rate_limited_calls > 0 && (
          <span className="text-red-600">{session.rate_limited_calls} rate-limited</span>
        )}
        {session.error_calls > 0 && <span className="text-red-600">{session.error_calls} failed</span>}
      </div>
    </div>
  );
}

function SummaryStrip({ sessions }: { sessions: SessionSummary[] }) {
  const counts = {
    attention: sessions.filter((s) => s.needs_attention).length,
    working: sessions.filter((s) => s.state === 'working').length,
    idle: sessions.filter((s) => s.state === 'idle' || s.state === 'unknown').length,
  };
  const cost = sessions.reduce((sum, s) => sum + (s.total_cost ?? 0), 0);

  const tiles = [
    { label: 'Need you', value: counts.attention, tone: counts.attention > 0 ? 'text-amber-600' : 'text-gray-800' },
    { label: 'Working', value: counts.working, tone: 'text-blue-600' },
    { label: 'Idle', value: counts.idle, tone: 'text-gray-400' },
    { label: 'Spend', value: formatCost(cost), tone: 'text-gray-800' },
  ];

  return (
    <div className="surface grid grid-cols-2 divide-x divide-y divide-slate-100 overflow-hidden lg:grid-cols-4 lg:divide-y-0">
      {tiles.map((tile) => (
        <div key={tile.label} className="px-5 py-4 sm:px-6">
          <p className="text-[10px] uppercase tracking-[0.14em] font-semibold text-slate-400">{tile.label}</p>
          <p className={`mt-1 text-2xl font-semibold tracking-tight ${tile.tone}`}>{tile.value}</p>
        </div>
      ))}
    </div>
  );
}

const AgentStatusView = () => {
  const { sessions, limits, error, loaded, live } = useSessions();
  const ordered = sortSessions(sessions);

  return (
    <div className="page-wrap">
      <div className="flex justify-between items-center">
        <div>
          <h2 className="text-xl font-semibold tracking-tight text-slate-900">Live agents</h2>
          <p className="mt-1 text-sm text-slate-500">See what is running, blocked, and costing you—without opening every agent.</p>
        </div>
        <span
          className="flex items-center gap-2 text-xs text-gray-500"
          title={live ? 'Streaming updates from the proxy' : 'Falling back to polling every few seconds'}
        >
          {live ? <Wifi size={14} className="text-emerald-500" /> : <WifiOff size={14} className="text-gray-400" />}
          {live ? 'Live' : 'Polling'}
        </span>
      </div>

      {error && (
        <div className="rounded-xl border border-red-200 bg-red-50 px-5 py-4 text-sm text-red-800">
          <p className="font-semibold">Can't reach the Harnesswurm backend.</p>
          <p className="mt-1 text-red-700">{error}</p>
        </div>
      )}

      <SummaryStrip sessions={sessions} />
      <ProviderLimitsCard limits={limits} />

      {ordered.length > 0 ? (
        <div className="grid grid-cols-1 xl:grid-cols-2 gap-4">
          {ordered.map((session) => (
            <SessionCard key={`${session.agent_name}:${session.session_id ?? ''}`} session={session} />
          ))}
        </div>
      ) : (
        !error && (
          <div className="surface py-16 text-center">
            <Bot size={40} className="mx-auto text-gray-200 mb-3" />
            <p className="text-gray-400 text-sm">
              {loaded
                ? 'No agent sessions yet. Point an agent at this proxy and send it a task.'
                : 'Loading agent sessions…'}
            </p>
          </div>
        )
      )}
    </div>
  );
};

export default AgentStatusView;
