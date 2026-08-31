import React, { useEffect, useState } from "react"
import { BrowserRouter, Routes, Route, Link, useLocation } from "react-router-dom"
import { MessageSquare, Settings, BarChart3, Radio, Bot } from "lucide-react"
import AnalyticsDashboard from "./components/AnalyticsDashboard"
import AgentStatusView from "./components/AgentStatusView"
import TrafficView from "./components/TrafficView"
import { SessionsProvider, useAttentionCount } from "./hooks/useSessions"

const NAV_ITEMS = [
  { to: "/", label: "Agents", icon: Bot },
  { to: "/traffic", label: "Traffic", icon: Radio },
  { to: "/analytics", label: "Analytics", icon: BarChart3 },
  { to: "/chat", label: "Chat", icon: MessageSquare },
  { to: "/settings", label: "Settings", icon: Settings },
] as const

function Layout({ children }: { children: React.ReactNode }) {
  const location = useLocation();
  const [status, setStatus] = useState("Initializing...")
  // Surfaced on the nav itself so a waiting or blocked agent is visible from
  // any tab — the dashboard shouldn't have to be the open one to be useful.
  const attention = useAttentionCount()

  useEffect(() => {
    // Placeholder for Tauri command call
    setStatus("Ready")
  }, [])

  return (
    <div className="flex h-screen bg-gray-100 overflow-hidden">
      {/* Sidebar */}
      <div className="w-64 bg-slate-900 text-white flex flex-col">
        <div className="p-6 text-xl font-bold border-b border-slate-800">
          Harnesswurm
        </div>
        <nav className="flex-1 p-4 space-y-2">
          {NAV_ITEMS.map(({ to, label, icon: Icon }) => (
            <Link
              key={to}
              to={to}
              className={`flex items-center gap-3 px-4 py-3 rounded-lg transition-colors ` + (location.pathname === to ? 'bg-blue-600' : 'hover:bg-slate-800')}
            >
              <Icon size={20} />
              <span className="flex-1">{label}</span>
              {to === "/" && attention > 0 && (
                <span
                  className="px-2 py-0.5 rounded-full bg-amber-400 text-slate-900 text-xs font-bold"
                  title={`${attention} agent session(s) need you`}
                >
                  {attention}
                </span>
              )}
            </Link>
          ))}
        </nav>
        <div className="p-4 border-t border-slate-800 text-xs text-slate/50">
          Status: <span className="font-mono">{status}</span>
        </div>
      </div>

      {/* Main Content */}
      <div className="flex-1 flex flex-col overflow-hidden">
        <main className="flex-1 overflow-y-auto">
          {children}
        </main>
      </div>
    </div>
  )
}

function ChatView() {
  return (
    <div className="p-6">
      <h1 className="text-2xl font-bold text-gray-800 mb-4">Chat Interface</h1>
      <div className="bg-white p-8 rounded-xl shadow-sm border border-gray-100 text-center text-gray-500">
        Chat component under development
      </div>
    </div>
  )
}

function SettingsView() {
  return (
    <div className="p-6">
      <h1 className="text-2xl font-bold text-gray-800 mb-4">Settings</h1>
      <div className="bg-white p-8 rounded-xl shadow-sm border border-gray-100 text-center text-gray-500">
        Settings component under development
      </div>
    </div>
  )
}

function App() {
  return (
    // The provider wraps the router so the sidebar's attention badge reads
    // the same live session data as the Agents view, from one subscription.
    <SessionsProvider>
      <BrowserRouter>
        <Layout>
          <Routes>
            {/* Agents is the landing view: "what is everything doing right
                now" is the question the app exists to answer. */}
            <Route path="/" element={<AgentStatusView />} />
            <Route path="/analytics" element={<AnalyticsDashboard />} />
            <Route path="/traffic" element={<TrafficView />} />
            <Route path="/chat" element={<ChatView />} />
            <Route path="/settings" element={<SettingsView />} />
          </Routes>
        </Layout>
      </BrowserRouter>
    </SessionsProvider>
  )
}

export default App
