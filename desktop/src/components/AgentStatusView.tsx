import React, { useState } from 'react';
import {
  AlertTriangle,
  Ban,
  BellOff,
  Bot,
  ChevronDown,
  ChevronRight,
  CircleSlash,
  Clock,
  CloudOff,
  HelpCircle,
  Loader2,
  MessageCircleQuestion,
  Moon,
  Scissors,
  Trash2,
  Wifi,
  WifiOff,
} from 'lucide-react';
import { AgentStateKind, deleteAgent, dismissSession, ProviderLimits, SessionSummary, formatCost, formatTokens, humanizeSecs } from '../lib/api';
import { useSessions } from '../hooks/useSessions';

/// Per-state presentation. Kept as one table rather than scattered
/// conditionals so adding a state to the backend is a single edit here, and
/// so the ordering below has one place to read priorities from.
///
/// `rank` decides sort order: whatever wants a human comes first, then
/// whatever is actively running, then everything at rest. A dashboard sorted
/// purely by time buries the one session that is blocked on you.
///
/// The same rank doubles as the Running/Finished split: below `interrupted`
/// the session is still live (it wants you, or it is working/blocked), from
/// `interrupted` up it is at rest.
const STATE_STYLES: Record<AgentStateKind, {
  rank: number;
  chip: string;
  accent: string;
  icon: React.ComponentType<{ size?: number | string; className?: string }>;
  spin?: boolean;
}> = {
  waiting_for_you: { rank: 0, chip: 'bg-amber-400/10 text-amber-300 border-amber-400/30', accent: 'border-l-amber-400', icon: MessageCircleQuestion },
  rate_limited: { rank: 1, chip: 'bg-red-500/10 text-red-300 border-red-500/30', accent: 'border-l-red-500', icon: Ban },
  stalled: { rank: 2, chip: 'bg-red-500/10 text-red-300 border-red-500/30', accent: 'border-l-red-500', icon: AlertTriangle },
  error: { rank: 3, chip: 'bg-red-500/10 text-red-300 border-red-500/30', accent: 'border-l-red-500', icon: AlertTriangle },
  truncated: { rank: 4, chip: 'bg-purple-400/10 text-purple-300 border-purple-400/30', accent: 'border-l-purple-400', icon: Scissors },
  working: { rank: 5, chip: 'bg-blue-500/10 text-blue-300 border-blue-500/30', accent: 'border-l-blue-500', icon: Loader2, spin: true },
  overloaded: { rank: 6, chip: 'bg-orange-400/10 text-orange-300 border-orange-400/30', accent: 'border-l-orange-400', icon: CloudOff },
  interrupted: { rank: 7, chip: 'bg-slate-400/10 text-slate-300 border-slate-400/20', accent: 'border-l-slate-400', icon: CircleSlash },
  idle: { rank: 8, chip: 'bg-white/[0.06] text-slate-400 border-white/10', accent: 'border-l-slate-600', icon: Moon },
  unknown: { rank: 9, chip: 'bg-white/[0.06] text-slate-500 border-white/10', accent: 'border-l-slate-700', icon: HelpCircle },
};

const AT_REST_RANK = STATE_STYLES.interrupted.rank;

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

export interface SessionGroups {
  running: SessionSummary[];
  finished: SessionSummary[];
}

/// Splits the sorted sessions the way the sidecar lists them. Dismissed runs
/// land under Finished whatever their state — someone has seen them.
export function groupSessions(sessions: SessionSummary[]): SessionGroups {
  const running: SessionSummary[] = [];
  const finished: SessionSummary[] = [];
  for (const session of sessions) {
    const rank = session.dismissed ? STATE_STYLES.idle.rank : styleFor(session.state).rank;
    (rank < AT_REST_RANK ? running : finished).push(session);
  }
  return { running, finished };
}

function StateChip({ session }: { session: SessionSummary }) {
  const { chip, icon: Icon, spin } = styleFor(session.state);
  // A dismissed state keeps its truthful label but loses the alarm colors:
  // someone has seen it, and the run's next call re-arms the badge.
  const palette = session.dismissed ? 'bg-white/[0.06] text-slate-500 border-white/10' : chip;
  return (
    <span className={`inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-[11px] font-semibold ${palette}`}>
      <Icon size={12} className={spin && !session.dismissed ? 'animate-spin' : ''} />
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
    <div className="space-y-1">
      <div className="flex justify-between text-xs text-slate-400">
        <span>{label}</span>
        <span className="font-mono">{formatTokens(remaining)} / {formatTokens(limit)}</span>
      </div>
      <div className="h-1.5 w-full rounded-full bg-white/10 overflow-hidden">
        <div className={`h-full rounded-full ${color}`} style={{ width: `${ratio * 100}%` }} />
      </div>
    </div>
  );
}

