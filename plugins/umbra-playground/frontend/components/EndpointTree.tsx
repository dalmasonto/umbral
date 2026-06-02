import { useMemo, useState } from "react";
import { usePlayground } from "../state/store";
import { listOperations, type TreeEntry } from "../state/spec";
import { MethodBadge } from "./MethodBadge";
import { EmptyState } from "./EmptyState";

export function EndpointTree() {
  const spec = usePlayground((s) => s.spec);
  const loadingSpec = usePlayground((s) => s.loadingSpec);
  const selected = usePlayground((s) => s.selectedOperationId);
  const select = usePlayground((s) => s.selectEndpoint);
  const [search, setSearch] = useState("");

  const grouped = useMemo(() => {
    if (!spec) return null;
    const all = listOperations(spec);
    const q = search.toLowerCase();
    const filtered = q
      ? all.filter(
          (e) =>
            e.path.toLowerCase().includes(q) ||
            e.method.toLowerCase().includes(q) ||
            (e.summary?.toLowerCase().includes(q) ?? false),
        )
      : all;
    const byTag = new Map<string, TreeEntry[]>();
    for (const e of filtered) {
      const list = byTag.get(e.tag) ?? [];
      list.push(e);
      byTag.set(e.tag, list);
    }
    return Array.from(byTag.entries()).sort(([a], [b]) => a.localeCompare(b));
  }, [spec, search]);

  if (loadingSpec) {
    return <EmptyState title="Loading spec..." />;
  }
  if (!spec) {
    return <EmptyState title="No spec loaded" />;
  }
  if (!grouped || grouped.length === 0) {
    return (
      <div className="p-3">
        <input
          className="w-full px-2 py-1 bg-slate-900 border border-slate-800 rounded text-xs font-mono text-slate-200 placeholder-slate-600"
          placeholder="Search endpoints..."
          value={search}
          onChange={(e) => setSearch(e.target.value)}
        />
        <EmptyState title="No matches" />
      </div>
    );
  }

  return (
    <div className="p-2 space-y-1">
      <input
        className="w-full px-2 py-1 bg-slate-900 border border-slate-800 rounded text-xs font-mono text-slate-200 placeholder-slate-600 focus:outline-none focus:border-indigo-500"
        placeholder="Search endpoints..."
        value={search}
        onChange={(e) => setSearch(e.target.value)}
      />
      {grouped.map(([tag, entries]) => (
        <details key={tag} open className="group">
          <summary className="cursor-pointer px-2 py-1 text-[10px] font-mono uppercase tracking-widest text-slate-500 hover:text-slate-300 select-none">
            {tag} <span className="text-slate-700">({entries.length})</span>
          </summary>
          <ul className="mt-1 space-y-0.5">
            {entries.map((e) => (
              <li key={e.operationId}>
                <button
                  type="button"
                  onClick={() => select(e.operationId)}
                  className={`w-full text-left px-2 py-1 rounded text-xs flex items-center gap-2 ${
                    selected === e.operationId
                      ? "bg-indigo-500/20 text-slate-100"
                      : "hover:bg-slate-900 text-slate-400"
                  }`}
                >
                  <MethodBadge method={e.method} />
                  <span className="font-mono truncate">{e.path}</span>
                </button>
              </li>
            ))}
          </ul>
        </details>
      ))}
    </div>
  );
}
