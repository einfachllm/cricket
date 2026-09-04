import React, { useState } from "react"
import { BrowserRouter, Routes, Route, NavLink } from "react-router-dom"
import { Settings, BarChart3, Radio, Bot, FlaskConical, Minus, Square, X, Pin, PinOff } from "lucide-react"
import AnalyticsDashboard from "./components/AnalyticsDashboard"
import AgentStatusView from "./components/AgentStatusView"
import TrafficView from "./components/TrafficView"
import ProviderSettings from "./components/ProviderSettings"
import { SessionsProvider, useAttentionCount, useSessions } from "./hooks/useSessions"
import {
  closeWindow,
  minimizeWindow,
  setAlwaysOnTop,
  toggleMaximizeWindow,
  windowControlsAvailable,
} from "./lib/windowChrome"

const NAV_ITEMS = [
  { to: "/", label: "Agents", icon: Bot },
  { to: "/traffic", label: "Runs", icon: Radio },
  { to: "/analytics", label: "Compare", icon: BarChart3 },
  { to: "/settings", label: "Settings", icon: Settings },
] as const

function BackendPill() {
  const { error, loaded } = useSessions()
  const unreachable = error !== null
  const dot = unreachable ? "bg-red-500" : loaded ? "bg-emerald-500" : "bg-slate-500"
  const label = unreachable ? "Backend unreachable" : loaded ? "Backend ready" : "Backend…"
  return (
    <div className="flex min-w-0 items-center gap-1.5 rounded-full border border-white/10 bg-white/[0.04] px-2 py-0.5 text-[10px] font-medium text-slate-400">
      <span className={`h-1.5 w-1.5 shrink-0 rounded-full ${dot}`} />
      <span className="truncate">{label}</span>
    </div>
  )
}

function WindowButton({
  label,
  onClick,
  extraHover = "",
  children,
}: {
  label: string
  onClick: () => void
  extraHover?: string
  children: React.ReactNode
}) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      onClick={onClick}
      className={`grid h-7 w-7 place-items-center rounded-lg text-slate-500 transition-colors hover:bg-white/10 hover:text-slate-200 ${extraHover}`}
    >
      {children}
    </button>
  )
}

/// The frameless window has no OS chrome, so the header carries it. Rendered
/// only under Tauri — in a plain browser there is nothing to control.
function WindowControls() {
  if (!windowControlsAvailable()) return null
  return (
    <div className="flex items-center">
      <WindowButton label="Minimize window" onClick={() => void minimizeWindow()}>
        <Minus size={14} />
      </WindowButton>
      <WindowButton label="Maximize window" onClick={() => void toggleMaximizeWindow()}>
        <Square size={11} />
      </WindowButton>
      <WindowButton
        label="Close window"
        onClick={() => void closeWindow()}
        extraHover="hover:bg-red-500/80 hover:text-white"
      >
        <X size={14} />
      </WindowButton>
    </div>
  )
}

/// Sidecar pin, matching the always-on-top default the window is created
/// with. Off means other windows may cover it again.
function PinToggle() {
  const [pinned, setPinned] = useState(true)
  if (!windowControlsAvailable()) return null
  const toggle = async () => {
    if (await setAlwaysOnTop(!pinned)) setPinned(!pinned)
  }
  return (
    <button
      type="button"
      aria-pressed={pinned}
      aria-label={pinned ? "Unpin window" : "Pin window"}
      title={pinned ? "Unpin — other windows can cover Harnesswurm" : "Pin — keep Harnesswurm above other windows"}
      onClick={() => void toggle()}
      className={`grid h-8 w-8 place-items-center rounded-lg transition-colors ${
        pinned ? "bg-white/[0.09] text-indigo-300" : "text-slate-500 hover:bg-white/[0.05] hover:text-slate-300"
      }`}
    >
      {pinned ? <Pin size={13} /> : <PinOff size={13} />}
    </button>
  )
}

function SidecarNav() {
  const attention = useAttentionCount()
  return (
    <nav
      aria-label="Sidecar navigation"
      className="flex shrink-0 items-stretch gap-1 border-b border-white/[0.07] bg-[#11131a] px-2 pb-1.5 pt-1"
    >
      {NAV_ITEMS.map(({ to, label, icon: Icon }) => (
        <NavLink
          key={to}
          to={to}
          end={to === "/"}
          className={({ isActive }) =>
            `relative flex min-w-0 max-w-[96px] flex-1 flex-col items-center gap-0.5 rounded-lg px-1 py-1.5 text-[10px] font-medium transition-colors ${
              isActive ? "bg-white/[0.09] text-white" : "text-slate-500 hover:bg-white/[0.04] hover:text-slate-300"
            }`
          }
        >
          {({ isActive }) => (
            <>
              <Icon size={16} className={isActive ? "text-indigo-400" : ""} />
              {label}
              {to === "/" && attention > 0 && (
                <span className="absolute right-1.5 top-1 h-1.5 w-1.5 rounded-full bg-amber-400" />
              )}
            </>
          )}
        </NavLink>
      ))}
      <div className="flex items-center pl-0.5">
        <PinToggle />
      </div>
    </nav>
  )
}

function Layout({ children }: { children: React.ReactNode }) {
  const attention = useAttentionCount()

  return (
    <div className="flex h-screen flex-col overflow-hidden bg-[#0b0e14] text-slate-200">
      <header
        data-tauri-drag-region
        className="flex h-11 shrink-0 select-none items-center justify-between gap-2 border-b border-white/[0.07] bg-[#11131a] py-1 pl-3 pr-1.5"
      >
        <div data-tauri-drag-region className="flex min-w-0 items-center gap-2">
          <div className="grid h-6 w-6 shrink-0 place-items-center rounded-lg bg-indigo-500 text-white shadow-lg shadow-indigo-950/30">
            <FlaskConical size={13} strokeWidth={2.2} />
          </div>
          <span
            data-tauri-drag-region
            className="hidden truncate text-[13px] font-semibold tracking-tight text-white min-[350px]:block"
          >
            Harnesswurm
          </span>
          <BackendPill />
        </div>
        <div className="flex shrink-0 items-center gap-1.5">
          {attention > 0 && (
            <span
              title={`${attention} agent session${attention === 1 ? "" : "s"} need${attention === 1 ? "s" : ""} you`}
              className="grid h-5 min-w-5 place-items-center rounded-full bg-amber-400 px-1 text-[10px] font-bold text-slate-950"
            >
              {attention}
            </span>
          )}
          <WindowControls />
        </div>
      </header>
      <SidecarNav />
      <main className="min-h-0 flex-1 overflow-y-auto">{children}</main>
    </div>
  )
}

function SettingsView() {
  return (
    <div className="page-wrap">
      <ProviderSettings />
    </div>
  )
}

function App() {
  return (
    <SessionsProvider>
      <BrowserRouter>
        <Layout>
          <Routes>
            <Route path="/" element={<AgentStatusView />} />
            <Route path="/analytics" element={<AnalyticsDashboard />} />
            <Route path="/traffic" element={<TrafficView />} />
            <Route path="/settings" element={<SettingsView />} />
          </Routes>
        </Layout>
      </BrowserRouter>
    </SessionsProvider>
  )
}

export default App
