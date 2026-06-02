import { useState } from "react";

function isObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function Node({ name, value, depth }: { name: string; value: unknown; depth: number }) {
  const [open, setOpen] = useState(depth < 2);
  const isExpandable = isObject(value) || Array.isArray(value);
  if (!isExpandable) {
    return (
      <div className="flex gap-1.5 font-mono text-xs">
        <span className="text-slate-500">{name}:</span>
        <span className="text-emerald-300 break-all">
          {JSON.stringify(value)}
        </span>
      </div>
    );
  }
  const entries: Array<readonly [string, unknown]> = Array.isArray(value)
    ? value.map((v, i) => [String(i), v] as const)
    : Object.entries(value);
  return (
    <div className="font-mono text-xs">
      <button
        type="button"
        onClick={() => setOpen(!open)}
        className="text-slate-400 hover:text-slate-200"
      >
        {open ? "▾" : "▸"} {name} {Array.isArray(value) ? `(${value.length})` : `{${entries.length}}`}
      </button>
      {open && (
        <div className="ml-4 border-l border-slate-800 pl-2 mt-0.5 space-y-0.5">
          {entries.map(([k, v]) => (
            <Node key={k} name={k} value={v} depth={depth + 1} />
          ))}
        </div>
      )}
    </div>
  );
}

export function JsonView({ text }: { text: string }) {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    return (
      <pre className="font-mono text-xs text-slate-300 whitespace-pre-wrap break-all">
        {text}
      </pre>
    );
  }
  if (Array.isArray(parsed)) {
    return (
      <div className="space-y-0.5">
        <Node name="[root]" value={parsed} depth={0} />
      </div>
    );
  }
  if (isObject(parsed)) {
    return (
      <div className="space-y-0.5">
        {Object.entries(parsed).map(([k, v]) => (
          <Node key={k} name={k} value={v} depth={0} />
        ))}
      </div>
    );
  }
  return (
    <pre className="font-mono text-xs text-slate-300 whitespace-pre-wrap break-all">
      {JSON.stringify(parsed, null, 2)}
    </pre>
  );
}
