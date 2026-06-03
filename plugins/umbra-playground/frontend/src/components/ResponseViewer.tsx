import { useState } from "react";
import { usePlayground } from "@/state/store";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { JsonView } from "./JsonView";
import {
  Clock,
  HardDrive,
  AlertTriangle,
  Trash2,
  Terminal,
  Eye,
  Package,
} from "lucide-react";

import type { ResponseRecord } from "@/state/store";

type TabId = "body" | "headers" | "history" | "curl";

const TABS: { id: TabId; label: string; icon: React.ReactNode }[] = [
  { id: "body", label: "Body", icon: <Eye className="size-3" /> },
  { id: "headers", label: "Headers", icon: <Package className="size-3" /> },
  { id: "history", label: "History", icon: <Clock className="size-3" /> },
  { id: "curl", label: "cURL", icon: <Terminal className="size-3" /> },
];

function statusColor(status: number): string {
  if (status >= 200 && status < 300) return "bg-emerald-500/15 text-emerald-600 border-emerald-500/25";
  if (status >= 300 && status < 400) return "bg-amber-500/15 text-amber-600 border-amber-500/25";
  if (status >= 400 && status < 500) return "bg-rose-500/15 text-rose-600 border-rose-500/25";
  if (status >= 500) return "bg-rose-500/15 text-rose-600 border-rose-500/25";
  return "bg-muted text-muted-foreground";
}

function toCurl(record: ResponseRecord): string {
  const req = record.request;
  const parts: string[] = [`curl -X ${req.method}`];
  for (const [k, v] of Object.entries(req.headers)) {
    if (!v) continue;
    parts.push(`-H '${k}: ${v.replace(/'/g, "'\\''")}'`);
  }
  if (req.bearerToken) {
    parts.push(`-H 'Authorization: Bearer ${req.bearerToken}'`);
  }
  if (req.body && req.method !== "GET" && req.method !== "HEAD") {
    parts.push(`-d '${req.body.replace(/'/g, "'\\''")}'`);
  }
  parts.push(`'${req.url}'`);
  return parts.join(" \\\n  ");
}

