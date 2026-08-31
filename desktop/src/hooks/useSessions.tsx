import React, { createContext, useCallback, useContext, useEffect, useRef, useState } from 'react';
import { API_BASE, ProviderLimits, SessionSummary, fetchJson } from '../lib/api';

/// Live session state, shared by every view that needs it.
///
/// Two independent refresh triggers, because either one alone is wrong:
///
/// - The server's SSE feed pings on every call start and finish, so a state
///   change appears immediately rather than up to a poll-interval late. That
///   matters here specifically — the whole point is answering "is it stuck or
///   is it thinking?" *now*.
/// - A slow interval also runs regardless, because plenty of state changes
///   with no event at all: a "Thinking for 20s" turning into "Thinking for
///   9m" is exactly the situation worth noticing, and nothing gets pushed
///   while it happens. It also covers the backend being restarted underneath
///   a connected UI.
const POLL_INTERVAL_MS = 5000;

/// Collapse bursts of events (a fan-out of parallel tool calls finishing
/// together) into a single refetch.
const REFRESH_DEBOUNCE_MS = 150;

interface SessionsContextValue {
  sessions: SessionSummary[];
  limits: ProviderLimits[];
  /// Null until the first fetch resolves, so "no agents yet" can be told
  /// apart from "haven't looked yet".
  error: string | null;
  loaded: boolean;
  live: boolean;
  refresh: () => void;
}

const SessionsContext = createContext<SessionsContextValue | null>(null);

export function SessionsProvider({ children }: { children: React.ReactNode }) {
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [limits, setLimits] = useState<ProviderLimits[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [live, setLive] = useState(false);
  const debounce = useRef<ReturnType<typeof setTimeout> | null>(null);

  const refresh = useCallback(async () => {
    try {
      const [nextSessions, nextLimits] = await Promise.all([
        fetchJson<SessionSummary[]>('/v1/analytics/sessions'),
        fetchJson<ProviderLimits[]>('/v1/analytics/limits'),
      ]);
      setSessions(nextSessions);
      setLimits(nextLimits);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Could not reach the Harnesswurm backend');
    } finally {
      setLoaded(true);
    }
  }, []);

  useEffect(() => {
    refresh();
    const timer = setInterval(refresh, POLL_INTERVAL_MS);
    return () => clearInterval(timer);
  }, [refresh]);

  useEffect(() => {
    // Absent under jsdom in tests, and in any browser without SSE support;
    // polling above already keeps the view correct without it.
    if (typeof EventSource === 'undefined') return;

    const source = new EventSource(`${API_BASE}/v1/analytics/events`);
    source.onopen = () => setLive(true);
    source.onerror = () => setLive(false);
    source.onmessage = () => {
      if (debounce.current) clearTimeout(debounce.current);
      debounce.current = setTimeout(refresh, REFRESH_DEBOUNCE_MS);
    };

    return () => {
      if (debounce.current) clearTimeout(debounce.current);
      source.close();
    };
  }, [refresh]);

  return (
    <SessionsContext.Provider value={{ sessions, limits, error, loaded, live, refresh }}>
      {children}
    </SessionsContext.Provider>
  );
}

export function useSessions(): SessionsContextValue {
  const context = useContext(SessionsContext);
  if (!context) {
    throw new Error('useSessions must be used inside a SessionsProvider');
  }
  return context;
}

/// How many sessions want something from the human. Drives the sidebar badge,
/// which is the whole reason the dashboard doesn't need to be watched.
export function useAttentionCount(): number {
  return useSessions().sessions.filter((s) => s.needs_attention).length;
}
