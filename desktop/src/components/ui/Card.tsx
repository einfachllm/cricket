import React from "react";

export function Card({ children, className = "" }: { children: React.ReactNode; className?: string }) {
  return <div className={`surface p-6 ${className}`}>{children}</div>;
}
export default Card;
