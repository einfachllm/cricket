import React, { useEffect, useState } from 'react';
import { Radio, RefreshCw, X, MessageCircleQuestion } from 'lucide-react';

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

const API_BASE = 'http://localhost:8081';

function formatCost(cost: number | null): string {
  if (cost === null || cost === undefined) return '–';
  if (cost === 0) return '$0.00';
  return cost < 0.01 ? `$${cost.toFixed(5)}` : `$${cost.toFixed(4)}`;
}

function formatJson(raw: string | null): string {
  if (!raw) return '(no data captured)';
  try {
    return JSON.stringify(JSON.parse(raw), null, 2);
  } catch {
    return raw;
  }
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
  const [loading, setLoading] = useState(false);
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [traffic, setTraffic] = useState<TaskTraffic | null>(null);
  const [trafficLoading, setTrafficLoading] = useState(false);

  const fetchTasks = async () => {
    setLoading(true);
    try {
      const response = await fetch(`${API_BASE}/v1/analytics/tasks`);
      const data = await response.json();
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
    setTrafficLoading(true);
    fetch(`${API_BASE}/v1/analytics/tasks/${selectedId}/traffic`)
      .then((res) => (res.ok ? res.json() : Promise.reject(res.status)))
      .then((data) => setTraffic(data))
      .catch((error) => console.error('Error fetching traffic:', error))
      .finally(() => setTrafficLoading(false));
  }, [selectedId]);

  const agents = Array.from(new Set(tasks.map((t) => t.agent_name))).sort();
  const visibleTasks = tasks
    .filter((t) => agentFilter === 'all' || t.agent_name === agentFilter)
    .filter((t) => !questionsOnly || !!t.agent_question_text);

  return (
    <div className="p-6 space-y-6">
      <div className="flex justify-between items-center">
        <h2 className="text-3xl font-bold text-gray-800 flex items-center gap-2">
          <Radio size={28} className="text-blue-600" />
          Traffic
        </h2>
        <div className="flex items-center gap-3">
          <select
            value={agentFilter}
            onChange={(e) => setAgentFilter(e.target.value)}
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
              onChange={(e) => setQuestionsOnly(e.target.checked)}
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

      <div className="bg-white rounded-xl shadow-sm border border-gray-100 overflow-hidden">
        <div className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead className="bg-gray-50 text-gray-500 uppercase text-xs">
              <tr>
                <th className="text-left px-4 py-3 font-semibold">Time</th>
                <th className="text-left px-4 py-3 font-semibold">Agent</th>
                <th className="text-left px-4 py-3 font-semibold">Task</th>
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
              {visibleTasks.map((task) => (
                <tr
                  key={task.task_id}
                  onClick={() => setSelectedId(task.task_id)}
                  className={`border-t border-gray-100 cursor-pointer hover:bg-blue-50 transition-colors ${
                    selectedId === task.task_id ? 'bg-blue-50' : ''
                  }`}
                >
                  <td className="px-4 py-3 text-gray-500 whitespace-nowrap">
                    {new Date(task.timestamp + 'Z').toLocaleTimeString()}
                  </td>
                  <td className="px-4 py-3 font-medium text-gray-800">{task.agent_name}</td>
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
                  <td className="px-4 py-3 text-gray-600 font-mono text-xs">{task.model_name || '–'}</td>
                  <td className="px-4 py-3"><ProviderBadge provider={task.provider} /></td>
                  <td className="px-4 py-3 text-right text-gray-700">{(task.prompt_tokens ?? 0).toLocaleString()}</td>
                  <td className="px-4 py-3 text-right text-gray-500">
                    {(task.cache_read_tokens ?? 0).toLocaleString()}/{(task.cache_creation_tokens ?? 0).toLocaleString()}
                  </td>
                  <td className="px-4 py-3 text-right text-gray-700">{(task.completion_tokens ?? 0).toLocaleString()}</td>
                  <td className="px-4 py-3 text-right text-gray-700">{task.tool_calls_count ?? 0}</td>
                  <td className="px-4 py-3 text-right text-gray-500">{task.latency_ms !== null ? `${task.latency_ms}ms` : '–'}</td>
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
        </div>
      </div>

      {selectedId !== null && (
        <div className="bg-white rounded-xl shadow-sm border border-gray-100 p-6 space-y-4">
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
                <pre className="bg-gray-900 text-gray-100 text-xs rounded-lg p-4 overflow-auto max-h-96 whitespace-pre-wrap break-words">
                  {formatJson(traffic?.request_body ?? null)}
                </pre>
              </div>
              <div>
                <p className="text-xs uppercase font-semibold text-gray-400 mb-2">Response</p>
                <pre className="bg-gray-900 text-gray-100 text-xs rounded-lg p-4 overflow-auto max-h-96 whitespace-pre-wrap break-words">
                  {formatJson(traffic?.response_body ?? null)}
                </pre>
              </div>
            </div>
          )}
        </div>
      )}
    </div>
  );
};

export default TrafficView;
