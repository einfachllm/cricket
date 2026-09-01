import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { AlertTriangle, Check, Copy, Plus, RefreshCw, Server, Trash2 } from 'lucide-react';
import {
  ProviderApiStyle,
  ProviderConfig,
  ProviderDraft,
  fetchProviders,
  proxyBaseUrl,
  saveProviders,
} from '../lib/api';

/// A row being edited. The `id` exists only to key the list: names are the
/// identity on the backend, but they change while being typed, so keying on
/// one would remount the input on every keystroke and lose the cursor.
interface Row extends ProviderDraft {
  id: number;
  envOverride: string | null;
}

let nextRowId = 1;

function toRow(provider: ProviderConfig): Row {
  return {
    id: nextRowId++,
    name: provider.name,
    api: provider.api,
    base_url: provider.base_url,
    default: provider.default,
    envOverride: provider.env_override,
  };
}

function toDraft(row: Row): ProviderDraft {
  return { name: row.name, api: row.api, base_url: row.base_url, default: row.default };
}

/// The frontend twin of `ProviderConfig::target_url`, so a row shows where it
/// would forward *while being typed* rather than only after a save. Like the
/// backend, the endpoint goes on the path and any query is kept at the end,
/// so a gateway URL carrying an api-version previews as it will be called.
export function previewTargetUrl(row: { api: ProviderApiStyle; base_url: string }): string {
  const trimmed = row.base_url.trim();
  if (!trimmed) return '';
  const queryAt = trimmed.indexOf('?');
  const query = queryAt === -1 ? '' : trimmed.slice(queryAt);
  const base = (queryAt === -1 ? trimmed : trimmed.slice(0, queryAt)).replace(/\/+$/, '');
  if (!base) return '';
  const path = row.api === 'openai' ? '/chat/completions' : '/v1/messages';
  return (base.endsWith(path) ? base : base + path) + query;
}

function CopyButton({ value }: { value: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <button
      type="button"
      title={`Copy ${value}`}
      aria-label={`Copy ${value}`}
      onClick={async () => {
        try {
          await navigator.clipboard?.writeText(value);
          setCopied(true);
          setTimeout(() => setCopied(false), 1500);
        } catch {
          // A denied clipboard is not worth an error state: the URL is on
          // screen and can be selected by hand.
        }
      }}
      className="text-slate-400 hover:text-slate-700"
    >
      {copied ? <Check size={14} className="text-emerald-600" /> : <Copy size={14} />}
    </button>
  );
}

