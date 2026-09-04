import React, { useEffect, useState } from 'react';
import { AreaChart, Area, BarChart, Bar, XAxis, YAxis, CartesianGrid, Tooltip, Legend, ResponsiveContainer } from 'recharts';
import { Activity, DollarSign, TrendingUp, Trash2 } from 'lucide-react';
import ExperimentComparison from './ExperimentComparison';
import RunBreakdown from './RunBreakdown';
import { deleteExperiment, fetchJson, formatCost, type MetricPoint, type RunGrouping } from '../lib/api';
import { distinctValues, EMPTY_FILTERS, filterMetrics, type MetricFilters } from './RunFilters';
import Card from './ui/Card';
import Empty from './ui/Empty';
import Skeleton from './ui/Skeleton';

interface Experiment {
  id: number;
  name: string;
  description: string;
}

export function tokensByModel(metrics: MetricPoint[]): { model: string; tokens: number }[] {
  const sums = new Map<string, number>();
  for (const m of metrics) {
    const key = m.model_name ?? "unknown";
    sums.set(key, (sums.get(key) ?? 0) + m.prompt_tokens + m.completion_tokens);
  }
  return [...sums.entries()].map(([model, tokens]) => ({ model, tokens })).sort((a, b) => b.tokens - a.tokens);
}