function ProviderLimitsCard({ limits }: { limits: ProviderLimits[] }) {
  const reporting = limits.filter((l) => l.requests_limit !== null || l.tokens_limit !== null);
  if (reporting.length === 0) return null;

  return (
    <div className="surface p-3.5">
      <h3 className="text-sm font-semibold text-slate-200 mb-3">Provider quota</h3>
      <div className="space-y-4">
        {reporting.map((limit) => (
          <div key={limit.provider ?? 'unknown'} className="space-y-2.5">
            <div className="flex items-baseline justify-between">
              <span className="font-medium text-slate-200 capitalize">{limit.provider ?? 'unknown'}</span>
              <span className="text-xs text-slate-500">read {humanizeSecs(limit.observed_seconds_ago)} ago</span>
            </div>
            <QuotaBar label="Requests" remaining={limit.requests_remaining} limit={limit.requests_limit} />
            <QuotaBar label="Tokens" remaining={limit.tokens_remaining} limit={limit.tokens_limit} />
            {limit.retry_after_remaining_s !== null && (
              <p className="text-xs text-red-300 font-medium">
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
  const { accent, icon: StateIcon, spin } = styleFor(session.state);
  const attentionRing = session.needs_attention ? 'ring-1 ring-amber-400/40' : '';
  const { refresh } = useSessions();
  const [dismissing, setDismissing] = useState(false);
  const [dismissError, setDismissError] = useState<string | null>(null);
  const [deleting, setDeleting] = useState(false);

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

  const removeAgent = async () => {
    if (!window.confirm(`Delete agent "${session.agent_name}" and all its recorded calls? This cannot be undone.`)) {
      return;
    }
    setDeleting(true);
    setDismissError(null);
    try {
      await deleteAgent(session.agent_name);
      await refresh();
    } catch (err) {
      setDismissError(err instanceof Error ? err.message : String(err));
    } finally {
      setDeleting(false);
    }
  };

  return (
    <div className={`rounded-xl border border-white/10 border-l-[3px] bg-[#151a23] ${accent} ${attentionRing} space-y-2 p-3 transition-all`}>
      <div className="flex items-start gap-2.5">
        <div className={`grid h-8 w-8 shrink-0 place-items-center rounded-lg bg-white/[0.05] ${session.dismissed ? 'text-slate-500' : 'text-slate-300'}`}>
          <StateIcon size={15} className={spin && !session.dismissed ? 'animate-spin text-blue-300' : ''} />
        </div>
        <div className="min-w-0 flex-1">
          <div className="flex items-baseline justify-between gap-2">
            <span className="truncate text-sm font-semibold text-slate-100">{session.agent_name}</span>
            <span className="shrink-0 font-semibold text-slate-200 text-xs">
              {session.unpriced_calls === session.call_count ? '–' : formatCost(session.total_cost)}
            </span>
          </div>
          <div className="mt-1 flex items-center justify-between gap-2">
            <StateChip session={session} />
            <div className="flex min-w-0 items-center gap-2">
              {session.experiment_name && (
                <span className="truncate text-[10px] text-indigo-300" title={session.experiment_name}>
                  {session.experiment_name}
                </span>
              )}
              <span className="flex shrink-0 items-center gap-1 text-[10px] text-slate-500">
                <Clock size={10} />
                {humanizeSecs(session.idle_seconds)} ago
              </span>
            </div>
          </div>
        </div>
      </div>

      {dismissError && (
        <p role="alert" className="text-xs text-red-300">
          Could not dismiss: {dismissError}
        </p>
      )}

      {session.state_detail && (
        <p className={`text-xs leading-relaxed ${session.needs_attention ? 'text-slate-100' : 'text-slate-400'}`}>
          {session.state_detail}
        </p>
      )}

      {session.last_task_description && (
        <p className="text-xs text-slate-400 line-clamp-2" title={session.last_task_description}>
          Last turn: {session.last_task_description}
        </p>
      )}

      <div className="flex items-center justify-between gap-2 border-t border-white/5 pt-2 text-[11px] text-slate-500">
        <div className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-0.5">
          <span className="max-w-full truncate font-mono text-slate-400" title={session.session_id ?? ''}>
            {session.session_id ?? 'no session id'}
          </span>
          <span className="truncate font-mono">{session.model_name ?? 'unknown model'}</span>
          <span>{session.call_count} call{session.call_count === 1 ? '' : 's'}</span>
          <span title="Input / output tokens across the session">
            {formatTokens(session.input_tokens)} in / {formatTokens(session.output_tokens)} out
          </span>
          {session.rate_limited_calls > 0 && (
            <span className="text-red-300">{session.rate_limited_calls} rate-limited</span>
          )}
          {session.error_calls > 0 && <span className="text-red-300">{session.error_calls} failed</span>}
        </div>
        <div className="flex shrink-0 items-center gap-0.5">
          {session.needs_attention && (
            <button
              type="button"
              onClick={dismiss}
              disabled={dismissing}
              title="Dismiss — I've seen this. Re-arms on the run's next call."
              aria-label={`Dismiss attention on ${session.agent_name} ${session.session_id ?? ''}`.trimEnd()}
              className="text-slate-500 hover:text-slate-200 disabled:opacity-50 p-1 rounded-full hover:bg-white/10 transition-colors"
            >
              <BellOff size={14} />
            </button>
          )}
          <button
            type="button"
            onClick={removeAgent}
            disabled={deleting}
            title="Delete this agent and all its recorded calls"
            aria-label={`Delete agent ${session.agent_name}`}
            className="text-slate-500 hover:text-red-300 disabled:opacity-50 p-1 rounded-full hover:bg-white/10 transition-colors"
          >
            <Trash2 size={14} />
          </button>
        </div>
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
    { label: 'Need you', value: counts.attention, tone: counts.attention > 0 ? 'text-amber-300' : 'text-slate-100' },
    { label: 'Working', value: counts.working, tone: 'text-blue-300' },
    { label: 'Idle', value: counts.idle, tone: 'text-slate-500' },
    { label: 'Spend', value: formatCost(cost), tone: 'text-slate-100' },
  ];

  return (
    <div className="surface grid grid-cols-2 divide-x divide-y divide-white/5 overflow-hidden sm:grid-cols-4 sm:divide-y-0">
      {tiles.map((tile) => (
        <div key={tile.label} className="px-3 py-2.5">
          <p className="text-[10px] uppercase tracking-[0.14em] font-semibold text-slate-400">{tile.label}</p>
          <p className={`mt-0.5 text-xl font-semibold tracking-tight ${tile.tone}`}>{tile.value}</p>
        </div>
      ))}
    </div>
  );
}

/// A collapsible Running/Finished group, blume-style. Empty groups render
/// nothing at all — a "Running (0)" header is noise.
function SessionSection({ label, sessions }: { label: string; sessions: SessionSummary[] }) {
  const [open, setOpen] = useState(true);
  if (sessions.length === 0) return null;

  return (
    <section>
      <button
        type="button"
        onClick={() => setOpen(!open)}
        aria-expanded={open}
        className="flex w-full items-center gap-1.5 py-1 text-[10px] font-semibold uppercase tracking-[0.16em] text-slate-500 transition-colors hover:text-slate-300"
      >
        {open ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
        {label}
        <span className="rounded-full bg-white/[0.06] px-1.5 py-px text-[10px] tracking-normal text-slate-400">
          {sessions.length}
        </span>
      </button>
      {open && (
        <div className="space-y-2 pt-1">
          {sessions.map((session) => (
            <SessionCard key={`${session.agent_name}:${session.session_id ?? ''}`} session={session} />
          ))}
        </div>
      )}
    </section>
  );
}

const AgentStatusView = () => {
  const { sessions, limits, error, loaded, live } = useSessions();
  const ordered = sortSessions(sessions);
  const { running, finished } = groupSessions(ordered);

  return (
    <div className="page-wrap">
      <div className="flex items-center justify-between gap-2">
        <h2 className="text-lg font-semibold tracking-tight text-slate-100">Live agents</h2>
        <span
          className="flex items-center gap-1.5 text-xs text-slate-500"
          title={live ? 'Streaming updates from the proxy' : 'Falling back to polling every few seconds'}
        >
          {live ? <Wifi size={13} className="text-emerald-500" /> : <WifiOff size={13} className="text-gray-400" />}
          {live ? 'Live' : 'Polling'}
        </span>
      </div>

      {error && (
        <div className="rounded-xl border border-red-500/30 bg-red-500/10 px-3.5 py-3 text-sm text-red-200">
          <p className="font-semibold">Can't reach the Harnesswurm backend.</p>
          <p className="mt-1 text-red-300/80">{error}</p>
        </div>
      )}

      <SummaryStrip sessions={sessions} />
      <ProviderLimitsCard limits={limits} />

      {ordered.length > 0 ? (
        <div className="space-y-3">
          <SessionSection label="Running" sessions={running} />
          <SessionSection label="Finished" sessions={finished} />
        </div>
      ) : (
        !error && (
          <div className="surface py-12 text-center">
            <Bot size={36} className="mx-auto text-slate-700 mb-3" />
            <p className="px-6 text-slate-500 text-sm">
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
