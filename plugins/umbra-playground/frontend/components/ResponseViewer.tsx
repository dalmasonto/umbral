import { usePlayground, type ResponseRecord } from "../state/store";
import { Tabs } from "./Tabs";
import { JsonView } from "./JsonView";
import { EmptyState } from "./EmptyState";
import { toCurl } from "../state/curl";

function StatusBadge({ status }: { status: number }) {
  const cls =
    status === 0
      ? "bg-slate-700 text-slate-200"
      : status < 300
      ? "bg-emerald-500/20 text-emerald-300"
      : status < 400
      ? "bg-amber-500/20 text-amber-300"
      : "bg-rose-500/20 text-rose-300";
  return (
    <span
      className={`inline-block px-2 py-0.5 rounded text-[10px] font-mono font-semibold ${cls}`}
    >
      {status === 0 ? "ERR" : status}
    </span>
  );
}

function HistoryRow({
  record,
  onRestore,
}: {
  record: ResponseRecord;
  onRestore: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onRestore}
      className="w-full text-left px-2 py-1.5 rounded hover:bg-slate-900 flex items-center gap-2 text-xs"
    >
      <StatusBadge status={record.status} />
      <span className="font-mono text-[10px] text-slate-500">
        {new Date(record.timestamp).toLocaleTimeString()}
      </span>
      <span className="font-mono text-[10px] text-slate-600">
        {record.durationMs}ms
      </span>
      <span className="font-mono text-xs text-slate-300 truncate flex-1">
        {record.request.method} {record.request.url}
      </span>
    </button>
  );
}

export function ResponseViewer() {
  const last = usePlayground((s) => s.lastResponse);
  const selected = usePlayground((s) => s.selectedOperationId);
  const history = usePlayground((s) => s.history);
  const resetCurrent = usePlayground((s) => s.resetCurrent);

  if (!last) {
    return <EmptyState title="Send a request to see the response" />;
  }

  const opId = selected ?? "unknown";
  const opHistory = history[opId] ?? [];
  const contentType = last.headers["content-type"] ?? "";
  const isJson = contentType.includes("application/json");
  const sortedHeaders = Object.entries(last.headers).sort(([a], [b]) => a.localeCompare(b));

  return (
    <div className="flex flex-col h-full">
      <div className="px-3 py-2 border-b border-slate-800 flex items-center gap-3 text-xs">
        <StatusBadge status={last.status} />
        <span className="font-mono text-slate-400">{last.statusText}</span>
        <span className="font-mono text-slate-600">·</span>
        <span className="font-mono text-slate-400">{last.durationMs}ms</span>
        <span className="font-mono text-slate-600">·</span>
        <span className="font-mono text-slate-400">{last.sizeBytes}b</span>
        {last.error && (
          <span className="ml-auto font-mono text-rose-300 text-[10px]">
            {last.error}
          </span>
        )}
      </div>
      <Tabs
        tabs={[
          {
            id: "body",
            label: "Body",
            content: isJson
              ? <JsonView text={last.bodyText} />
              : <pre className="font-mono text-xs text-slate-300 whitespace-pre-wrap break-all">{last.bodyText}</pre>,
          },
          {
            id: "headers",
            label: "Headers",
            content: (
              <div className="font-mono text-xs space-y-0.5">
                {sortedHeaders.map(([k, v]) => (
                  <div key={k} className="flex gap-2">
                    <span className="text-slate-500">{k}:</span>
                    <span className="text-slate-300 break-all">{v}</span>
                  </div>
                ))}
              </div>
            ),
          },
          {
            id: "history",
            label: `History (${opHistory.length})`,
            content: opHistory.length === 0
              ? <EmptyState title="No history yet" />
              : (
                <div className="space-y-1">
                  {[...opHistory].reverse().map((r, i) => (
                    <HistoryRow
                      key={opHistory.length - 1 - i}
                      record={r}
                      onRestore={() => {
                        resetCurrent({
                          method: r.request.method,
                          url: r.request.url,
                          params: r.request.params,
                          headers: r.request.headers,
                          body: r.request.body,
                          bearerToken: r.request.bearerToken,
                        });
                      }}
                    />
                  ))}
                </div>
              ),
          },
          {
            id: "curl",
            label: "cURL",
            content: (
              <pre className="font-mono text-xs text-slate-300 whitespace-pre-wrap break-all">
                {toCurl(last.request)}
              </pre>
            ),
          },
        ]}
      />
    </div>
  );
}
