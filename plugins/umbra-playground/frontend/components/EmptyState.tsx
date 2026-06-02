import type { ReactNode } from "react";

export function EmptyState({
  title,
  children,
}: {
  title: string;
  children?: ReactNode;
}) {
  return (
    <div className="flex items-center justify-center h-full text-slate-500 text-sm">
      <div className="text-center">
        <p className="font-mono text-xs uppercase tracking-widest mb-2">{title}</p>
        {children}
      </div>
    </div>
  );
}
