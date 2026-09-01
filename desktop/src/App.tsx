import React from "react"
import { BrowserRouter, Routes, Route, NavLink, useLocation } from "react-router-dom"
import { Settings, BarChart3, Radio, Bot, FlaskConical } from "lucide-react"
import AnalyticsDashboard from "./components/AnalyticsDashboard"
import AgentStatusView from "./components/AgentStatusView"
import TrafficView from "./components/TrafficView"
import ProviderSettings from "./components/ProviderSettings"
import { SessionsProvider, useAttentionCount } from "./hooks/useSessions"

const NAV_ITEMS = [
  { to: "/", label: "Agents", description: "Live workspace", icon: Bot },
  { to: "/traffic", label: "Runs", description: "Requests & traces", icon: Radio },
  { to: "/analytics", label: "Compare", description: "Experiments", icon: BarChart3 },
] as const

const PAGE_META: Record<string, { eyebrow: string; title: string }> = {
  "/": { eyebrow: "Workspace", title: "Agent activity" },
  "/traffic": { eyebrow: "Observability", title: "Run history" },
  "/analytics": { eyebrow: "Evaluation", title: "Compare experiments" },
  "/settings": { eyebrow: "Workspace", title: "Settings" },
}

function Brand() {
  return (
    <div className="flex items-center gap-3">
      <div className="grid h-9 w-9 place-items-center rounded-xl bg-indigo-500 text-white shadow-lg shadow-indigo-950/30">
        <FlaskConical size={18} strokeWidth={2.2} />
      </div>
      <div>
        <p className="text-[15px] font-semibold tracking-tight text-white">Harnesswurm</p>
        <p className="text-[11px] font-medium text-slate-500">Agent lab</p>
      </div>
    </div>
  )
}

function Layout({ children }: { children: React.ReactNode }) {
  const location = useLocation()
  const attention = useAttentionCount()
  const page = PAGE_META[location.pathname] ?? PAGE_META["/"]

  return (
    <div className="app-shell min-h-screen bg-[#f7f8fb] text-slate-950">
      <aside className="fixed inset-y-0 left-0 z-20 hidden w-[248px] flex-col border-r border-white/5 bg-[#11131a] px-4 py-5 text-white md:flex">
        <div className="px-2"><Brand /></div>

        <div className="mt-9 px-3 text-[10px] font-semibold uppercase tracking-[0.18em] text-slate-600">Monitor</div>
        <nav className="mt-2 flex-1 space-y-1">
          {NAV_ITEMS.map(({ to, label, description, icon: Icon }) => (
            <NavLink
              key={to}
              to={to}
              end={to === "/"}
              className={({ isActive }) => `group flex items-center gap-3 rounded-xl px-3 py-2.5 transition-all ${
                isActive ? "bg-white/[0.09] text-white shadow-sm" : "text-slate-400 hover:bg-white/[0.05] hover:text-slate-200"
              }`}
            >
              {({ isActive }) => <>
                <Icon size={18} className={isActive ? "text-indigo-400" : "text-slate-500 group-hover:text-slate-300"} />
                <span className="min-w-0 flex-1">
                  <span className="block text-sm font-medium">{label}</span>
                  <span className="block text-[11px] text-slate-600">{description}</span>
                </span>
                {to === "/" && attention > 0 && <span className="grid min-w-5 place-items-center rounded-full bg-amber-400 px-1.5 py-0.5 text-[10px] font-bold text-slate-950">{attention}</span>}
              </>}
            </NavLink>
          ))}
        </nav>

        <NavLink to="/settings" className={({ isActive }) => `flex items-center gap-3 rounded-xl px-3 py-2.5 text-sm font-medium transition-colors ${isActive ? "bg-white/[0.09] text-white" : "text-slate-400 hover:bg-white/[0.05] hover:text-white"}`}>
          <Settings size={18} className="text-slate-500" /> Settings
        </NavLink>
        <div className="mt-4 flex items-center gap-2 border-t border-white/[0.06] px-3 pt-4 text-[11px] text-slate-500">
          <span className="h-1.5 w-1.5 rounded-full bg-emerald-400 shadow-[0_0_8px_#34d399]" /> Local workspace
        </div>
      </aside>

      <div className="md:pl-[248px]">
        <header className="sticky top-0 z-10 flex h-[72px] items-center justify-between border-b border-slate-200/70 bg-[#f7f8fb]/90 px-5 backdrop-blur-xl sm:px-8 lg:px-10">
          <div>
            <p className="text-[10px] font-semibold uppercase tracking-[0.16em] text-slate-400">{page.eyebrow}</p>
            <h1 className="mt-0.5 text-lg font-semibold tracking-tight text-slate-900">{page.title}</h1>
          </div>
          <div className="flex items-center gap-3">
            <div className="hidden items-center gap-2 rounded-full border border-slate-200 bg-white px-3 py-1.5 text-xs font-medium text-slate-500 shadow-sm sm:flex">
              <span className="h-1.5 w-1.5 rounded-full bg-emerald-500" /> Backend ready
            </div>
            <div className="grid h-8 w-8 place-items-center rounded-lg bg-indigo-500 text-white md:hidden"><FlaskConical size={15} /></div>
          </div>
        </header>
        <main className="mx-auto max-w-[1500px]">{children}</main>
      </div>
      <nav aria-label="Mobile navigation" className="fixed inset-x-3 bottom-3 z-30 flex items-center justify-around rounded-2xl border border-white/10 bg-[#11131a]/95 p-1.5 shadow-2xl backdrop-blur md:hidden">
        {NAV_ITEMS.map(({ to, label, icon: Icon }) => (
          <NavLink key={to} to={to} end={to === "/"} className={({ isActive }) => `relative flex min-w-[68px] flex-col items-center gap-1 rounded-xl px-3 py-2 text-[10px] font-medium ${isActive ? "bg-white/10 text-white" : "text-slate-500"}`}>
            <Icon size={17} />{label}
            {to === "/" && attention > 0 && <span className="absolute right-2 top-1 h-2 w-2 rounded-full bg-amber-400" />}
          </NavLink>
        ))}
        <NavLink to="/settings" aria-label="Settings" className={({ isActive }) => `grid h-10 w-10 place-items-center rounded-xl ${isActive ? "bg-white/10 text-white" : "text-slate-500"}`}><Settings size={17} /></NavLink>
      </nav>
    </div>
  )
}

function SettingsView() {
  return <div className="page-wrap"><ProviderSettings /></div>
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