export function ResponseViewer() {
  const lastResponse = usePlayground((s) => s.lastResponse);
  const inFlight = usePlayground((s) => s.inFlight);
  const selected = usePlayground((s) => s.selectedOperationId);
  const history = usePlayground((s) => s.history);
  const clearHistory = usePlayground((s) => s.clearHistory);
  const [activeTab, setActiveTab] = useState<TabId>("body");

  const opHistory = selected ? history[selected] ?? [] : [];

  if (inFlight) {
    return (
      <div className="flex flex-col h-full p-4 space-y-3">
        <Skeleton className="h-8 w-full" />
        <Skeleton className="h-4 w-3/4" />
        <Skeleton className="h-4 w-1/2" />
        <Skeleton className="h-32 w-full" />
      </div>
    );
  }

  if (!lastResponse) {
    return (
      <div className="flex items-center justify-center h-full text-muted-foreground">
        <div className="text-center space-y-2">
          <HardDrive className="size-8 mx-auto opacity-40" />
          <p className="text-xs font-medium">No response yet</p>
          <p className="text-[10px] text-muted-foreground/60">
            Send a request to see the response here.
          </p>
        </div>
      </div>
    );
  }

  const { status, statusText, durationMs, sizeBytes, bodyText, headers, error } =
    lastResponse;

  const prettyBody = (() => {
    try {
      return JSON.parse(bodyText);
    } catch {
      return null;
    }
  })();

  return (
    <div className="flex flex-col h-full">
      {/* Status Bar */}
      <div className="p-3 border-b border-border flex items-center gap-3 flex-wrap">
        {error ? (
          <div className="flex items-center gap-2 text-destructive">
            <AlertTriangle className="size-4" />
            <span className="text-xs font-semibold">Network Error</span>
            <span className="text-[10px] text-muted-foreground">{error}</span>
          </div>
        ) : (
          <>
            <Badge
              variant="outline"
              className={`font-mono text-xs font-bold ${statusColor(status)}`}
            >
              {status} {statusText}
            </Badge>
            <div className="flex items-center gap-1 text-[10px] text-muted-foreground">
              <Clock className="size-3" />
              <span>{durationMs}ms</span>
            </div>
            <div className="flex items-center gap-1 text-[10px] text-muted-foreground">
              <HardDrive className="size-3" />
              <span>{(sizeBytes / 1024).toFixed(2)}KB</span>
            </div>
          </>
        )}
      </div>

      {/* Tabs */}
      <div className="flex items-center gap-0 border-b border-border px-2">
        {TABS.map((tab) => (
          <button
            key={tab.id}
            type="button"
            onClick={() => setActiveTab(tab.id)}
            className={`px-3 py-2 text-[11px] font-semibold uppercase tracking-wide transition-colors border-b-2 flex items-center gap-1.5 ${
              activeTab === tab.id
                ? "text-primary border-primary"
                : "text-muted-foreground border-transparent hover:text-foreground"
            }`}
          >
            {tab.icon}
            {tab.label}
            {tab.id === "history" && opHistory.length > 0 && (
              <Badge variant="secondary" className="text-[9px] px-1 py-0">
                {opHistory.length}
              </Badge>
            )}
          </button>
        ))}
      </div>

      {/* Tab Content */}
      <div className="flex-1 overflow-y-auto p-3">
        {activeTab === "body" && (
          <div className="h-full">
            {prettyBody ? (
              <div className="font-mono text-xs leading-relaxed">
                <JsonView data={prettyBody} collapsed={false} />
              </div>
            ) : (
              <pre className="font-mono text-xs whitespace-pre-wrap break-all text-foreground">
                {bodyText || (
                  <span className="text-muted-foreground italic">
                    Empty response body
                  </span>
                )}
              </pre>
            )}
          </div>
        )}

        {activeTab === "headers" && (
          <div className="space-y-1">
            {Object.entries(headers)
              .sort(([a], [b]) => a.localeCompare(b))
              .map(([key, value]) => (
                <div
                  key={key}
                  className="flex items-start gap-2 text-xs py-0.5"
                >
                  <span className="font-mono text-foreground font-medium min-w-[10rem]">
                    {key}
                  </span>
                  <span className="font-mono text-muted-foreground break-all">
                    {value}
                  </span>
                </div>
              ))}
            {Object.keys(headers).length === 0 && (
              <p className="text-xs text-muted-foreground italic">
                No headers received.
              </p>
            )}
          </div>
        )}

        {activeTab === "history" && (
          <div className="space-y-2">
            {opHistory.length === 0 && (
              <p className="text-xs text-muted-foreground italic text-center py-4">
                No history for this endpoint yet.
              </p>
            )}
            {opHistory
              .slice()
              .reverse()
              .map((record, idx) => {
                const actualIndex = opHistory.length - 1 - idx;
                return (
                  <div
                    key={actualIndex}
                    className="group flex items-center gap-2 p-2 rounded-md border border-border hover:bg-muted/50 transition-colors"
                  >
                    <Badge
                      variant="outline"
                      className={`text-[9px] font-mono font-bold ${statusColor(
                        record.status,
                      )}`}
                    >
                      {record.status || "ERR"}
                    </Badge>
                    <span className="text-[10px] text-muted-foreground font-mono">
                      {record.durationMs}ms
                    </span>
                    <span className="text-[10px] text-muted-foreground font-mono">
                      {(record.sizeBytes / 1024).toFixed(2)}KB
                    </span>
                    <span className="text-[10px] text-muted-foreground ml-auto">
                      {new Date(record.timestamp).toLocaleTimeString()}
                    </span>
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon-xs"
                      className="opacity-0 group-hover:opacity-100 transition-opacity text-muted-foreground hover:text-destructive"
                      onClick={() => selected && clearHistory(selected)}
                    >
                      <Trash2 className="size-3" />
                    </Button>
                  </div>
                );
              })}
            {opHistory.length > 0 && selected && (
              <Button
                type="button"
                variant="ghost"
                size="xs"
                onClick={() => clearHistory(selected)}
                className="text-muted-foreground hover:text-destructive text-[10px]"
              >
                <Trash2 className="size-3 mr-1" />
                Clear history
              </Button>
            )}
          </div>
        )}

        {activeTab === "curl" && (
          <div className="space-y-2">
            <div className="flex items-center justify-between">
              <p className="text-[10px] text-muted-foreground uppercase tracking-wider font-semibold">
                Equivalent cURL
              </p>
              <Button
                type="button"
                variant="ghost"
                size="xs"
                onClick={() =>
                  navigator.clipboard.writeText(toCurl(lastResponse))
                }
                className="text-muted-foreground hover:text-foreground text-[10px]"
              >
                Copy
              </Button>
            </div>
            <pre className="font-mono text-[11px] whitespace-pre-wrap break-all p-3 rounded-lg bg-muted border border-border text-foreground">
              {toCurl(lastResponse)}
            </pre>
          </div>
        )}
      </div>
    </div>
  );
}
