import { useEffect, useMemo } from "react";
import { usePlayground } from "../state/store";
import { findOperation } from "../state/spec";
import { Tabs } from "./Tabs";
import { KeyValueTable, type KV } from "./KeyValueTable";
import { AuthTab } from "./AuthTab";
import { EmptyState } from "./EmptyState";

function kvToRecord(rows: KV[]): Record<string, string> {
  return Object.fromEntries(rows.map((r) => [r.key, r.value]).filter(([k]) => k));
}

function recordToKv(r: Record<string, string>): KV[] {
  return Object.entries(r).map(([key, value]) => ({ key, value }));
}

function buildPathParamInputs(
  path: string,
  params: Record<string, string>,
  setParam: (name: string, value: string) => void,
): JSX.Element | null {
  const names = [...path.matchAll(/\{([a-zA-Z_][a-zA-Z0-9_]*)\}/g)].map((m) => m[1]);
  if (names.length === 0) return null;
  return (
    <div className="flex flex-wrap gap-2">
      {names.map((name) => (
        <label key={name} className="flex items-center gap-1.5 text-xs">
          <span className="font-mono text-[10px] uppercase tracking-widest text-slate-500">
            {name}
          </span>
          <input
            value={params[name] ?? ""}
            onChange={(e) => setParam(name, e.target.value)}
            className="px-2 py-1 bg-slate-900 border border-slate-800 rounded font-mono text-slate-200 placeholder-slate-600 focus:outline-none focus:border-indigo-500 w-32"
            placeholder={`{${name}}`}
          />
        </label>
      ))}
    </div>
  );
}

export function RequestBuilder() {
  const spec = usePlayground((s) => s.spec);
  const selected = usePlayground((s) => s.selectedOperationId);
  const current = usePlayground((s) => s.current);
  const setUrl = usePlayground((s) => s.setUrl);
  const setParam = usePlayground((s) => s.setParam);
  const setHeader = usePlayground((s) => s.setHeader);
  const setBody = usePlayground((s) => s.setBody);
  const resetCurrent = usePlayground((s) => s.resetCurrent);
  const send = usePlayground((s) => s.send);
  const inFlight = usePlayground((s) => s.inFlight);

  const op = useMemo(
    () => (spec && selected ? findOperation(spec, selected) : null),
    [spec, selected],
  );

  useEffect(() => {
    if (op) {
      resetCurrent({ method: op.method, url: op.path });
    }
  }, [op?.method, op?.path, resetCurrent]);

  if (!selected) {
    return <EmptyState title="Select an endpoint" />;
  }

  const pathInputs = buildPathParamInputs(current.url, current.params, setParam);
  const paramRows: KV[] = recordToKv(current.params);
  const headerRows: KV[] = recordToKv(current.headers);

  return (
    <div className="flex flex-col h-full">
      <div className="p-3 space-y-2 border-b border-slate-800">
        <div className="flex items-center gap-2">
          <span className="font-mono text-xs px-2 py-1 rounded bg-slate-900 border border-slate-800 text-slate-300">
            {current.method}
          </span>
          <input
            value={current.url}
            onChange={(e) => setUrl(e.target.value)}
            className="flex-1 px-2 py-1 bg-slate-900 border border-slate-800 rounded font-mono text-sm text-slate-200 focus:outline-none focus:border-indigo-500"
          />
          <button
            type="button"
            onClick={() => void send()}
            disabled={inFlight}
            className="px-3 py-1 rounded bg-indigo-500 hover:bg-indigo-400 disabled:opacity-50 text-white text-xs font-semibold"
          >
            {inFlight ? "Sending..." : "Send"}
          </button>
        </div>
        {pathInputs}
      </div>
      <Tabs
        tabs={[
          {
            id: "params",
            label: "Params",
            content: (
              <KeyValueTable
                rows={paramRows}
                onChange={(rows) => {
                  const next: Record<string, string> = {};
                  for (const r of rows) {
                    if (r.key) next[r.key] = r.value;
                  }
                  for (const k of Object.keys(current.params)) {
                    if (!(k in next)) {
                      setParam(k, "");
                    }
                  }
                  for (const [k, v] of Object.entries(next)) {
                    if (current.params[k] !== v) setParam(k, v);
                  }
                }}
              />
            ),
          },
          {
            id: "body",
            label: "Body",
            content: (
              <div className="space-y-2 h-full flex flex-col">
                <textarea
                  value={current.body}
                  onChange={(e) => setBody(e.target.value)}
                  className="flex-1 w-full px-2 py-1 bg-slate-950 border border-slate-800 rounded font-mono text-xs text-slate-200 placeholder-slate-600 focus:outline-none focus:border-indigo-500 resize-none"
                  placeholder="JSON body"
                />
                <button
                  type="button"
                  onClick={() => {
                    try {
                      setBody(JSON.stringify(JSON.parse(current.body), null, 2));
                    } catch {
                      /* leave as-is */
                    }
                  }}
                  className="self-start text-[10px] font-mono uppercase tracking-widest text-slate-500 hover:text-slate-200"
                >
                  Format
                </button>
              </div>
            ),
          },
          {
            id: "headers",
            label: "Headers",
            content: (
              <KeyValueTable
                rows={headerRows}
                onChange={(rows) => {
                  const next = kvToRecord(rows);
                  for (const k of Object.keys(current.headers)) {
                    if (!(k in next)) setHeader(k, "");
                  }
                  for (const [k, v] of Object.entries(next)) {
                    if (current.headers[k] !== v) setHeader(k, v);
                  }
                }}
              />
            ),
          },
          { id: "auth", label: "Auth", content: <AuthTab /> },
        ]}
      />
    </div>
  );
}
