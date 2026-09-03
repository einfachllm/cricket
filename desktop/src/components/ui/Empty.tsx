import React from "react";

export function Empty({ title, hint }: { title: string; hint?: string }) {
  return (
    <div className="empty-panel">
      <h2>{title}</h2>
      {hint ? <p>{hint}</p> : null}
    </div>
  );
}
export default Empty;
