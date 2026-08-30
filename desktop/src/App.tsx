import React, { useEffect, useState } from "react"
import { BrowserRouter, Routes, Route, Link, useLocation } from "react-router-dom"
import { MessageSquare, Settings, BarChart3, Radio } from "lucide-react"
import AnalyticsDashboard from "./components/AnalyticsDashboard"
import TrafficView from "./components/TrafficView"

function Layout({ children }: { children: React.ReactNode }) {
  const location = useLocation();
  const [status, setStatus] = useState("Initializing...")

  useEffect(() => {
    // Placeholder for Tauri command call
    setStatus("Ready")
  }, [])

  return (
    <div className="flex h-screen bg-gray-100 overflow-hidden">
      {/* Sidebar */}
      <div className="w-64 bg-slate-900 text-white flex flex-col">
        <div className="p-6 text-xl font-bold border-b border-slate-800">
          Agent-Turn
        </div>
        <nav className="flex-1 p-4 space-y-2">
          <Link to="/" className={`flex items-center gap-3 px-4 py-3 rounded-lg transition-colors ` + (location.pathname === '/' ? 'bg-blue-600' : 'hover:bg-slate-800')}>
            <MessageSquare size={20} />
            <span>Chat</span>
          </Link>
          <Link to="/analytics" className={`flex items-center gap-3 px-4 py-3 rounded-lg transition-colors ` + (location.pathname === '/analytics' ? 'bg-blue-600' : 'hover:bg-slate-800')}>
            <BarChart3 size={20} />
            <span>Analytics</span>
          </Link>
          <Link to="/traffic" className={`flex items-center gap-3 px-4 py-3 rounded-lg transition-colors ` + (location.pathname === '/traffic' ? 'bg-blue-600' : 'hover:bg-slate-800')}>
            <Radio size={20} />
            <span>Traffic</span>
          </Link>
          <Link to="/settings" className={`flex items-center gap-3 px-4 py-3 rounded-lg transition-colors ` + (location.pathname === '/settings' ? 'bg-blue-600' : 'hover:bg-slate-800')}>
            <Settings size={20} />
            <span>Settings</span>
          </Link>
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
    <BrowserRouter>
      <Layout>
        <Routes>
          <Route path="/" element={<ChatView />} />
          <Route path="/analytics" element={<AnalyticsDashboard />} />
          <Route path="/traffic" element={<TrafficView />} />
          <Route path="/settings" element={<SettingsView />} />
        </Routes>
      </Layout>
    </BrowserRouter>
  )
}

export default App
