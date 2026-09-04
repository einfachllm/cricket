import React, { useEffect, useState } from 'react';
import { Radio, RefreshCw, X, MessageCircleQuestion } from 'lucide-react';
import { fetchJson, formatCost, formatTimestampUtc } from '../lib/api';
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
  ok: { color: 'bg-emerald-100 text-emerald-800', label: 'ok' },
  in_flight: { color: 'bg-blue-100 text-blue-800', label: 'running' },
  rate_limited: { color: 'bg-red-100 text-red-800', label: 'rate limited' },
  overloaded: { color: 'bg-orange-100 text-orange-800', label: 'overloaded' },
  error: { color: 'bg-red-100 text-red-800', label: 'error' },
  interrupted: { color: 'bg-slate-100 text-slate-700', label: 'cut off' },
};

function StatusBadge({ task }: { task: TaskSummary }) {
  if (!task.status) return <span className="text-gray-300">–</span>;
  const style = STATUS_STYLES[task.status] ?? { color: 'bg-gray-100 text-gray-600', label: task.status };
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

function ProviderBadge({ provider }: { provider: string | null }) {
  const color = provider === 'anthropic'
    ? 'bg-orange-100 text-orange-800'
    : provider === 'openai'
      ? 'bg-emerald-100 text-emerald-800'
      : 'bg-gray-100 text-gray-600';
  return (
    <span className={`px-2 py-0.5 rounded-full text-xs font-medium ${color}`}>
      {provider || 'unknown'}
    </span>
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
      <div className="flex justify-between items-center">
        <h2 className="text-3xl font-bold text-gray-800 flex items-center gap-2">
          <Radio size={28} className="text-blue-600" />
          Traffic
        </h2>
        <div className="flex items-center gap-3">
          <input
            value={query}
            onChange={(e) => { setQuery(e.target.value); setPage(0); }}
            placeholder="Search agent or task..."
            className="border border-gray-200 rounded-lg px-3 py-2 text-sm bg-white"
          />
          <select
            value={agentFilter}
            onChange={(e) => { setAgentFilter(e.target.value); setPage(0); }}
            className="border border-gray-200 rounded-lg px-3 py-2 text-sm bg-white"
          >
            <option value="all">All agents</option>
            {agents.map((a) => (
              <option key={a} value={a}>{a}</option>
            ))}
          </select>
          <label className="flex items-center gap-2 text-sm text-gray-600 select-none cursor-pointer">
            <input
              type="checkbox"
              checked={questionsOnly}
              onChange={(e) => { setQuestionsOnly(e.target.checked); setPage(0); }}
              className="rounded border-gray-300"
            />
            Questions only
          </label>
          <button
            onClick={fetchTasks}
            className="flex items-center gap-2 px-3 py-2 rounded-lg text-sm bg-white border border-gray-200 hover:bg-gray-50 text-gray-700"
          >
            <RefreshCw size={16} className={loading ? 'animate-spin' : ''} />
            Refresh
          </button>
        </div>
      </div>

      <div className="surface overflow-hidden">
        <div className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead className="bg-gray-50 text-gray-500 uppercase text-xs">
              <tr>
                <th className="text-left px-4 py-3 font-semibold">Time</th>
                <th className="text-left px-4 py-3 font-semibold">Agent</th>
                <th className="text-left px-4 py-3 font-semibold">Session</th>
                <th className="text-left px-4 py-3 font-semibold">Task</th>
                <th className="text-left px-4 py-3 font-semibold">Status</th>
                <th className="text-left px-4 py-3 font-semibold">Model</th>
                <th className="text-left px-4 py-3 font-semibold">Provider</th>
                <th className="text-right px-4 py-3 font-semibold">Input</th>
                <th className="text-right px-4 py-3 font-semibold">Cache R/W</th>
                <th className="text-right px-4 py-3 font-semibold">Output</th>
                <th className="text-right px-4 py-3 font-semibold">Tools</th>
                <th className="text-right px-4 py-3 font-semibold">Latency</th>
                <th className="text-right px-4 py-3 font-semibold">Cost</th>
              </tr>
            </thead>
            <tbody>
              {pagedTasks.map((task) => (
                <tr
                  key={task.task_id}
                  onClick={() => setSelectedId(task.task_id)}
                  className={`border-t border-gray-100 cursor-pointer hover:bg-blue-50 transition-colors ${
                    selectedId === task.task_id ? 'bg-blue-50' : ''
                  }`}
                >
                  <td className="px-4 py-3 text-gray-500 whitespace-nowrap">
                    {formatTimestampUtc(task.timestamp)}
                  </td>
                  <td className="px-4 py-3 font-medium text-gray-800">{task.agent_name}</td>
                  <td
                    className="px-4 py-3 text-gray-500 font-mono text-xs max-w-[10rem] truncate"
                    title={task.session_id ?? ''}
                  >
                    {task.session_id || <span className="text-gray-300">–</span>}
                  </td>
                  <td className="px-4 py-3 text-gray-600 max-w-xs truncate" title={task.task_description || ''}>
                    <span className="inline-flex items-center gap-1.5 max-w-full">
                      {task.agent_question_text && (
                        <span title={`Agent asked: ${task.agent_question_text}`} className="shrink-0 text-amber-500">
                          <MessageCircleQuestion size={14} />
                        </span>
                      )}
                      <span className="truncate">
                        {task.task_description || <span className="text-gray-300">–</span>}
                      </span>
                    </span>
                  </td>
                  <td className="px-4 py-3"><StatusBadge task={task} /></td>
                  <td className="px-4 py-3 text-gray-600 font-mono text-xs">{task.model_name || '–'}</td>
                  <td className="px-4 py-3"><ProviderBadge provider={task.provider} /></td>
                  <td className="px-4 py-3 text-right text-gray-700">{(task.prompt_tokens ?? 0).toLocaleString()}</td>
                  <td className="px-4 py-3 text-right text-gray-500">
                    {(task.cache_read_tokens ?? 0).toLocaleString()}/{(task.cache_creation_tokens ?? 0).toLocaleString()}
                  </td>
                  <td className="px-4 py-3 text-right text-gray-700">{(task.completion_tokens ?? 0).toLocaleString()}</td>
                  <td className="px-4 py-3 text-right text-gray-700">{task.tool_calls_count ?? 0}</td>
                  <td
                    className="px-4 py-3 text-right text-gray-500"
                    title={task.ttfb_ms !== null ? `${task.ttfb_ms}ms to first byte` : undefined}
                  >
                    {formatDuration(task)}
                  </td>
                  <td className="px-4 py-3 text-right font-semibold text-gray-800">{formatCost(task.cost_estimate)}</td>
                </tr>
              ))}
            </tbody>
          </table>
          {visibleTasks.length === 0 && (
            <p className="text-center text-gray-400 py-10 text-sm">
              {loading ? 'Loading...' : 'No traffic captured yet. Point an agent at this proxy and run a task.'}
            </p>
          )}
          {visibleTasks.length > 0 && (
            <div className="flex items-center justify-between px-4 py-3 border-t border-gray-100 text-sm text-gray-600">
              <button
                onClick={() => setPage((p) => Math.max(0, p - 1))}
                disabled={safePage === 0}
                className="px-3 py-1 rounded-lg border border-gray-200 disabled:opacity-50"
              >
                Prev
              </button>
              <span>Page {safePage + 1} of {pageCount}</span>
              <button
                onClick={() => setPage((p) => Math.min(pageCount - 1, p + 1))}
                disabled={safePage >= pageCount - 1}
                className="px-3 py-1 rounded-lg border border-gray-200 disabled:opacity-50"
              >
                Next
              </button>
            </div>
          )}
        </div>
      </div>

      {selectedId !== null && (
        <div className="surface p-6 space-y-4">
          <div className="flex justify-between items-start gap-4">
            <div>
              <h3 className="text-lg font-semibold text-gray-700">
                Task #{selectedId} raw traffic
              </h3>
              {traffic?.task_description && (
                <p className="text-sm text-gray-500 mt-1">{traffic.task_description}</p>
              )}
            </div>
            <button onClick={() => setSelectedId(null)} className="text-gray-400 hover:text-gray-600 shrink-0">
              <X size={20} />
            </button>
          </div>
          {traffic?.agent_question_text && (
            <div className="flex items-start gap-2 rounded-lg bg-amber-50 border border-amber-200 px-3 py-2">
              <MessageCircleQuestion size={16} className="text-amber-500 shrink-0 mt-0.5" />
              <div>
                <p className="text-xs font-semibold text-amber-700 uppercase">
                  Agent asked{traffic.agent_question_tool ? ` (${traffic.agent_question_tool})` : ''}
                </p>
                <p className="text-sm text-amber-900">{traffic.agent_question_text}</p>
              </div>
            </div>
          )}
          {trafficLoading ? (
            <div className="h-32 flex items-center justify-center">
              <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-blue-600"></div>
            </div>
          ) : (
            <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
              <div>
                <p className="text-xs uppercase font-semibold text-gray-400 mb-2">Request</p>
                <div className="bg-gray-900 text-gray-100 text-xs rounded-lg p-4 overflow-auto max-h-96 whitespace-pre-wrap break-words">
                  <CollapsibleJson raw={traffic?.request_body ?? null} />
                </div>
              </div>
              <div>
                <p className="text-xs uppercase font-semibold text-gray-400 mb-2">Response</p>
                <div className="bg-gray-900 text-gray-100 text-xs rounded-lg p-4 overflow-auto max-h-96 whitespace-pre-wrap break-words">
                  <CollapsibleJson raw={traffic?.response_body ?? null} />
                </div>
              </div>
            </div>
          )}
        </div>
      )}
    </div>
  );
};

export default TrafficView;
