const COLORS: Record<string, string> = {
  GET: "bg-indigo-500/20 text-indigo-300 border-indigo-500/30",
  POST: "bg-emerald-500/20 text-emerald-300 border-emerald-500/30",
  PUT: "bg-amber-500/20 text-amber-300 border-amber-500/30",
  PATCH: "bg-sky-500/20 text-sky-300 border-sky-500/30",
  DELETE: "bg-rose-500/20 text-rose-300 border-rose-500/30",
};

export function MethodBadge({ method }: { method: string }) {
  const cls = COLORS[method.toUpperCase()] ?? "bg-slate-500/20 text-slate-300 border-slate-500/30";
  return (
    <span
      className={`inline-block px-1.5 py-0.5 rounded text-[10px] font-mono font-semibold border ${cls}`}
    >
      {method.toUpperCase()}
    </span>
  );
}
