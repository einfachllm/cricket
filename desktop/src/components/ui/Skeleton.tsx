import React from "react";

export function Skeleton({ className = "h-64" }: { className?: string }) {
  return <div className={`animate-pulse rounded-xl bg-slate-200/70 ${className}`} aria-label="Loading" />;
}
export default Skeleton;
