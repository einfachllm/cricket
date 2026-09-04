import React, { useEffect, useState } from 'react';
import { Radio, RefreshCw, X, MessageCircleQuestion } from 'lucide-react';
import { fetchJson, formatCost, formatTimestampUtc, formatTokens } from '../lib/api';
import { CollapsibleJson } from './CollapsibleJson';

interface TaskSummary {
  task_id: number;
  agent_name: string;
  model_name: string | null;
  provider: string | null;
  session_id: string | null;
  timestamp: string;
  task_description: string | null;
  experiment_name: string | null;
  prompt_tokens: number | null;
  completion_tokens: number | null;
  cache_creation_tokens: number | null;
  cache_read_tokens: number | null;
  tool_calls_count: number | null;
  latency_ms: number | null;
  cost_estimate: number | null;
  agent_question_tool: string | null;
  agent_question_text: string | null;
  status: string | null;
  http_status: number | null;
  error_type: string | null;
  error_message: string | null;
  stop_reason: string | null;
  awaiting_input: boolean;
  ttfb_ms: number | null;
  duration_ms: number | null;
}

interface TaskTraffic {
  task_id: number;
  agent_name: string;
  model_name: string | null;
  provider: string | null;
  timestamp: string;
  task_description: string | null;
  request_body: string | null;
  response_body: string | null;
  agent_question_tool: string | null;
  agent_question_text: string | null;
}

/// Per-call outcome, colored so a screen of traffic reads at a glance: red
/// is a call that failed, amber is one that stopped to ask you something.
const STATUS_STYLES: Record<string, { color: string; label: string }> = {
  ok: { color: 'bg-emerald-500/15 text-emerald-300', label: 'ok' },
  in_flight: { color: 'bg-blue-500/15 text-blue-300', label: 'running' },
  rate_limited: { color: 'bg-red-500/15 text-red-300', label: 'rate limited' },
  overloaded: { color: 'bg-orange-400/15 text-orange-300', label: 'overloaded' },
  error: { color: 'bg-red-500/15 text-red-300', label: 'error' },
  interrupted: { color: 'bg-slate-400/10 text-slate-300', label: 'cut off' },
};

function StatusBadge({ task }: { task: TaskSummary }) {
  if (!task.status) return <span className="text-slate-600">–</span>;
  const style = STATUS_STYLES[task.status] ?? { color: 'bg-white/[0.06] text-slate-300', label: task.status };
  // The provider's own message is the fastest way to understand a failure,
  // so it rides along as the tooltip rather than needing a click-through.
  const title = [task.error_type, task.error_message, task.http_status ? `HTTP ${task.http_status}` : null]
    .filter(Boolean)
    .join(' — ');

  return (
    <span className={`px-2 py-0.5 rounded-full text-xs font-medium ${style.color}`} title={title || undefined}>
      {style.label}
    </span>
  );
}

/// How long the call actually took. `duration_ms` is preferred over
/// `latency_ms` because the latter is only time-to-headers — on a streamed
/// call that excludes the entire generation, which is most of the wait.
/// Older rows predate `duration_ms` and fall back to what they have.
function formatDuration(task: TaskSummary): string {
  const ms = task.duration_ms ?? task.latency_ms;
  return ms === null || ms === undefined ? '–' : `${ms}ms`;
}

export const PAGE_SIZE = 50;

export function filterTasks(
  tasks: TaskSummary[],
  query: string,
  agentFilter: string,
  questionsOnly: boolean,
): TaskSummary[] {
  const q = query.trim().toLowerCase();
  return tasks
    .filter((t) => agentFilter === 'all' || t.agent_name === agentFilter)
    .filter((t) => !questionsOnly || !!t.agent_question_text)
    .filter(
      (t) =>
        q === '' ||
        t.agent_name.toLowerCase().includes(q) ||
        (t.task_description ?? '').toLowerCase().includes(q) ||
        (t.session_id ?? '').toLowerCase().includes(q),
    );
}

