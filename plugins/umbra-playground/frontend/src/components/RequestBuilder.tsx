import { useEffect, useMemo, useState } from "react";
import { usePlayground } from "@/state/store";
import type { OpenAPIV3 } from "openapi-types";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Separator } from "@/components/ui/separator";
import { KeyValueEditor } from "./KeyValueEditor";
import { MethodBadge } from "./MethodBadge";
import { Send, Lock, AlignLeft, Code2, Braces, FormInput } from "lucide-react";
import Editor from "@monaco-editor/react";

function extractPathParams(url: string): string[] {
  return [...url.matchAll(/\{([a-zA-Z_][a-zA-Z0-9_]*)\}/g)].map((m) => m[1]);
}

function findPathParamValue(
  params: Array<{ key: string; value: string; enabled: boolean }>,
  name: string,
): string {
  return params.find((p) => p.key === name && p.enabled)?.value ?? "";
}

type TabId = "params" | "body" | "headers" | "auth";

const TABS: { id: TabId; label: string }[] = [
  { id: "params", label: "Params" },
  { id: "body", label: "Body" },
  { id: "headers", label: "Headers" },
  { id: "auth", label: "Auth" },
];

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

export function RequestBuilder() {
  const spec = usePlayground((s) => s.spec);
  const selected = usePlayground((s) => s.selectedOperationId);
  const current = usePlayground((s) => s.current);
  const setUrl = usePlayground((s) => s.setUrl);
  const setParams = usePlayground((s) => s.setParams);
  const setHeaders = usePlayground((s) => s.setHeaders);
  const setBody = usePlayground((s) => s.setBody);
  const setBodyType = usePlayground((s) => s.setBodyType);
  const setFormFields = usePlayground((s) => s.setFormFields);
  const setAuthScheme = usePlayground((s) => s.setAuthScheme);
  const setAuthToken = usePlayground((s) => s.setAuthToken);
  const resetCurrent = usePlayground((s) => s.resetCurrent);
  const send = usePlayground((s) => s.send);
  const inFlight = usePlayground((s) => s.inFlight);
  const [activeTab, setActiveTab] = useState<TabId>("params");
  const isDark = useIsDark();

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

  const enabledHeaders = current.headers.filter((h) => h.enabled);

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
            className="flex-1 font-mono text-sm h-10 rounded-md"
          />
          <Button
            onClick={handleSend}
            disabled={inFlight}
            size="sm"
            className="bg-primary hover:bg-primary/90 text-primary-foreground font-semibold text-xs h-10 px-4"
          >
            <Send className="size-3.5 mr-1.5" />
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
                  value={findPathParamValue(current.params, name)}
                  onChange={(e) => {
                    const existing = current.params.find((p) => p.key === name);
                    if (existing) {
                      setParams(
                        current.params.map((p) =>
                          p.key === name ? { ...p, value: e.target.value } : p,
                        ),
                      );
                    } else {
                      setParams([
                        ...current.params,
                        { key: name, value: e.target.value, enabled: true },
                      ]);
                    }
                  }}
                  placeholder={`{${name}}`}
                  className="w-36 font-mono text-sm h-9 rounded-md"
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
            className={`px-3 py-2.5 text-[11px] font-semibold uppercase tracking-wide transition-colors border-b-2 ${
              activeTab === tab.id
                ? "text-primary border-primary"
                : "text-muted-foreground border-transparent hover:text-foreground"
            }`}
          >
            {tab.label}
            {tab.id === "headers" && enabledHeaders.length > 0 && (
              <Badge variant="secondary" className="ml-1.5 text-[9px] px-1 py-0">
                {enabledHeaders.length}
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
              rows={current.params}
              onChange={setParams}
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
          <div className="h-full flex flex-col gap-3">
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-2">
                <button
                  type="button"
                  onClick={() => setBodyType("json")}
                  className={`flex items-center gap-1.5 px-2.5 py-1.5 rounded-md text-[10px] font-semibold uppercase tracking-wide transition-colors border ${
                    current.bodyType === "json"
                      ? "bg-primary/10 text-primary border-primary/20"
                      : "text-muted-foreground border-transparent hover:text-foreground hover:bg-muted"
                  }`}
                >
                  <Braces className="size-3" />
                  JSON
                </button>
                <button
                  type="button"
                  onClick={() => setBodyType("form")}
                  className={`flex items-center gap-1.5 px-2.5 py-1.5 rounded-md text-[10px] font-semibold uppercase tracking-wide transition-colors border ${
                    current.bodyType === "form"
                      ? "bg-primary/10 text-primary border-primary/20"
                      : "text-muted-foreground border-transparent hover:text-foreground hover:bg-muted"
                  }`}
                >
                  <FormInput className="size-3" />
                  Form
                </button>
              </div>
              {current.bodyType === "json" && (
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
              )}
            </div>

            {current.bodyType === "json" ? (
              <div className="flex-1 min-h-[12rem] rounded-md overflow-hidden border border-border">
                <Editor
                  height="100%"
                  language="json"
                  theme={isDark ? "vs-dark" : "light"}
                  value={current.body}
                  onChange={(v) => setBody(v ?? "")}
                  options={{
                    minimap: { enabled: false },
                    lineNumbers: "on",
                    wordWrap: "on",
                    folding: true,
                    scrollBeyondLastLine: false,
                    automaticLayout: true,
                    fontSize: 13,
                    fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", monospace',
                    tabSize: 2,
                    formatOnPaste: true,
                  }}
                />
              </div>
            ) : (
              <div className="space-y-2">
                <p className="text-[10px] text-muted-foreground">
                  application/x-www-form-urlencoded
                </p>
                <KeyValueEditor
                  rows={current.formFields}
                  onChange={setFormFields}
                  keyPlaceholder="field"
                  valuePlaceholder="value"
                  allowFileType
                />
              </div>
            )}
          </div>
        )}

        {activeTab === "headers" && (
          <div className="space-y-3">
            <p className="text-[10px] text-muted-foreground uppercase tracking-wider font-semibold">
              Request Headers
            </p>
            <KeyValueEditor
              rows={current.headers}
              onChange={setHeaders}
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
                  Authorization
                </p>
              </div>
              <div className="flex items-center gap-2">
                <Input
                  value={current.authScheme}
                  onChange={(e) => setAuthScheme(e.target.value)}
                  placeholder="Bearer"
                  className="w-28 font-mono text-sm h-9 rounded-md"
                />
                <Input
                  type="password"
                  value={current.authToken}
                  onChange={(e) => setAuthToken(e.target.value)}
                  placeholder="token"
                  className="flex-1 font-mono text-sm h-9 rounded-md"
                />
              </div>
              <p className="text-[10px] text-muted-foreground/60">
                Sent as <code className="font-mono text-foreground">
                  Authorization: {current.authScheme || "<scheme>"} {current.authToken ? "<token>" : "<empty>"}
                </code>
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