const AnalyticsDashboard = () => {
  const [experiments, setExperiments] = useState<Experiment[]>([]);
  const [selectedExperiment, setSelectedExperiment] = useState<string | null>(null);
  const [metrics, setMetrics] = useState<MetricPoint[]>([]);
  const [loading, setLoading] = useState(false);
  // Lifted so the comparison and the breakdown always count in the same
  // unit — two cards disagreeing about what a run is would be worse than
  // either default.
  const [grouping, setGrouping] = useState<RunGrouping>('session');
  const [filters, setFilters] = useState<MetricFilters>(EMPTY_FILTERS);
  const filtered = filterMetrics(metrics, filters);

  useEffect(() => {
    fetchExperiments();
  }, []);

  useEffect(() => {
    if (selectedExperiment) {
      fetchMetrics(selectedExperiment);
    }
  }, [selectedExperiment]);

  const fetchExperiments = async () => {
    try {
      setExperiments(await fetchJson<Experiment[]>('/v1/analytics/experiments'));
    } catch (error) {
      console.error('Error fetching experiments:', error);
    }
  };

  const fetchMetrics = async (id: string) => {
    setLoading(true);
    try {
      setMetrics(await fetchJson<MetricPoint[]>(`/v1/analytics/experiments/${id}/metrics`));
    } catch (error) {
      console.error('Error fetching metrics:', error);
    } finally {
      setLoading(false);
    }
  };

  const removeExperiment = async (exp: Experiment) => {
    if (!window.confirm(`Delete experiment "${exp.name}"? Its calls stay but become ungrouped. This cannot be undone.`)) {
      return;
    }
    try {
      await deleteExperiment(exp.id);
      if (selectedExperiment === exp.id.toString()) {
        setSelectedExperiment(null);
        setMetrics([]);
        setFilters(EMPTY_FILTERS);
      }
      await fetchExperiments();
    } catch (error) {
      console.error('Error deleting experiment:', error);
    }
  };

  const getSummary = () => {
    if (filtered.length === 0) return { totalPrompt: 0, totalCompletion: 0, avgLatency: 0, totalCost: 0, pricedCount: 0 };
    const totalPrompt = filtered.reduce((sum, m) => sum + m.prompt_tokens + m.cache_creation_tokens + m.cache_read_tokens, 0);
    const totalCompletion = filtered.reduce((sum, m) => sum + m.completion_tokens, 0);
    const avgLatency = filtered.reduce((sum, m) => sum + m.latency_ms, 0) / filtered.length;
    const priced = filtered.filter((m) => m.cost_estimate !== null);
    const totalCost = priced.reduce((sum, m) => sum + (m.cost_estimate ?? 0), 0);
    return { totalPrompt, totalCompletion, avgLatency, totalCost, pricedCount: priced.length };
  };

  const summary = getSummary();

  return (
    <div className="page-wrap">
      <div className="flex justify-between items-center">
        <h2 className="text-3xl font-bold text-slate-100">Experiment Analytics</h2>
        <div className="flex gap-4">
            <div className="bg-blue-100 text-blue-800 px-4 py-2 rounded-lg flex items-center gap-2">
                <Activity size={20} />
                <span className="font-semibold">Tasks: {filtered.length}</span>
            </div>
            <div className="bg-green-100 text-green-800 px-4 py-2 rounded-lg flex items-center gap-2">
                <TrendingUp size="20" />
                <span className="font-semibold">Avg Latency: {Math.round(summary.avgLatency)}ms</span>
            </div>
        </div>
      </div>

      <div className="flex flex-wrap items-center gap-3">
        <label className="text-sm text-slate-400">Agent
          <select
            value={filters.agent}
            onChange={(e) => setFilters({ ...filters, agent: e.target.value })}
            className="ml-2 rounded bg-slate-800 px-2 py-1 text-sm text-slate-200"
          >
            <option value="all">All</option>
            {distinctValues(metrics, "agent_name").map((v) => (
              <option key={v} value={v}>{v}</option>
            ))}
          </select>
        </label>
        <label className="text-sm text-slate-400">Model
          <select
            value={filters.model}
            onChange={(e) => setFilters({ ...filters, model: e.target.value })}
            className="ml-2 rounded bg-slate-800 px-2 py-1 text-sm text-slate-200"
          >
            <option value="all">All</option>
            {distinctValues(metrics, "model_name").map((v) => (
              <option key={v} value={v}>{v}</option>
            ))}
          </select>
        </label>
        <label className="text-sm text-slate-400">Provider
          <select
            value={filters.provider}
            onChange={(e) => setFilters({ ...filters, provider: e.target.value })}
            className="ml-2 rounded bg-slate-800 px-2 py-1 text-sm text-slate-200"
          >
            <option value="all">All</option>
            {distinctValues(metrics, "provider").map((v) => (
              <option key={v} value={v}>{v}</option>
            ))}
          </select>
        </label>
        <label className="text-sm text-slate-400">From
          <input
            type="date"
            value={filters.from}
            onChange={(e) => setFilters({ ...filters, from: e.target.value })}
            className="ml-2 rounded bg-slate-800 px-2 py-1 text-sm text-slate-200"
          />
        </label>
        <label className="text-sm text-slate-400">To
          <input
            type="date"
            value={filters.to}
            onChange={(e) => setFilters({ ...filters, to: e.target.value })}
            className="ml-2 rounded bg-slate-800 px-2 py-1 text-sm text-slate-200"
          />
        </label>
        <button
          onClick={() => setFilters(EMPTY_FILTERS)}
          className="rounded bg-slate-800 px-3 py-1 text-sm text-slate-200 hover:bg-slate-700"
        >
          Clear
        </button>
      </div>

      <div className="grid grid-cols-1 md:grid-cols-4 gap-6">
        <Card>
          <p className="text-sm text-gray-500 uppercase font-semibold">Total Prompt Tokens</p>
          <p className="text-2xl font-bold text-slate-100">{summary.totalPrompt.toLocaleString()}</p>
        </Card>
        <Card>
          <p className="text-sm text-gray-500 uppercase font-semibold">Total Completion Tokens</p>
          <p className="text-2xl font-bold text-slate-100">{summary.totalCompletion.toLocaleString()}</p>
        </Card>
        <Card>
          <p className="text-sm text-gray-500 uppercase font-semibold flex items-center gap-1">
            <DollarSign size={14} /> Total Cost
          </p>
          <p className="text-2xl font-bold text-slate-100">
            {formatCost(summary.totalCost)}
          </p>
          {summary.pricedCount < filtered.length && (
            <p className="text-xs text-gray-400 mt-1">
              {filtered.length - summary.pricedCount} task(s) on an unpriced model, excluded
            </p>
          )}
        </Card>
        <Card>
          <p className="text-sm text-gray-500 uppercase font-semibold">Total Requests</p>
          <p className="text-2xl font-bold text-slate-100">{filtered.length}</p>
        </Card>
      </div>

      <div className="grid grid-cols-1 lg:grid-cols-4 gap-6">
        <Card className="lg:col-span-1 p-4">
          <h3 className="text-lg font-semibold mb-4 text-gray-700">Experiments</h3>
          <div className="space-y-2">
            {experiments.map((exp) => (
              <div
                key={exp.id}
                className={`flex items-center rounded-lg transition-colors ${
                  selectedExperiment === exp.id.toString()
                    ? 'bg-blue-600 text-white shadow-md'
                    : 'bg-gray-50 text-slate-500 hover:bg-gray-100'
                }`}
              >
                <button
                  onClick={() => setSelectedExperiment(exp.id.toString())}
                  className="flex-1 min-w-0 text-left px-4 py-3"
                >
                  <p className="font-medium truncate">{exp.name}</p>
                  <p className="text-xs opacity-70 truncate">{exp.description || 'No description'}</p>
                </button>
                <button
                  onClick={() => removeExperiment(exp)}
                  title="Delete this experiment (its calls stay, ungrouped)"
                  aria-label={`Delete experiment ${exp.name}`}
                  className="mr-2 p-1.5 rounded-full text-gray-400 hover:text-red-600 hover:bg-white/60 transition-colors"
                >
                  <Trash2 size={14} />
                </button>
              </div>
            ))}
            {experiments.length === 0 && <p className="text-gray-400 text-sm">No experiments found.</p>}
          </div>
        </Card>

        <div className="lg:col-span-3 space-y-6">
          {selectedExperiment && (
            <ExperimentComparison
              experimentId={selectedExperiment}
              grouping={grouping}
              onGroupingChange={setGrouping}
            />
          )}
          {selectedExperiment && <RunBreakdown experimentId={selectedExperiment} grouping={grouping} />}

          <Card>
          {loading ? (
            <Skeleton className="h-64" />
          ) : metrics.length > 0 ? (
            <div className="h-96 w-full">
              <h3 className="text-lg font-semibold mb-4 text-gray-700">Token Usage Over Time</h3>
              <ResponsiveContainer width="100%" height="100%">
                <AreaChart data={filtered}>
                  <CartesianGrid strokeDasharray="3 3" vertical={false} />
                  <XAxis
                    dataKey="timestamp"
                    tickFormatter={(tick) => new Date(tick).toLocaleTimeString()}
                    minTickGap={30}
                  />
                  <YAxis />
                  <Tooltip formatter={(value) => Number(value).toLocaleString()} />
                  <Legend />
                  <Area type="monotone" dataKey="prompt_tokens" stroke="#3b82f6" fill="#3b82f6" fillOpacity={0.3} strokeWidth={2} dot={false} />
                  <Area type="monotone" dataKey="completion_tokens" stroke="#10b981" fill="#10b981" fillOpacity={0.3} strokeWidth={2} dot={false} />
                  <Area type="monotone" dataKey="cache_read_tokens" stroke="#a855f7" fill="#a855f7" fillOpacity={0.3} strokeWidth={2} dot={false} />
                </AreaChart>
              </ResponsiveContainer>
            </div>
          ) : (
            <Empty title="Select an experiment" hint="Select an experiment to view detailed metrics" />
          )}
          </Card>
          {!loading && metrics.length > 0 && (
            <Card>
              <div className="h-96 w-full">
                <h3 className="text-lg font-semibold mb-4 text-gray-700">Tokens by Model</h3>
                <ResponsiveContainer width="100%" height="100%">
                  <BarChart data={tokensByModel(filtered)} layout="vertical">
                    <CartesianGrid strokeDasharray="3 3" horizontal={false} />
                    <XAxis type="number" />
                    <YAxis type="category" dataKey="model" width={120} />
                    <Tooltip formatter={(value) => Number(value).toLocaleString()} />
                    <Legend />
                    <Bar dataKey="tokens" fill="#3b82f6" />
                  </BarChart>
                </ResponsiveContainer>
              </div>
            </Card>
          )}
        </div>
      </div>
    </div>
  );
};

export default AnalyticsDashboard;