const TrafficView = () => {
  const [tasks, setTasks] = useState<TaskSummary[]>([]);
  const [agentFilter, setAgentFilter] = useState<string>('all');
  const [questionsOnly, setQuestionsOnly] = useState(false);
  const [query, setQuery] = useState('');
  const [page, setPage] = useState(0);
  const [loading, setLoading] = useState(false);
  const [selectedId, setSelectedId] = useState<number | null>(() => {
    const raw = new URLSearchParams(window.location.search).get("task");
    const parsed = raw === null ? NaN : Number(raw);
    return Number.isInteger(parsed) ? parsed : null;
  });
  const [traffic, setTraffic] = useState<TaskTraffic | null>(null);
  const [trafficLoading, setTrafficLoading] = useState(false);

  const fetchTasks = async () => {
    setLoading(true);
    try {
      const data = await fetchJson<TaskSummary[]>('/v1/analytics/tasks');
      setTasks(data);
    } catch (error) {
      console.error('Error fetching tasks:', error);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    fetchTasks();
  }, []);

  useEffect(() => {
    if (selectedId === null) {
      setTraffic(null);
      return;
    }
    // A newly selected task can resolve before an older, larger one still
    // in flight; without a staleness guard the older response would land
    // last and overwrite `traffic` for a task that's no longer selected.
    let cancelled = false;
    const controller = new AbortController();
    setTrafficLoading(true);
    fetchJson<TaskTraffic>(`/v1/analytics/tasks/${selectedId}/traffic`, { signal: controller.signal })
      .then((data) => {
        if (!cancelled) setTraffic(data);
      })
      .catch((error) => {
        if (!cancelled && error?.name !== 'AbortError') {
          console.error('Error fetching traffic:', error);
        }
      })
      .finally(() => {
        if (!cancelled) setTrafficLoading(false);
      });
    return () => {
      cancelled = true;
      controller.abort();
    };
  }, [selectedId]);

  const agents = Array.from(new Set(tasks.map((t) => t.agent_name))).sort();
  const visibleTasks = filterTasks(tasks, query, agentFilter, questionsOnly);
  const pageCount = Math.max(1, Math.ceil(visibleTasks.length / PAGE_SIZE));
  const safePage = Math.min(page, pageCount - 1);
  const pagedTasks = visibleTasks.slice(safePage * PAGE_SIZE, safePage * PAGE_SIZE + PAGE_SIZE);

  return (
    <div className="page-wrap">
      <div className="flex items-center justify-between gap-2">
        <h2 className="flex items-center gap-2 text-lg font-semibold tracking-tight text-slate-100">
          <Radio size={18} className="text-indigo-400" />
          Traffic
        </h2>
        <button
          onClick={fetchTasks}
          title="Reload the captured calls"
          className="flex items-center gap-1.5 rounded-lg border border-white/10 bg-white/[0.04] px-2.5 py-1.5 text-xs text-slate-300 hover:bg-white/[0.08]"
        >
          <RefreshCw size={13} className={loading ? 'animate-spin' : ''} />
          Refresh
        </button>
      </div>

      <div className="space-y-2">
        <input
          value={query}
          onChange={(e) => { setQuery(e.target.value); setPage(0); }}
          placeholder="Search agent or task..."
          className="w-full border border-white/10 rounded-lg px-3 py-2 text-sm bg-white/[0.04] text-slate-200 placeholder-slate-500"
        />
        <div className="flex items-center gap-2">
          <select
            value={agentFilter}
            onChange={(e) => { setAgentFilter(e.target.value); setPage(0); }}
            className="min-w-0 flex-1 border border-white/10 rounded-lg px-2 py-1.5 text-sm bg-white/[0.04] text-slate-200"
          >
            <option value="all">All agents</option>
            {agents.map((a) => (
              <option key={a} value={a}>{a}</option>
            ))}
          </select>
          <label className="flex shrink-0 items-center gap-1.5 text-xs text-slate-400 select-none cursor-pointer">
            <input
              type="checkbox"
              checked={questionsOnly}
              onChange={(e) => { setQuestionsOnly(e.target.checked); setPage(0); }}
              className="rounded border-white/20 bg-white/[0.04]"
            />
            Questions only
          </label>
        </div>
      </div>

      <div className="surface overflow-hidden">
        <div className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead className="bg-white/[0.04] text-slate-500 uppercase text-xs">
              <tr>
                <th className="text-left px-3 py-2.5 font-semibold">Call</th>
                <th className="text-left px-3 py-2.5 font-semibold">Status</th>
                <th className="text-right px-3 py-2.5 font-semibold">Cost</th>
              </tr>
            </thead>
            <tbody>
              {pagedTasks.map((task) => (
                <tr
                  key={task.task_id}
                  onClick={() => setSelectedId(task.task_id)}
                  className={`border-t border-white/5 cursor-pointer align-top hover:bg-white/[0.03] transition-colors ${
                    selectedId === task.task_id ? 'bg-indigo-500/10' : ''
                  }`}
                >
                  <td className="px-3 py-2.5 min-w-0">
                    <span className="flex items-center gap-1.5 text-slate-300">
                      {task.agent_question_text && (
                        <span title={`Agent asked: ${task.agent_question_text}`} className="shrink-0 text-amber-400">
                          <MessageCircleQuestion size={14} />
                        </span>
                      )}
                      <span className="truncate" title={task.task_description || ''}>
                        {task.task_description || <span className="text-slate-600">–</span>}
                      </span>
                    </span>
                    <span className="mt-0.5 flex items-center gap-1.5 text-xs text-slate-500">
                      <span className="font-medium text-slate-400">{task.agent_name}</span>
                      <span aria-hidden>·</span>
                      <span className="truncate font-mono text-xs" title={task.session_id ?? ''}>
                        {task.session_id || <span className="text-slate-600">–</span>}
                      </span>
                    </span>
                    <span className="mt-0.5 flex items-center gap-1.5 text-[11px] text-slate-600">
                      <span className="truncate" title={`${task.provider || 'unknown'} · ${task.model_name || 'unknown model'}`}>
                        {task.provider || 'unknown'} · {task.model_name || 'unknown model'}
                      </span>
                      <span aria-hidden>·</span>
                      <span className="shrink-0">{formatTimestampUtc(task.timestamp)}</span>
                    </span>
                  </td>
                  <td className="px-3 py-2.5">
                    <StatusBadge task={task} />
                    <span
                      className="mt-1 block text-[11px] text-slate-600"
                      title={task.ttfb_ms !== null ? `${task.ttfb_ms}ms to first byte` : undefined}
                    >
                      {formatDuration(task)}
                    </span>
                  </td>
                  <td className="px-3 py-2.5 text-right">
                    <span className="block font-semibold text-slate-100 tabular-nums">{formatCost(task.cost_estimate)}</span>
                    <span className="mt-0.5 block text-[11px] text-slate-500 tabular-nums" title="Input / output tokens">
                      {formatTokens(task.prompt_tokens)} / {formatTokens(task.completion_tokens)}
                    </span>
                    <span className="mt-0.5 block text-[11px] text-slate-600 tabular-nums" title="Tool calls">
                      {task.tool_calls_count ?? 0} tool{task.tool_calls_count === 1 ? '' : 's'}
                    </span>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          {visibleTasks.length === 0 && (
            <p className="text-center text-slate-500 py-10 text-sm">
              {loading ? 'Loading...' : 'No traffic captured yet. Point an agent at this proxy and run a task.'}
            </p>
          )}
          {visibleTasks.length > 0 && (
            <div className="flex items-center justify-between px-3 py-2.5 border-t border-white/5 text-sm text-slate-400">
              <button
                onClick={() => setPage((p) => Math.max(0, p - 1))}
                disabled={safePage === 0}
                className="px-3 py-1 rounded-lg border border-white/10 hover:bg-white/[0.05] disabled:opacity-50 disabled:hover:bg-transparent"
              >
                Prev
              </button>
              <span>Page {safePage + 1} of {pageCount}</span>
              <button
                onClick={() => setPage((p) => Math.min(pageCount - 1, p + 1))}
                disabled={safePage >= pageCount - 1}
                className="px-3 py-1 rounded-lg border border-white/10 hover:bg-white/[0.05] disabled:opacity-50 disabled:hover:bg-transparent"
              >
                Next
              </button>
            </div>
          )}
        </div>
      </div>

      {/* In a window this narrow the raw traffic cannot sit below a long
          table — it would land off-screen. It covers the view as a sheet
          instead, and closing returns to the list exactly as it was. */}
      {selectedId !== null && (
        <div className="fixed inset-0 z-40 overflow-y-auto bg-[#0b0e14]/[0.97] p-3 backdrop-blur-sm">
          <div className="surface space-y-4 p-4">
            <div className="flex justify-between items-start gap-4">
              <div>
                <h3 className="text-base font-semibold text-slate-200">
                  Task #{selectedId} raw traffic
                </h3>
                {traffic?.task_description && (
                  <p className="text-sm text-slate-500 mt-1">{traffic.task_description}</p>
                )}
              </div>
              <button onClick={() => setSelectedId(null)} aria-label="Close task detail" className="text-slate-500 hover:text-slate-300 shrink-0">
                <X size={18} />
              </button>
            </div>
            {traffic?.agent_question_text && (
              <div className="flex items-start gap-2 rounded-lg bg-amber-400/10 border border-amber-400/20 px-3 py-2">
                <MessageCircleQuestion size={16} className="text-amber-400 shrink-0 mt-0.5" />
                <div>
                  <p className="text-xs font-semibold text-amber-300 uppercase">
                    Agent asked{traffic.agent_question_tool ? ` (${traffic.agent_question_tool})` : ''}
                  </p>
                  <p className="text-sm text-amber-200">{traffic.agent_question_text}</p>
                </div>
              </div>
            )}
            {trafficLoading ? (
              <div className="h-32 flex items-center justify-center">
                <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-indigo-400"></div>
              </div>
            ) : (
              <div className="space-y-4">
                <div>
                  <p className="text-xs uppercase font-semibold text-slate-500 mb-2">Request</p>
                  <div className="bg-black/40 border border-white/10 text-slate-200 text-xs rounded-lg p-3 overflow-auto max-h-96 whitespace-pre-wrap break-words">
                    <CollapsibleJson raw={traffic?.request_body ?? null} />
                  </div>
                </div>
                <div>
                  <p className="text-xs uppercase font-semibold text-slate-500 mb-2">Response</p>
                  <div className="bg-black/40 border border-white/10 text-slate-200 text-xs rounded-lg p-3 overflow-auto max-h-96 whitespace-pre-wrap break-words">
                    <CollapsibleJson raw={traffic?.response_body ?? null} />
                  </div>
                </div>
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
};

export default TrafficView;
