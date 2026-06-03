import { useEffect, useMemo, useRef, useState } from "react";
import { usePlayground } from "@/state/store";
import { usePersistedState } from "@/hooks/usePersistedState";
import type { OpenAPIV3 } from "openapi-types";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Separator } from "@/components/ui/separator";
import { Checkbox } from "@/components/ui/checkbox";
import { KeyValueEditor } from "./KeyValueEditor";
import { MethodBadge } from "./MethodBadge";
import { SchemaTable } from "./SchemaTable";
import {
  fieldInfosFromSchema,
  listItemSchema,
  requestBodySchema,
  responseSchemaEntries,
  synthesizeJsonBody,
  type FieldInfo,
} from "@/lib/openapiSchema";
import {
  Send,
  Lock,
  AlignLeft,
  Code2,
  Braces,
  FormInput,
  Filter as FilterIcon,
  Plus,
  FileJson,
  FileOutput,
  Trash2,
} from "lucide-react";
import Editor from "@monaco-editor/react";

function extractPathParams(url: string): string[] {
  return [...url.matchAll(/(?<!\{)\{([a-zA-Z_][a-zA-Z0-9_]*)\}(?!\})/g)].map(
    (m) => m[1],
  );
}

function findPathParamValue(
  params: Array<{ key: string; value: string; enabled: boolean }>,
  name: string,
): string {
  return params.find((p) => p.key === name && p.enabled)?.value ?? "";
}

/** Build the display URL from stored URL + params. Preserves trailing ?. */
function buildDisplayUrl(
  url: string,
  params: Array<{ key: string; value: string; enabled: boolean }>,
): string {
  const base = url.split("?")[0];
  const queryEntries = params.filter(
    (p) => p.enabled && p.key && !base.includes(`{${p.key}}`),
  );
  if (queryEntries.length === 0) {
    // Preserve trailing ? if user explicitly typed it with no params.
    if (url.endsWith("?")) return `${base}?`;
    return base;
  }
  const qs = queryEntries
    .map(
      ({ key, value }) =>
        `${encodeURIComponent(key)}=${encodeURIComponent(value)}`,
    )
    .join("&");
  return `${base}?${qs}`;
}

/** Parse a full URL input into base path + query params. */
function parseUrlInput(
  input: string,
): { baseUrl: string; queryEntries: Array<{ key: string; value: string }> } {
  const qIdx = input.indexOf("?");
  if (qIdx === -1) return { baseUrl: input, queryEntries: [] };
  const baseUrl = input.slice(0, qIdx);
  const queryString = input.slice(qIdx + 1);
  if (!queryString) return { baseUrl, queryEntries: [] };
  try {
    const sp = new URLSearchParams(queryString);
    const queryEntries: Array<{ key: string; value: string }> = [];
    sp.forEach((value, key) => {
      queryEntries.push({ key, value });
    });
    return { baseUrl, queryEntries };
  } catch {
    return { baseUrl, queryEntries: [] };
  }
}

function findOperation(
  spec: OpenAPIV3.Document | null,
  selected: string | null,
): { method: string; path: string; operation: OpenAPIV3.OperationObject } | null {
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
}

type TabId = "params" | "body" | "headers" | "auth" | "schema";

const TABS: { id: TabId; label: string }[] = [
  { id: "params", label: "Params" },
  { id: "body", label: "Body" },
  { id: "headers", label: "Headers" },
  { id: "auth", label: "Auth" },
  { id: "schema", label: "Schema" },
];

/** Lookup suffixes the umbra-rest list view recognises. Mirrors the
 *  __eq/__contains/etc grammar Django-filter exposes, scoped per
 *  scalar type to keep the chip palette short. */
const LOOKUPS_BY_TYPE: Record<string, string[]> = {
  string: ["", "__contains", "__icontains", "__startswith", "__in"],
  integer: ["", "__gte", "__lte", "__gt", "__lt", "__in"],
  number: ["", "__gte", "__lte", "__gt", "__lt", "__in"],
  boolean: [""],
  // Choice/enum fields get __in for set-membership, no substring lookups.
  enum: ["", "__in"],
};

