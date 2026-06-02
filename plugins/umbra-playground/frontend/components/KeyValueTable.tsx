import { useState } from "react";

export interface KV {
  key: string;
  value: string;
}

export function KeyValueTable({
  rows,
  onChange,
  keyPlaceholder = "key",
  valuePlaceholder = "value",
}: {
  rows: KV[];
  onChange: (rows: KV[]) => void;
  keyPlaceholder?: string;
  valuePlaceholder?: string;
}) {
  const [adding, setAdding] = useState(false);
  const [draft, setDraft] = useState<KV>({ key: "", value: "" });

  const update = (i: number, patch: Partial<KV>) => {
    const next = rows.map((r, idx) => (idx === i ? { ...r, ...patch } : r));
    onChange(next);
  };
  const remove = (i: number) => {
    onChange(rows.filter((_, idx) => idx !== i));
  };
  const commit = () => {
    if (!draft.key && !draft.value) {
      setAdding(false);
      return;
    }
    onChange([...rows, draft]);
    setDraft({ key: "", value: "" });
    setAdding(false);
  };

  return (
    <div className="space-y-1.5 text-xs">
      {rows.map((row, i) => (
        <div key={i} className="flex gap-1.5">
          <input
            value={row.key}
            onChange={(e) => update(i, { key: e.target.value })}
            placeholder={keyPlaceholder}
            className="flex-1 px-2 py-1 bg-slate-900 border border-slate-800 rounded font-mono text-slate-200 placeholder-slate-600 focus:outline-none focus:border-indigo-500"
          />
          <input
            value={row.value}
            onChange={(e) => update(i, { value: e.target.value })}
            placeholder={valuePlaceholder}
            className="flex-[2] px-2 py-1 bg-slate-900 border border-slate-800 rounded font-mono text-slate-200 placeholder-slate-600 focus:outline-none focus:border-indigo-500"
          />
          <button
            type="button"
            onClick={() => remove(i)}
            className="px-2 py-1 text-slate-500 hover:text-rose-400"
            aria-label="Remove"
          >
            ×
          </button>
        </div>
      ))}
      {adding ? (
        <div className="flex gap-1.5">
          <input
            autoFocus
            value={draft.key}
            onChange={(e) => setDraft({ ...draft, key: e.target.value })}
            placeholder={keyPlaceholder}
            className="flex-1 px-2 py-1 bg-slate-900 border border-slate-800 rounded font-mono text-slate-200 placeholder-slate-600 focus:outline-none focus:border-indigo-500"
          />
          <input
            value={draft.value}
            onChange={(e) => setDraft({ ...draft, value: e.target.value })}
            placeholder={valuePlaceholder}
            onKeyDown={(e) => {
              if (e.key === "Enter") commit();
              if (e.key === "Escape") {
                setAdding(false);
                setDraft({ key: "", value: "" });
              }
            }}
            className="flex-[2] px-2 py-1 bg-slate-900 border border-slate-800 rounded font-mono text-slate-200 placeholder-slate-600 focus:outline-none focus:border-indigo-500"
          />
          <button
            type="button"
            onClick={commit}
            className="px-2 py-1 text-indigo-300 hover:text-indigo-200"
          >
            ✓
          </button>
        </div>
      ) : (
        <button
          type="button"
          onClick={() => setAdding(true)}
          className="text-[10px] font-mono uppercase tracking-widest text-slate-500 hover:text-slate-200"
        >
          + add row
        </button>
      )}
    </div>
  );
}
