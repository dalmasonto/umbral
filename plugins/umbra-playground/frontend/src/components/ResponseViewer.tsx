import { useState, useMemo, useEffect } from "react";
import { usePlayground } from "@/state/store";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import Editor from "@monaco-editor/react";
import {
  Clock,
  HardDrive,
  AlertTriangle,
  Trash2,
  Terminal,
  Eye,
  Package,
  Copy,
  Check,
  Search,
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
  for (const h of req.headers) {
    if (!h.enabled || !h.key) continue;
    parts.push(`-H '${h.key}: ${h.value.replace(/'/g, "'\\''")}'`);
  }
  if (req.authToken) {
    parts.push(`-H 'Authorization: ${req.authScheme} ${req.authToken}'`);
  }
  if (req.bodyType === "json" && req.body && req.method !== "GET" && req.method !== "HEAD") {
    parts.push(`-d '${req.body.replace(/'/g, "'\\''")}'`);
  } else if (req.bodyType === "form" && req.formFields.length > 0 && req.method !== "GET" && req.method !== "HEAD") {
    const hasFiles = req.formFields.some((f) => f.enabled && f.type === "file");
    if (hasFiles) {
      for (const f of req.formFields) {
        if (!f.enabled || !f.key) continue;
        if (f.type === "file") {
          parts.push(`-F '${f.key}=@${f.fileName || "file"}'`);
        } else {
          parts.push(`-F '${f.key}=${f.value.replace(/'/g, "'\\''")}'`);
        }
      }
    } else {
      const qs = req.formFields
        .filter((f) => f.enabled && f.key)
        .map(({ key, value }) => `${encodeURIComponent(key)}=${encodeURIComponent(value)}`)
        .join("&");
      parts.push(`-d '${qs.replace(/'/g, "'\\''")}'`);
    }
  }
  parts.push(`'${req.url}'`);
  return parts.join(" \\\n  ");
}

function useIsDark() {
  const [dark, setDark] = useState(() =>
    document.documentElement.classList.contains("dark"),
  );
  useEffect(() => {
    const obs = new MutationObserver(() =>
      setDark(document.documentElement.classList.contains("dark")),
    );
    obs.observe(document.documentElement, { attributes: true, attributeFilter: ["class"] });
    return () => obs.disconnect();
  }, []);
  return dark;
}

function HeadersTable({ headers }: { headers: Record<string, string> }) {
  const [filter, setFilter] = useState("");
  const [copiedKey, setCopiedKey] = useState<string | null>(null);

  const entries = useMemo(() => {
    const sorted = Object.entries(headers).sort(([a], [b]) =>
      a.toLowerCase().localeCompare(b.toLowerCase()),
    );
    const q = filter.trim().toLowerCase();
    if (!q) return sorted;
    return sorted.filter(
      ([k, v]) =>
        k.toLowerCase().includes(q) || v.toLowerCase().includes(q),
    );
  }, [headers, filter]);

  const total = Object.keys(headers).length;

  if (total === 0) {
    return (
      <p className="text-xs text-muted-foreground italic">
        No headers received.
      </p>
    );
  }

  const copyValue = (key: string, value: string) => {
    void navigator.clipboard.writeText(value);
    setCopiedKey(key);
    setTimeout(() => setCopiedKey((k) => (k === key ? null : k)), 1200);
  };

  return (
    <div className="space-y-2.5">
      {total > 5 && (
        <div className="relative">
          <Search className="pointer-events-none absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground" />
          <Input
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            placeholder={`Filter ${total} headers`}
            className="h-8 pl-7 font-mono text-xs"
          />
        </div>
      )}
      <div className="overflow-hidden rounded-md border border-border">
        <Table>
          <TableHeader className="bg-muted/40">
            <TableRow className="hover:bg-muted/40">
              <TableHead className="w-[14rem]">Header</TableHead>
              <TableHead>Value</TableHead>
              <TableHead className="w-10" />
            </TableRow>
          </TableHeader>
          <TableBody>
            {entries.length === 0 ? (
              <TableRow className="hover:bg-transparent">
                <TableCell
                  colSpan={3}
                  className="text-center text-xs italic text-muted-foreground"
                >
                  No headers match "{filter}".
                </TableCell>
              </TableRow>
            ) : (
              entries.map(([key, value]) => (
                <TableRow key={key} className="group">
                  <TableCell className="font-mono text-xs font-medium text-foreground">
                    {key}
                  </TableCell>
                  <TableCell className="break-all font-mono text-xs text-muted-foreground">
                    {value}
                  </TableCell>
                  <TableCell className="text-right">
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon-xs"
                      onClick={() => copyValue(key, value)}
                      title="Copy value"
                      className="opacity-0 transition-opacity group-hover:opacity-100 text-muted-foreground hover:text-foreground"
                    >
                      {copiedKey === key ? (
                        <Check className="size-3.5 text-emerald-600" />
                      ) : (
                        <Copy className="size-3.5" />
                      )}
                    </Button>
                  </TableCell>
                </TableRow>
              ))
            )}
          </TableBody>
        </Table>
      </div>
    </div>
  );
}

function ReadonlyMonaco({
  value,
  language,
}: {
  value: string;
  language: string;
}) {
  const isDark = useIsDark();
  return (
    <div className="flex-1 min-h-0 h-full rounded-md overflow-hidden border border-border">
      <Editor
        height="100%"
        language={language}
        theme={isDark ? "vs-dark" : "light"}
        value={value}
        options={{
          readOnly: true,
          minimap: { enabled: false },
          lineNumbers: "on",
          wordWrap: "on",
          folding: true,
          scrollBeyondLastLine: false,
          automaticLayout: true,
          fontSize: 13,
          fontFamily:
            'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", monospace',
          tabSize: 2,
        }}
      />
    </div>
  );
}

export function ResponseViewer() {
  const lastResponse = usePlayground((s) => s.lastResponse);
  const inFlight = usePlayground((s) => s.inFlight);
  const selected = usePlayground((s) => s.selectedOperationId);
  const history = usePlayground((s) => s.history);
  const clearHistory = usePlayground((s) => s.clearHistory);
  const [activeTab, setActiveTab] = useState<TabId>("body");

  const opHistory = selected ? history[selected] ?? [] : [];

  const prettyBody = useMemo(() => {
    if (!lastResponse) return null;
    try {
      return JSON.stringify(JSON.parse(lastResponse.bodyText), null, 2);
    } catch {
      return null;
    }
  }, [lastResponse]);

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
            className={`px-3 py-2.5 text-[11px] font-semibold uppercase tracking-wide transition-colors border-b-2 flex items-center gap-1.5 ${
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
          <div className="h-full flex flex-col">
            {prettyBody ? (
              <ReadonlyMonaco value={prettyBody} language="json" />
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
          <HeadersTable headers={headers} />
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
                  </div>
                );
              })}
            {opHistory.length > 0 && selected && (
              <Button
                type="button"
                variant="ghost"
                size="sm"
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
          <div className="space-y-2 h-full flex flex-col">
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
            <ReadonlyMonaco value={toCurl(lastResponse)} language="shell" />
          </div>
        )}
      </div>
    </div>
  );
}
