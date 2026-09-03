# Harnesswurm Dark Dev-Dashboard — Phased Design (Sections 1, 2, 4)

Date: 2026-09-03 | Branch: `feature/agent-integration` | Scope: user-approved phases 1→2→4 (defer analytics drill-down §3, error/testing hardening §5)

## Context
Harnesswurm is a local LLM-traffic proxy (Rust Axum + SQLite, embedded in Tauri) comparing coding agents on the same task. Frontend: React 18 + Vite + Tailwind v3 + recharts, routes `/` Agents, `/traffic` Runs, `/analytics` Compare, `/settings` Providers. Pain: style drift (Traffic/Analytics use old `p-6/text-3xl/bg-gray-50`, others use `surface/rounded-2xl`), hardcoded `http://localhost:8081` in Dashboard/Traffic bypassing `lib/api.ts`, unbounded traffic table with `<pre>` JSON, static "Backend ready" pill, thin tests. Backend attribution precedence `/r/…` > headers > fingerprints; `fingerprints.yaml` seeds only `claude-code`; `pricing.yaml` starter set unverified.

User votes: design polish first, dark dev-dashboard (Grafana/Vercel-style), drill-down deferred, zero-setup for opencode + Claude Code.

## Phase 1 — Section 1: Architecture / Foundation (do first)
Goal: single design system + single fetch path, additive-only backend.
- Frontend: add `desktop/src/theme/tokens.ts` (bg `#0b0e14`, card `#151a23`, sidebar `#11131a`, accent indigo/cyan, text slate) + `desktop/src/components/ui/` primitives (Card, Badge, Table, Empty, Skeleton). Migrate `index.css` helpers (`.page-wrap`, `.surface`) to tokens; no global restyle yet.
- Data: `lib/api.ts` becomes sole fetch path. Remove raw `fetch("http://localhost:8081…")` in `AnalyticsDashboard.tsx` and `TrafficView.tsx`; route via `fetchJson`/`API_BASE` so Tauri/prod ports work. Keep `useSessions.tsx` SSE (`/v1/analytics/events`, 150ms debounce) + 5s poll.
- Backend (additive only): `GET /v1/analytics/models` and `GET /v1/analytics/tools/summary` derived from existing `tasks/metrics/task_tools` tables; no migration files (follow `add_column_if_missing` pattern if needed). No breaking API change; `parse_proxy_path`/`proxy_dispatch` untouched.
- Success: `npm run typecheck` + `npm test` pass; no hardcoded host remains (`grep localhost:8081 desktop/src` empty except `api.ts` default); Traffic/Analytics render identically but via shared primitives.

## Phase 2 — Section 2: Components (dark dev-dashboard)
Goal: consistent, modern, user-friendly ops UI.
- Shell: keep dark sidebar `w-[248px] #11131a` + sticky header; content bg `#0b0e14`, cards `#151a23 rounded-2xl`. Unify Traffic/Analytics headings to `text-lg` + `surface` (drop `text-3xl/bg-gray-50`).
- Agents view: reuse `STATE_STYLES`/`sortSessions`; live pill reflects `useSessions.error` (fixes static "Backend ready"); `SummaryStrip` 4 tiles + `QuotaBar` keep layout, new tokens only.
- Traffic: virtualized/searchable/paginated table (12 cols unchanged: Time/Agent/Task/Status/Model/Provider/Input/Cache R-W/Output/Tools/Latency/Cost), `StatusBadge`/`ProviderBadge` from `ui/`; collapsible JSON tree replaces `<pre max-h-96>`; keep AbortController staleness guard; fix timestamp `+ 'Z'` hack with explicit UTC format helper.
- Analytics (visual only in this phase): token area chart + per-model bar + per-tool bars + existing `PhaseColumns`/`ToolBars` with tooltips (`<title>` → custom tooltip) + loading skeletons; `BarChart/Bar` unused import either used or removed.
- Success: visual consistency checklist (sidebar/header/cards/badges/skeletons same tokens); traffic with 1k rows stays responsive; no new backend required.

## Phase 3 — Section 4: Agent connectivity (opencode + Claude Code)
Goal: zero manual setup for the two voted agents.
- Capture live UA + system-prompt substrings from Traffic tab for opencode and Claude Code; add verified entries to `harnesswurm/backend/fingerprints.yaml` + unit tests in `fingerprints.rs` (header-less attribution path). Keep precedence: `/r/…` > `X-Agent-ID` headers > fingerprints.
- Update `harnesswurm/backend/pricing.yaml` for their current models (longest-prefix dated IDs); unpriced calls stay excluded from ranking (never $0-wins).
- Document zero-setup path: `cargo run --bin harnesswurm -- run --agent <name> --experiment <id> -- <cmd>` building `/r/<agent>[/<exp>]/<session>[/p/<prov>]` prefix; add/extend `parse_proxy_path` test for combined `/r/…/p/…`.
- Out of scope: Cursor/Aider/Kilo/Codex fingerprints (phase 2), hot-edit for non-provider yamls (restart-to-apply stays), `/v1/models` + `count_tokens` recording (forwarded-not-recorded by design).
- Success: fresh opencode + Claude Code runs attribute correctly with no header/prefix config; cost shows instead of `≥ $x`; wrapper path tested end-to-end.

## Deferred (explicitly not in this plan)
- §3 Deeper drill-down (run-detail timeline, filters agent/model/provider/date, new rollups) — next spec.
- §5 Error/testing hardening (error panels + retry, verdict/grouping integration tests) — next spec.
- Unmerged `feature/agent-integration` PR to `origin/main` still required before release.

## Verification per phase
- Phase 1: `npm run typecheck`, `npm test`, `grep -rn "localhost:8081" desktop/src` only in `api.ts`.
- Phase 2: manual 1k-row traffic scroll, `npm test`, visual token checklist.
- Phase 3: `cargo test` (fingerprint + proxy path), live opencode + Claude Code runs attributed + priced.

## Self-review
- Placeholders: none — all files/endpoints/tokens named explicitly.
- Consistency: additive backend only, no migration conflict; precedence rule preserved; pricing never $0-wins preserved.
- Scope: single plan in three ordered phases (1→2→4); §3/§5 deferred to avoid bloat.
- Ambiguity: "zero-setup" means no header/prefix hand-config for the two agents via fingerprints + wrapper docs; other agents remain manual.
