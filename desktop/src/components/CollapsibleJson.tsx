import React from "react";

function Node({ name, value, depth }: { name: string; value: unknown; depth: number }) {
  if (value === null) return <div><span className="text-slate-400">{name}</span>: <span className="text-slate-500">null</span></div>;
  if (Array.isArray(value)) return (
    <details open={depth < 1}>
      <summary className="cursor-pointer text-slate-300">{name} <span className="text-slate-500">[{value.length}]</span></summary>
      <div className="pl-4">{value.map((v, i) => <Node key={i} name={String(i)} value={v} depth={depth + 1} />)}</div>
    </details>
  );
  if (typeof value === "object") {
    const entries = Object.entries(value as Record<string, unknown>);
    return (
      <details open={depth < 1}>
        <summary className="cursor-pointer text-slate-300">{name} <span className="text-slate-500">{"{...}"}</span></summary>
        <div className="pl-4">{entries.map(([k, v]) => <Node key={k} name={k} value={v} depth={depth + 1} />)}</div>
      </details>
    );
  }
  const str = typeof value === "string" ? value : JSON.stringify(value);
  return <div className="break-words"><span className="text-sky-300">{name}</span>: <span className="text-slate-200">{str.length > 500 ? str.slice(0, 500) + "…" : str}</span></div>;
}

export function CollapsibleJson({ raw }: { raw: string | null }) {
  if (!raw) return <p className="text-sm text-slate-500">(no data captured)</p>;
  let parsed: unknown;
  try { parsed = JSON.parse(raw); } catch { return <pre className="whitespace-pre-wrap break-words text-xs text-slate-200">{raw.slice(0, 4000)}</pre>; }
  if (typeof parsed !== "object" || parsed === null) return <pre className="text-xs text-slate-200">{JSON.stringify(parsed)}</pre>;
  return <div className="font-mono text-xs space-y-0.5">{Object.entries(parsed as Record<string, unknown>).map(([k, v]) => <Node key={k} name={k} value={v} depth={0} />)}</div>;
}
export default CollapsibleJson;
