import React, { useEffect, useState } from 'react';
import { LineChart, Line, XAxis, YAxis, CartesianGrid, Tooltip, Legend, ResponsiveContainer, BarChart, Bar } from 'recharts';
import { Activity, Database, TrendingUp, AlertCircle } from 'lucide-react';

interface MetricData {
  timestamp: string;
  prompt_tokens: number;
  completion_tokens: number;
  tool_calls_count: number;
  latency_ms: number;
}

interface Experiment {
  id: number;
  name: string;
  description: string;
}

const AnalyticsDashboard = () => {
  const [experiments, setExperiments] = useState<Experiment[]>([]);
  const [selectedExperiment, setSelectedExperiment] = useState<string | null>(null);
  const [metrics, setMetrics] = useState<MetricData[]>([]);
  const [loading, setLoading] = useState(false);

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
      const response = await fetch('http://localhost:8081/v1/analytics/experiments');
      const data = await response.json();
      setExperiments(data);
    } catch (error) {
      console.error('Error fetching experiments:', error);
    }
  };

  const fetchMetrics = async (id: string) => {
    setLoading(true);
    try {
      const response = await fetch(`http://localhost:8081/v1/analytics/experiments/${id}/metrics`);
      const data = await response.json();
      setMetrics(data);
    } catch (error) {
      console.error('Error fetching metrics:', error);
    } finally {
      setLoading(false);
    }
  };

  const getSummary = () => {
    if (metrics.length === 0) return { totalPrompt: 0, totalCompletion: 0, avgLatency: 0 };
    const totalPrompt = metrics.reduce((sum, m) => sum + m.prompt_tokens, 0);
    const totalCompletion = metrics.reduce((sum, m) => sum + m.completion_tokens, 0);
    const avgLatency = metrics.reduce((sum, m) => sum + m.latency_ms, 0) / metrics.length;
    return { totalPrompt, totalCompletion, avgLatency };
  };

  const summary = getSummary();

  return (
    <div className="p-6 space-y-6">
      <div className="flex justify-between items-center">
        <h2 className="text-3xl font-bold text-gray-800">Experiment Analytics</h2>
        <div className="flex gap-4">
            <div className="bg-blue-100 text-blue-800 px-4 py-2 rounded-lg flex items-center gap-2">
                <Activity size={20} />
                <span className="font-semibold">Tasks: {metrics.length}</span>
            </div>
            <div className="bg-green-100 text-green-800 px-4 py-2 rounded-lg flex items-center gap-2">
                <TrendingUp size="20" />
                <span className="font-semibold">Avg Latency: {Math.round(summary.avgLatency)}ms</span>
            </div>
        </div>
      </div>

      <div className="grid grid-cols-1 md:grid-cols-3 gap-6">
        <div className="bg-white p-6 rounded-xl shadow-sm border border-gray-100">
          <p className="text-sm text-gray-500 uppercase font-semibold">Total Prompt Tokens</p>
          <p className="text-2xl font-bold text-gray-800">{summary.totalPrompt.toLocaleString()}</p>
        </div>
        <div className="bg-white p-6 rounded-xl shadow-sm border border-gray-100">
          <p className="text-sm text-gray-500 uppercase font-semibold">Total Completion Tokens</p>
          <p className="text-2xl font-bold text-gray-800">{summary.totalCompletion.toLocaleString()}</p>
        </div>
        <div className="bg-white p-6 rounded-xl shadow-sm border border-gray-100">
          <p className="text-sm text-gray-500 uppercase font-semibold">Total Requests</p>
          <p className="text-2xl font-bold text-gray-800">{metrics.length}</p>
        </div>
      </div>

      <div className="grid grid-cols-1 lg:grid-cols-4 gap-6">
        <div className="lg:col-span-1 bg-white p-4 rounded-xl shadow-sm border border-gray-100">
          <h3 className="text-lg font-semibold mb-4 text-gray-700">Experiments</h3>
          <div className="space-y-2">
            {experiments.map((exp) => (
              <button
                key={exp.id}
                onClick={() => setSelectedExperiment(exp.id.toString())}
                className={`w-full text-left px-4 py-3 rounded-lg transition-colors ${
                  selectedExperiment === exp.id.toString()
                    ? 'bg-blue-600 text-white shadow-md'
                    : 'bg-gray-50 text-gray/70 hover:bg-gray-100'
                }`}
              >
                <p className="font-medium truncate">{exp.name}</p>
                <p className="text-xs opacity-70 truncate">{exp.description || 'No description'}</p>
              </button>
            ))}
            {experiments.length === 0 && <p className="text-gray-400 text-sm">No experiments found.</p>}
          </div>
        </div>

        <div className="lg:col-span-3 bg-white p-6 rounded-xl shadow-sm border border-gray-100">
          {loading ? (
            <div className="h-64 flex items-center justify-center">
              <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-blue-600"></div>
            </div>
          ) : metrics.length > 0 ? (
            <div className="h-96 w-full">
              <h3 className="text-lg font-semibold mb-4 text-gray-700">Token Usage Over Time</h3>
              <ResponsiveContainer width="100%" height="100%">
                <LineChart data={metrics}>
                  <CartesianGrid strokeDasharray="3 3" vertical={false} />
                  <XAxis 
                    dataKey="timestamp" 
                    tickFormatter={(tick) => new Date(tick).toLocaleTimeString()}
                    minTickGap={30}
                  />
                  <YAxis />
                  <Tooltip />
                  <Legend />
                  <Line type="monotone" dataKey="prompt_tokens" stroke="#3b82f6" strokeWidth={2} dot={false} />
                  <Line type="monotone" dataKey="completion_tokens" stroke="#10b981" strokeWidth={2} dot={false} />
                </LineChart>
              </ResponsiveContainer>
            </div>
          ) : (
            <div className="h-64 flex flex-col items-center justify-center text-gray-400">
              <AlertCircle size={48} className="mb-4 opacity-20" />
              <p>Select an experiment to view detailed metrics</p>
            </div>
          )}
        </div>
      </div>
    </div>
  );
};

export default AnalyticsDashboard;
