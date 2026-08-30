import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import { useEffect, useState } from "react";
function App() {
    const [status, setStatus] = useState("Initializing...");
    useEffect(() => {
        // Placeholder for Tauri command call
        setStatus("Ready");
    }, []);
    return (_jsx("div", { className: "min-h-screen bg-gray-100 flex flex-col items-center justify-center p-4", children: _jsxs("div", { className: "bg-white p-8 rounded-lg shadow-md w-full max-w-md", children: [_jsx("h1", { className: "text-2xl font-bold mb-4 text-gray-800", children: "Agent-Turn Telemetry" }), _jsxs("div", { className: "mt-4 p-4 bg-blue-50 text-blue-700 rounded", children: ["Status: ", _jsx("span", { className: "font-mono font-bold", children: status })] }), _jsx("p", { className: "mt-4 text-sm text-gray400", children: "Monitoring telemetry from proxy server..." })] }) }));
}
export default App;