/// Editor for `providers.yaml` — where each proxied call is forwarded to.
/// The whole list is saved at once, because that is what the file is: a
/// partial save would need a merge rule the file itself doesn't have.
export default function ProviderSettings() {
  const [rows, setRows] = useState<Row[]>([]);
  const [saved, setSaved] = useState<ProviderDraft[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [justSaved, setJustSaved] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const providers = await fetchProviders();
      setRows(providers.map(toRow));
      setSaved(providers.map((p) => ({ name: p.name, api: p.api, base_url: p.base_url, default: p.default })));
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Could not load providers');
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { void load(); }, [load]);

  const drafts = useMemo(() => rows.map(toDraft), [rows]);
  const dirty = useMemo(() => JSON.stringify(drafts) !== JSON.stringify(saved), [drafts, saved]);

  function update(id: number, patch: Partial<Row>) {
    setJustSaved(false);
    setRows((current) => current.map((row) => (row.id === id ? { ...row, ...patch } : row)));
  }

  /// Exactly one default per api style, enforced here rather than only on
  /// save: a radio that can be switched off would let the bare
  /// /v1/chat/completions route silently pick for itself.
  function makeDefault(id: number, api: ProviderApiStyle) {
    setJustSaved(false);
    setRows((current) => current.map((row) => (
      row.api === api ? { ...row, default: row.id === id } : row
    )));
  }

  function addRow() {
    setJustSaved(false);
    setRows((current) => [...current, {
      id: nextRowId++,
      name: '',
      api: 'openai',
      base_url: 'http://localhost:11434/v1',
      default: false,
      envOverride: null,
    }]);
  }

  function removeRow(id: number) {
    setJustSaved(false);
    setRows((current) => current.filter((row) => row.id !== id));
  }

  async function save() {
    setBusy(true);
    try {
      const providers = await saveProviders(drafts);
      setRows(providers.map(toRow));
      setSaved(providers.map((p) => ({ name: p.name, api: p.api, base_url: p.base_url, default: p.default })));
      setError(null);
      setJustSaved(true);
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Saving providers failed');
      setJustSaved(false);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="space-y-4">
      <div className="flex items-start justify-between gap-4">
        <div>
          <h2 className="flex items-center gap-2 text-xl font-semibold tracking-tight text-slate-900">
            <Server size={18} /> Providers
          </h2>
          <p className="mt-1 max-w-3xl text-sm text-slate-500">
            Where each proxied call is forwarded to. Point an agent at the base URL of a
            provider below and its traffic is captured on the way through — a local model
            server (Ollama, vLLM, LM Studio, llama.cpp) is just another entry. Saved edits
            apply to the next call; running calls finish against the provider they started on.
          </p>
        </div>
        <button
          onClick={() => void load()}
          className="flex shrink-0 items-center gap-2 rounded-xl border border-slate-200 bg-white px-3 py-2 text-sm text-slate-700 hover:bg-slate-50"
        >
          <RefreshCw size={16} className={loading ? 'animate-spin' : ''} />
          Reload
        </button>
      </div>

      {error && (
        <div role="alert" className="flex items-start gap-2 rounded-xl border border-red-200 bg-red-50 p-3 text-sm text-red-800">
          <AlertTriangle size={16} className="mt-0.5 shrink-0" />
          <span>{error}</span>
        </div>
      )}

      {justSaved && !dirty && (
        <div className="flex items-center gap-2 rounded-xl border border-emerald-200 bg-emerald-50 p-3 text-sm text-emerald-800">
          <Check size={16} /> Saved to providers.yaml — new calls use it immediately.
        </div>
      )}

      <div className="surface overflow-hidden">
        <div className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead className="bg-slate-50 text-xs uppercase text-slate-500">
              <tr>
                <th className="text-left px-4 py-3 font-semibold">Name</th>
                <th className="text-left px-4 py-3 font-semibold">API</th>
                <th className="text-left px-4 py-3 font-semibold">Base URL</th>
                <th className="text-left px-4 py-3 font-semibold">Default</th>
                <th className="text-left px-4 py-3 font-semibold">Point agents at</th>
                <th className="px-4 py-3" />
              </tr>
            </thead>
            <tbody>
              {rows.map((row) => (
                <tr key={row.id} className="border-t border-slate-100 align-top">
                  <td className="px-4 py-3">
                    <input
                      aria-label="Provider name"
                      value={row.name}
                      onChange={(e) => update(row.id, { name: e.target.value })}
                      placeholder="ollama"
                      className="w-32 rounded-lg border border-slate-200 px-2 py-1.5 font-mono text-xs"
                    />
                  </td>
                  <td className="px-4 py-3">
                    <select
                      aria-label={`API format for ${row.name || 'new provider'}`}
                      value={row.api}
                      onChange={(e) => update(row.id, { api: e.target.value as ProviderApiStyle, default: false })}
                      className="rounded-lg border border-slate-200 bg-white px-2 py-1.5 text-xs"
                    >
                      <option value="openai">openai</option>
                      <option value="anthropic">anthropic</option>
                    </select>
                  </td>
                  <td className="px-4 py-3">
                    <input
                      aria-label={`Base URL for ${row.name || 'new provider'}`}
                      value={row.base_url}
                      onChange={(e) => update(row.id, { base_url: e.target.value })}
                      placeholder="http://localhost:11434/v1"
                      className="w-72 rounded-lg border border-slate-200 px-2 py-1.5 font-mono text-xs"
                    />
                    <div className="mt-1 max-w-[22rem] truncate font-mono text-xs text-slate-400" title={previewTargetUrl(row)}>
                      → {previewTargetUrl(row) || '–'}
                    </div>
                    {row.envOverride && (
                      <div className="mt-1 text-xs text-amber-700">
                        Overridden by <span className="font-mono">{row.envOverride}</span> until that
                        variable is unset — calls go there, not to the URL above.
                      </div>
                    )}
                  </td>
                  <td className="px-4 py-3">
                    <label className="inline-flex items-center gap-2 whitespace-nowrap text-xs text-slate-600">
                      <input
                        type="radio"
                        name={`default-${row.api}`}
                        aria-label={`Default for the ${row.api} API`}
                        checked={row.default}
                        onChange={() => makeDefault(row.id, row.api)}
                      />
                      for {row.api}
                    </label>
                  </td>
                  <td className="px-4 py-3">
                    <div className="flex items-center gap-2">
                      <code className="text-xs text-slate-600">{proxyBaseUrl(row)}</code>
                      <CopyButton value={proxyBaseUrl(row)} />
                    </div>
                  </td>
                  <td className="px-4 py-3 text-right">
                    <button
                      onClick={() => removeRow(row.id)}
                      aria-label={`Remove ${row.name || 'provider'}`}
                      className="text-slate-400 hover:text-red-600"
                    >
                      <Trash2 size={16} />
                    </button>
                  </td>
                </tr>
              ))}
              {!loading && rows.length === 0 && (
                <tr>
                  <td colSpan={6} className="px-4 py-8 text-center text-slate-400">
                    No providers configured — add one, or reload to get the defaults back.
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
      </div>

      <div className="flex items-center gap-3">
        <button
          onClick={addRow}
          className="flex items-center gap-2 rounded-xl border border-slate-200 bg-white px-3 py-2 text-sm text-slate-700 hover:bg-slate-50"
        >
          <Plus size={16} /> Add provider
        </button>
        <button
          onClick={() => void save()}
          disabled={!dirty || busy}
          className={`rounded-xl px-4 py-2 text-sm font-medium ${
            dirty && !busy ? 'bg-indigo-500 text-white hover:bg-indigo-600' : 'cursor-not-allowed bg-slate-200 text-slate-500'
          }`}
        >
          {busy ? 'Saving…' : 'Save providers'}
        </button>
        {dirty && <span className="text-xs text-slate-500">Unsaved changes</span>}
      </div>

      <p className="max-w-3xl text-xs text-slate-500">
        A provider is also addressable by header — <span className="font-mono">X-Provider: name</span> —
        for clients with a fixed base URL. Calls are recorded under the provider name, so a local run
        shows as itself in Traffic and Analytics; give <span className="font-mono">pricing.yaml</span> an
        entry with a matching <span className="font-mono">provider:</span> to cost it.
      </p>
    </div>
  );
}
