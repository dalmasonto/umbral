import { useEffect, useMemo, useState } from "react";
import { usePlayground } from "@/state/store";
import type { OpenAPIV3 } from "openapi-types";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";
import { Badge } from "@/components/ui/badge";
import { Separator } from "@/components/ui/separator";
import { KeyValueEditor, type KVRow } from "./KeyValueEditor";
import { MethodBadge } from "./MethodBadge";
import { Send, Lock, AlignLeft, Code2 } from "lucide-react";

function recordToKv(r: Record<string, string>): KVRow[] {
  return Object.entries(r).map(([key, value]) => ({ key, value }));
}

function kvToRecord(rows: KVRow[]): Record<string, string> {
  const out: Record<string, string> = {};
  for (const r of rows) {
    if (r.key) out[r.key] = r.value;
  }
  return out;
}

function extractPathParams(url: string): string[] {
  return [...url.matchAll(/\{([a-zA-Z_][a-zA-Z0-9_]*)\}/g)].map((m) => m[1]);
}

type TabId = "params" | "body" | "headers" | "auth";

const TABS: { id: TabId; label: string }[] = [
  { id: "params", label: "Params" },
  { id: "body", label: "Body" },
  { id: "headers", label: "Headers" },
  { id: "auth", label: "Auth" },
];

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
  const [activeTab, setActiveTab] = useState<TabId>("params");

  const op = useMemo(() => {
    if (!spec || !selected) return null;
    for (const [path, pathItem] of Object.entries(spec.paths ?? {})) {
      if (!pathItem) continue;
      const methods: Array<[string, OpenAPIV3.OperationObject | undefined]> = [
        ["GET", pathItem.get],
        ["POST", pathItem.post],
        ["PUT", pathItem.put],
        ["PATCH", pathItem.patch],
        ["DELETE", pathItem.delete],
      ];
      for (const [method, operation] of methods) {
        if (!operation) continue;
        const id = operation.operationId ?? `${method} ${path}`;
        if (id === selected) {
          return { method, path, operation };
        }
      }
    }
    return null;
  }, [spec, selected]);

  useEffect(() => {
    if (op) {
      resetCurrent({ method: op.method, url: op.path });
      setActiveTab("params");
    }
  }, [op?.method, op?.path, resetCurrent]);

  const pathParams = useMemo(() => extractPathParams(current.url), [current.url]);

  const handleSend = () => {
    void send();
  };

  if (!selected) {
    return (
      <div className="flex items-center justify-center h-full text-muted-foreground">
        <div className="text-center space-y-2">
          <Code2 className="size-8 mx-auto opacity-40" />
          <p className="text-xs font-medium">Select an endpoint from the sidebar</p>
          <p className="text-[10px] text-muted-foreground/60">Choose an operation to start building your request.</p>
        </div>
      </div>
    );
  }

  const paramRows = recordToKv(current.params);
  const headerRows = recordToKv(current.headers);

  return (
    <div className="flex flex-col h-full">
      {/* URL Bar */}
      <div className="p-3 border-b border-border space-y-2">
        <div className="flex items-center gap-2">
          <MethodBadge method={current.method} />
          <Input
            value={current.url}
            onChange={(e) => setUrl(e.target.value)}
            placeholder="/api/endpoint"
            className="flex-1 font-mono text-xs h-8"
          />
          <Button
            onClick={handleSend}
            disabled={inFlight}
            size="sm"
            className="bg-primary hover:bg-primary/90 text-primary-foreground font-semibold text-xs h-8 px-3"
          >
            <Send className="size-3.5 mr-1" />
            {inFlight ? "Sending…" : "Send"}
          </Button>
        </div>

        {pathParams.length > 0 && (
          <div className="flex flex-wrap gap-2">
            {pathParams.map((name) => (
              <div key={name} className="flex items-center gap-1.5">
                <span className="text-[10px] font-mono uppercase tracking-wider text-muted-foreground">
                  {name}
                </span>
                <Input
                  value={current.params[name] ?? ""}
                  onChange={(e) => setParam(name, e.target.value)}
                  placeholder={`{${name}}`}
                  className="w-32 font-mono text-xs h-7"
                />
              </div>
            ))}
          </div>
        )}
      </div>

      {/* Tabs */}
      <div className="flex items-center gap-0 border-b border-border px-2">
        {TABS.map((tab) => (
          <button
            key={tab.id}
            type="button"
            onClick={() => setActiveTab(tab.id)}
            className={`px-3 py-2 text-[11px] font-semibold uppercase tracking-wide transition-colors border-b-2 ${
              activeTab === tab.id
                ? "text-primary border-primary"
                : "text-muted-foreground border-transparent hover:text-foreground"
            }`}
          >
            {tab.label}
            {tab.id === "headers" && headerRows.length > 0 && (
              <Badge variant="secondary" className="ml-1 text-[9px] px-1 py-0">
                {headerRows.length}
              </Badge>
            )}
          </button>
        ))}
      </div>

      {/* Tab Content */}
      <div className="flex-1 overflow-y-auto p-3">
        {activeTab === "params" && (
          <div className="space-y-3">
            <p className="text-[10px] text-muted-foreground uppercase tracking-wider font-semibold">
              Query &amp; Path Parameters
            </p>
            <KeyValueEditor
              rows={paramRows}
              onChange={(rows) => {
                const next = kvToRecord(rows);
                for (const k of Object.keys(current.params)) {
                  if (!(k in next)) setParam(k, "");
                }
                for (const [k, v] of Object.entries(next)) {
                  if (current.params[k] !== v) setParam(k, v);
                }
              }}
              keyPlaceholder="param"
              valuePlaceholder="value"
            />
            {op?.operation.parameters && op.operation.parameters.length > 0 && (
              <>
                <Separator />
                <div className="space-y-1">
                  <p className="text-[10px] text-muted-foreground uppercase tracking-wider font-semibold">
                    Declared Parameters
                  </p>
                  {(op.operation.parameters as OpenAPIV3.ParameterObject[]).map((p) => (
                    <div key={p.name} className="flex items-center gap-2 text-xs">
                      <span className="font-mono text-foreground">{p.name}</span>
                      <Badge variant="outline" className="text-[9px] h-4 px-1">{p.in}</Badge>
                      {p.required && (
                        <Badge variant="destructive" className="text-[9px] h-4 px-1">required</Badge>
                      )}
                      <span className="text-muted-foreground">
                        {typeof p.schema === "object" && !("$ref" in p.schema) && p.schema.type}
                      </span>
                    </div>
                  ))}
                </div>
              </>
            )}
          </div>
        )}

        {activeTab === "body" && (
          <div className="h-full flex flex-col gap-2">
            <div className="flex items-center justify-between">
              <p className="text-[10px] text-muted-foreground uppercase tracking-wider font-semibold">
                Request Body
              </p>
              <Button
                type="button"
                variant="ghost"
                size="xs"
                onClick={() => {
                  try {
                    setBody(JSON.stringify(JSON.parse(current.body), null, 2));
                  } catch {
                    /* leave as-is */
                  }
                }}
                className="text-muted-foreground hover:text-foreground text-[10px]"
              >
                <AlignLeft className="size-3 mr-1" />
                Format JSON
              </Button>
            </div>
            <Textarea
              value={current.body}
              onChange={(e) => setBody(e.target.value)}
              placeholder={`{\n  "title": "Hello World",\n  "content": "..."\n}`}
              className="flex-1 font-mono text-xs resize-none min-h-[8rem]"
              spellCheck={false}
            />
          </div>
        )}

        {activeTab === "headers" && (
          <div className="space-y-3">
            <p className="text-[10px] text-muted-foreground uppercase tracking-wider font-semibold">
              Request Headers
            </p>
            <KeyValueEditor
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
              keyPlaceholder="Header"
              valuePlaceholder="Value"
            />
          </div>
        )}

        {activeTab === "auth" && (
          <div className="space-y-4">
            <div className="space-y-2">
              <div className="flex items-center gap-2">
                <Lock className="size-3.5 text-muted-foreground" />
                <p className="text-[10px] text-muted-foreground uppercase tracking-wider font-semibold">
                  Bearer Token
                </p>
              </div>
              <Input
                type="password"
                value={current.bearerToken}
                onChange={(e) => setHeader("Authorization", e.target.value ? `Bearer ${e.target.value}` : "")}
                placeholder="eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9..."
                className="font-mono text-xs h-8"
              />
              <p className="text-[10px] text-muted-foreground/60">
                Sent as <code className="font-mono text-foreground">Authorization: Bearer &lt;token&gt;</code>
              </p>
            </div>
            <Separator />
            <div className="p-3 rounded-lg bg-muted/50 border border-border/50">
              <p className="text-[10px] text-muted-foreground leading-relaxed">
                For session-based auth, make sure you are logged into the app in another tab.
                The playground shares cookies with the rest of the application.
              </p>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