function lookupsFor(f: FieldInfo): string[] {
  if (f.enumValues && f.enumValues.length > 0) return LOOKUPS_BY_TYPE.enum;
  if (f.type === "string" && (f.format === "date" || f.format === "date-time")) {
    return ["", "__gte", "__lte", "__gt", "__lt"];
  }
  return LOOKUPS_BY_TYPE[f.type] ?? [""];
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
  const [activeTab, setActiveTab] = usePersistedState<TabId>(
    "request-builder.active-tab",
    "params",
  );
  const isDark = useIsDark();

  const op = findOperation(spec, selected);
  const opMethod = op?.method;
  const opPath = op?.path;

  useEffect(() => {
    if (opMethod && opPath) {
      resetCurrent({ method: opMethod, url: opPath });
    }
  }, [opMethod, opPath, resetCurrent]);

  const pathParams = useMemo(() => extractPathParams(current.url), [current.url]);

  const displayUrl = useMemo(
    () => buildDisplayUrl(current.url, current.params),
    [current.url, current.params],
  );

  // Introspected schema info — drives the Schema tab and the filter
  // chip affordance in the Params tab.
  const requestBody = useMemo(
    () => requestBodySchema(op?.operation, spec),
    [op?.operation, spec],
  );
  const requestBodyFields = useMemo(
    () => fieldInfosFromSchema(requestBody?.schema ?? null, spec),
    [requestBody?.schema, spec],
  );

  // Auto-fill the JSON body with required fields the first time we
  // see each operation that declares a request body. Behaviour:
  //
  // - On endpoint click (opMethod+opPath change), we synthesise the
  //   "required only" skeleton and put it on `current.body`. The
  //   user lands on the Body tab with a runnable starting point.
  // - We track the last operation we autofilled (via ref) so that
  //   harmless re-renders — spec re-fetched, schema fields ident-
  //   ity-stable, etc. — don't clobber subsequent user edits.
  // - The Schema-tab buttons (`Required only` / `All fields`) still
  //   work as manual overrides if you want to refill from scratch
  //   or expand to every field.
  const autofilledForRef = useRef<string | null>(null);
  useEffect(() => {
    if (!opMethod || !opPath) return;
    const key = `${opMethod} ${opPath}`;
    if (autofilledForRef.current === key) return;
    if (!requestBody || requestBodyFields.length === 0) return;
    autofilledForRef.current = key;
    setBody(synthesizeJsonBody(requestBodyFields, { allFields: false }));
    setBodyType("json");
  }, [
    opMethod,
    opPath,
    requestBody,
    requestBodyFields,
    setBody,
    setBodyType,
  ]);

  const responses = useMemo(
    () => responseSchemaEntries(op?.operation, spec),
    [op?.operation, spec],
  );
  const listItem = useMemo(
    () => listItemSchema(op?.operation, spec),
    [op?.operation, spec],
  );
  // True when the spec already declares filter parameters (via the
  // `x-umbra-filter-field` vendor key umbra-openapi emits on list ops
  // when ResourceConfig.enable_filters() is on). In that case the
  // declared-parameters table below already renders them — showing
  // inferred chips on top would duplicate the UI.
  const specDeclaresFilters = useMemo(() => {
    const params =
      (op?.operation?.parameters as OpenAPIV3.ParameterObject[] | undefined) ?? [];
    return params.some(
      (p) =>
        (p as unknown as Record<string, unknown>)["x-umbra-filter-field"] !==
        undefined,
    );
  }, [op?.operation]);

  const filterableFields = useMemo<FieldInfo[]>(() => {
    if (current.method !== "GET" || !listItem) return [];
    if (specDeclaresFilters) return [];
    return fieldInfosFromSchema(listItem.schema, spec);
  }, [current.method, listItem, spec, specDeclaresFilters]);

  const addFilterParam = (key: string) => {
    const existing = current.params.find((p) => p.key === key);
    if (existing) {
      // Already present — just enable it and focus by re-emitting.
      if (!existing.enabled) {
        setParams(
          current.params.map((p) =>
            p.key === key ? { ...p, enabled: true } : p,
          ),
        );
      }
      return;
    }
    setParams([
      ...current.params,
      { key, value: "", enabled: true },
    ]);
  };

  const handleUrlChange = (input: string) => {
    const { queryEntries } = parseUrlInput(input);
    // Store the full input so trailing ? is preserved while typing.
    setUrl(input);

    if (queryEntries.length > 0) {
      // Merge query entries into current params.
      const next = [...current.params];
      for (const { key, value } of queryEntries) {
        const idx = next.findIndex((p) => p.key === key);
        if (idx >= 0) {
          next[idx] = { ...next[idx], value, enabled: true };
        } else {
          next.push({ key, value, enabled: true });
        }
      }
      setParams(next);
    }
  };

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
            value={displayUrl}
            onChange={(e) => handleUrlChange(e.target.value)}
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
          <div className="space-y-4">
            {filterableFields.length > 0 && (
              <div className="space-y-2 rounded-lg border border-border bg-muted/20 p-3">
                <div className="flex items-center justify-between">
                  <div className="flex items-center gap-1.5">
                    <FilterIcon className="size-3.5 text-primary" />
                    <p className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
                      Suggested filters
                    </p>
                  </div>
                  {listItem?.name && (
                    <Badge
                      variant="outline"
                      className="h-4 rounded px-1 font-mono text-[9px]"
                    >
                      {listItem.name}
                    </Badge>
                  )}
                </div>
                <p className="text-[10px] leading-relaxed text-muted-foreground">
                  Click a chip to add the filter as a query parameter. Lookup
                  suffixes follow the <code className="font-mono">__lookup</code>
                  {" "}grammar (eq, contains, gte, in, …).
                </p>
                <div className="space-y-2">
                  {filterableFields
                    .filter((f) => !f.fkTarget || f.type === "integer")
                    .map((f) => {
                      const lookups = lookupsFor(f);
                      return (
                        <div
                          key={f.name}
                          className="flex flex-wrap items-center gap-1.5 border-t border-border/60 pt-2 first:border-t-0 first:pt-0"
                        >
                          <span className="mr-1 font-mono text-[11px] font-medium text-foreground">
                            {f.name}
                          </span>
                          {f.enumValues && (
                            <Badge
                              variant="secondary"
                              className="h-4 rounded px-1 font-mono text-[9px]"
                              title={f.enumValues.join(" | ")}
                            >
                              choice
                            </Badge>
                          )}
                          {f.fkTarget && (
                            <Badge
                              variant="secondary"
                              className="h-4 rounded px-1 font-mono text-[9px]"
                            >
                              FK → {f.fkTarget}
                            </Badge>
                          )}
                          {lookups.map((suffix) => {
                            const key = `${f.name}${suffix}`;
                            const active = current.params.some(
                              (p) => p.key === key && p.enabled,
                            );
                            return (
                              <button
                                key={key}
                                type="button"
                                onClick={() => addFilterParam(key)}
                                className={`flex items-center gap-1 rounded-md border px-1.5 py-0.5 font-mono text-[10px] transition-colors ${
                                  active
                                    ? "border-primary/40 bg-primary/10 text-primary"
                                    : "border-border bg-background text-muted-foreground hover:text-foreground hover:border-primary/30"
                                }`}
                              >
                                <Plus className="size-2.5" />
                                {suffix === "" ? "= eq" : suffix.slice(2)}
                              </button>
                            );
                          })}
                        </div>
                      );
                    })}
                </div>
              </div>
            )}

            <div className="space-y-2">
              <p className="text-[10px] text-muted-foreground uppercase tracking-wider font-semibold">
                Parameters
              </p>
              <div className="rounded-lg border border-border overflow-hidden">
                {/* Table Header */}
                <div className="grid grid-cols-[2rem_1fr_4rem_4rem_1fr] gap-2 px-3 py-2 bg-muted/50 border-b border-border text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
                  <span />
                  <span>Name</span>
                  <span>In</span>
                  <span>Type</span>
                  <span>Value</span>
                </div>

                {/* Declared params from spec */}
                <div className="divide-y divide-border">
                  {(op?.operation?.parameters
                    ? (op.operation.parameters as OpenAPIV3.ParameterObject[])
                    : []
                  ).map((p) => {
                    const existing = current.params.find(
                      (param) => param.key === p.name,
                    );
                    const isChecked = !!existing && existing.enabled;
                    const schemaType =
                      typeof p.schema === "object" && !("$ref" in p.schema)
                        ? p.schema.type
                        : "";

                    const toggleParam = () => {
                      if (existing) {
                        setParams(
                          current.params.map((param) =>
                            param.key === p.name
                              ? { ...param, enabled: !param.enabled }
                              : param,
                          ),
                        );
                      } else {
                        setParams([
                          ...current.params,
                          { key: p.name, value: "", enabled: true },
                        ]);
                      }
                    };

                    const setParamValue = (value: string) => {
                      if (existing) {
                        setParams(
                          current.params.map((param) =>
                            param.key === p.name
                              ? { ...param, value }
                              : param,
                          ),
                        );
                      }
                    };

                    return (
                      <div
                        key={p.name}
                        className={`grid grid-cols-[2rem_1fr_4rem_4rem_1fr] gap-2 px-3 py-2 items-center text-xs transition-colors ${
                          isChecked ? "bg-primary/[0.02]" : "opacity-60"
                        }`}
                      >
                        <div className="flex justify-center">
                          <Checkbox
                            checked={isChecked}
                            onCheckedChange={toggleParam}
                          />
                        </div>

                        <div className="flex items-center gap-1.5 min-w-0">
                          <span className="font-mono text-foreground truncate">
                            {p.name}
                          </span>
                          {p.required && (
                            <Badge
                              variant="destructive"
                              className="text-[9px] h-4 px-1 shrink-0"
                            >
                              req
                            </Badge>
                          )}
                        </div>

                        <Badge
                          variant="outline"
                          className="text-[9px] h-5 px-1.5 justify-center w-fit"
                        >
                          {p.in}
                        </Badge>

                        <span className="text-muted-foreground font-mono text-[10px]">
                          {schemaType || "—"}
                        </span>

                        <div className="min-w-0 flex items-center gap-1">
                          {isChecked ? (
                            <>
                              <Input
                                value={existing?.value ?? ""}
                                onChange={(e) =>
                                  setParamValue(e.target.value)
                                }
                                placeholder="value"
                                className="h-7 text-xs font-mono rounded-md flex-1 min-w-0"
                              />
                              <Button
                                type="button"
                                variant="ghost"
                                size="icon-xs"
                                title="Remove from request"
                                onClick={() =>
                                  setParams(
                                    current.params.filter(
                                      (param) => param.key !== p.name,
                                    ),
                                  )
                                }
                                className="shrink-0 text-muted-foreground hover:text-destructive"
                              >
                                <Trash2 className="size-3.5" />
                              </Button>
                            </>
                          ) : (
                            <span className="text-muted-foreground/40 italic text-[10px]">
                              —
                            </span>
                          )}
                        </div>
                      </div>
                    );
                  })}
                </div>

                {/* Custom params */}
                {current.params.filter(
                  (p) =>
                    !(
                      op?.operation?.parameters as OpenAPIV3.ParameterObject[] | undefined
                    )?.some((declared) => declared.name === p.key),
                ).length > 0 && (
                  <div className="divide-y divide-border border-t border-border">
                    {current.params
                      .filter(
                        (p) =>
                          !(
                            op?.operation?.parameters as OpenAPIV3.ParameterObject[] | undefined
                          )?.some((declared) => declared.name === p.key),
                      )
                      .map((param) => {
                        const realIndex = current.params.indexOf(param);
                        return (
                          <div
                            key={realIndex}
                            className={`grid grid-cols-[2rem_1fr_4rem_4rem_1fr] gap-2 px-3 py-2 items-center text-xs transition-colors ${
                              param.enabled ? "bg-primary/[0.02]" : "opacity-60"
                            }`}
                          >
                            <div className="flex justify-center">
                              <Checkbox
                                checked={param.enabled}
                                onCheckedChange={(checked) =>
                                  setParams(
                                    current.params.map((p, i) =>
                                      i === realIndex
                                        ? { ...p, enabled: checked === true }
                                        : p,
                                    ),
                                  )
                                }
                              />
                            </div>

                            <Input
                              value={param.key}
                              onChange={(e) =>
                                setParams(
                                  current.params.map((p, i) =>
                                    i === realIndex
                                      ? { ...p, key: e.target.value }
                                      : p,
                                  ),
                                )
                              }
                              placeholder="name"
                              className="h-7 text-xs font-mono rounded-md"
                            />

                            <Badge
                              variant="outline"
                              className="text-[9px] h-5 px-1.5 justify-center w-fit"
                            >
                              query
                            </Badge>

                            <span className="text-muted-foreground font-mono text-[10px]">
                              string
                            </span>

                            <div className="min-w-0 flex items-center gap-1">
                              <Input
                                value={param.value}
                                onChange={(e) =>
                                  setParams(
                                    current.params.map((p, i) =>
                                      i === realIndex
                                        ? { ...p, value: e.target.value }
                                        : p,
                                    ),
                                  )
                                }
                                placeholder="value"
                                className="h-7 text-xs font-mono rounded-md flex-1 min-w-0"
                              />
                              <Button
                                type="button"
                                variant="ghost"
                                size="icon-xs"
                                title="Remove this custom parameter"
                                onClick={() =>
                                  setParams(
                                    current.params.filter(
                                      (_, i) => i !== realIndex,
                                    ),
                                  )
                                }
                                className="shrink-0 text-muted-foreground hover:text-destructive"
                              >
                                <Trash2 className="size-3.5" />
                              </Button>
                            </div>
                          </div>
                        );
                      })}
                  </div>
                )}
              </div>

              <Button
                type="button"
                variant="ghost"
                size="sm"
                onClick={() =>
                  setParams([
                    ...current.params,
                    { key: "", value: "", enabled: true },
                  ])
                }
                className="text-muted-foreground hover:text-foreground text-[10px] uppercase tracking-wider h-8"
              >
                + Add custom param
              </Button>
            </div>
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

        {activeTab === "schema" && (
          <div className="space-y-5">
            <section className="space-y-2">
              <div className="flex items-center justify-between gap-2">
                <div className="flex items-center gap-1.5">
                  <FileJson className="size-3.5 text-primary" />
                  <p className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
                    Request body
                  </p>
                </div>
                {requestBody?.name && (
                  <Badge
                    variant="outline"
                    className="h-5 rounded px-1.5 font-mono text-[10px]"
                  >
                    {requestBody.name}
                  </Badge>
                )}
              </div>
              {requestBody ? (
                <>
                  <p className="text-[10px] text-muted-foreground">
                    {requestBody.contentType ?? "application/json"}
                    {requestBody.required ? " · required" : " · optional"}
                  </p>
                  {requestBodyFields.length > 0 && (
                    <div className="flex flex-wrap items-center gap-1.5 rounded-md border border-border bg-muted/20 p-2">
                      <span className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground mr-1">
                        Autofill body
                      </span>
                      <Button
                        type="button"
                        variant="outline"
                        size="xs"
                        onClick={() => {
                          const skeleton = synthesizeJsonBody(
                            requestBodyFields,
                            { allFields: false },
                          );
                          setBodyType("json");
                          setBody(skeleton);
                          setActiveTab("body");
                        }}
                        className="h-6 px-2 font-mono text-[10px]"
                      >
                        Required only
                      </Button>
                      <Button
                        type="button"
                        variant="outline"
                        size="xs"
                        onClick={() => {
                          const skeleton = synthesizeJsonBody(
                            requestBodyFields,
                            { allFields: true },
                          );
                          setBodyType("json");
                          setBody(skeleton);
                          setActiveTab("body");
                        }}
                        className="h-6 px-2 font-mono text-[10px]"
                      >
                        All fields
                      </Button>
                      <span className="ml-auto text-[10px] text-muted-foreground/70">
                        Lands in the Body tab as JSON.
                      </span>
                    </div>
                  )}
                  <SchemaTable
                    fields={requestBodyFields}
                    emptyLabel="Request body has no declared properties."
                  />
                </>
              ) : (
                <p className="px-2 py-3 text-xs italic text-muted-foreground">
                  This operation does not declare a request body.
                </p>
              )}
            </section>

            <section className="space-y-2">
              <div className="flex items-center gap-1.5">
                <FileOutput className="size-3.5 text-primary" />
                <p className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
                  Responses
                </p>
              </div>
              {responses.length === 0 ? (
                <p className="px-2 py-3 text-xs italic text-muted-foreground">
                  No responses declared.
                </p>
              ) : (
                <div className="space-y-3">
                  {responses.map((entry, idx) => {
                    const fields = fieldInfosFromSchema(
                      entry.resolved,
                      spec,
                    );
                    const isError =
                      entry.status.startsWith("4") || entry.status.startsWith("5");
                    return (
                      <div
                        key={`${entry.status}-${entry.contentType ?? idx}`}
                        className="space-y-1.5 rounded-md border border-border bg-background p-2.5"
                      >
                        <div className="flex flex-wrap items-center gap-2">
                          <Badge
                            variant="outline"
                            className={`h-5 rounded px-1.5 font-mono text-[10px] font-bold ${
                              isError
                                ? "border-rose-500/30 text-rose-600"
                                : entry.status === "204"
                                  ? "border-muted-foreground/30 text-muted-foreground"
                                  : "border-emerald-500/30 text-emerald-600"
                            }`}
                          >
                            {entry.status}
                          </Badge>
                          {entry.description && (
                            <span className="text-[11px] text-foreground">
                              {entry.description}
                            </span>
                          )}
                          {entry.contentType && (
                            <Badge
                              variant="outline"
                              className="h-4 rounded px-1 font-mono text-[9px]"
                            >
                              {entry.contentType}
                            </Badge>
                          )}
                          {entry.resolvedName && (
                            <Badge
                              variant="outline"
                              className="h-4 rounded px-1 font-mono text-[9px]"
                            >
                              {entry.resolvedName}
                            </Badge>
                          )}
                        </div>
                        {entry.resolved && fields.length > 0 && (
                          <SchemaTable
                            fields={fields}
                            emptyLabel="Body has no declared properties."
                          />
                        )}
                        {entry.resolved &&
                          fields.length === 0 &&
                          entry.resolved.type === "object" && (
                            <p className="text-[10px] italic text-muted-foreground">
                              Object with no declared properties.
                            </p>
                          )}
                      </div>
                    );
                  })}
                </div>
              )}
            </section>
          </div>
        )}
      </div>
    </div>
  );
}
