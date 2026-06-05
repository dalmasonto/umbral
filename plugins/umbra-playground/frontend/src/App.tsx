import { useEffect, useMemo, useReducer } from "react";
import type { OpenAPIV3 } from "openapi-types";
import { usePlayground } from "@/state/store";
import { loadHistory } from "@/state/history";
import { loadTabs } from "@/state/tabsStorage";
import { useTheme } from "@/hooks/useTheme";
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarHeader,
  SidebarInset,
  SidebarProvider,
  SidebarTrigger,
} from "@/components/ui/sidebar";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { PasswordInput } from "@/components/ui/password-input";
import { Label } from "@/components/ui/label";
import { Separator } from "@/components/ui/separator";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetFooter,
  SheetHeader,
  SheetTitle,
  SheetTrigger,
} from "@/components/ui/sheet";
import { TooltipProvider } from "@/components/ui/tooltip";
import { EndpointTree } from "@/components/EndpointTree";
import { KeyValueEditor } from "@/components/KeyValueEditor";
import { RequestBuilder } from "@/components/RequestBuilder";
import { ResponseViewer } from "@/components/ResponseViewer";
import { SaveStatusIndicator } from "@/components/SaveStatusIndicator";
import { TabStrip } from "@/components/TabStrip";
import { Toaster } from "@/components/Toaster";
import {
  Activity,
  AlertCircle,
  CheckCircle2,
  Cookie,
  Database,
  Globe,
  History,
  Moon,
  RotateCcw,
  Settings2,
  SlidersHorizontal,
  Sun,
  Zap,
} from "lucide-react";

const METHODS = [
  ["GET", "get"],
  ["POST", "post"],
  ["PUT", "put"],
  ["PATCH", "patch"],
  ["DELETE", "delete"],
] as const;

interface OperationInfo {
  id: string;
  method: string;
  path: string;
  operation: OpenAPIV3.OperationObject;
}

function collectOperations(spec: OpenAPIV3.Document | null): OperationInfo[] {
  if (!spec) return [];
  const operations: OperationInfo[] = [];
  for (const [path, pathItem] of Object.entries(spec.paths ?? {})) {
    if (!pathItem) continue;
    for (const [method, key] of METHODS) {
      const operation = pathItem[key];
      if (!operation) continue;
      operations.push({
        id: operation.operationId ?? `${method} ${path}`,
        method,
        path,
        operation,
      });
    }
  }
  return operations;
}

function methodCounts(operations: OperationInfo[]): Array<[string, number]> {
  const counts: Array<[string, number]> = [];
  for (const [method] of METHODS) {
    const count = operations.filter((op) => op.method === method).length;
    if (count > 0) counts.push([method, count]);
  }
  return counts;
}

function responseTone(status?: number): string {
  if (!status) return "text-muted-foreground";
  if (status >= 200 && status < 300) return "text-emerald-600";
  if (status >= 300 && status < 400) return "text-amber-600";
  return "text-rose-600";
}

function SettingsSheetButton() {
  const settings = usePlayground((s) => s.settings);
  const saveStatus = usePlayground((s) => s.saveStatus);
  const lastSavedAt = usePlayground((s) => s.lastSavedAt);
  const setBaseUrl = usePlayground((s) => s.setBaseUrl);
  const setVariables = usePlayground((s) => s.setVariables);
  const setDefaultHeaders = usePlayground((s) => s.setDefaultHeaders);
  const setIncludeCredentials = usePlayground((s) => s.setIncludeCredentials);
  const setGlobalAuth = usePlayground((s) => s.setGlobalAuth);
  const applyDefaultHeaders = usePlayground((s) => s.applyDefaultHeaders);
  const resetSettings = usePlayground((s) => s.resetSettings);
  const saveSettingsNow = usePlayground((s) => s.saveSettingsNow);

  const variableCount = settings.variables.filter(
    (row) => row.enabled && row.key,
  ).length;
  const defaultHeaderCount = settings.defaultHeaders.filter(
    (row) => row.enabled && row.key,
  ).length;

  return (
    <Sheet>
      <SheetTrigger asChild>
        <Button
          type="button"
          variant="outline"
          size="sm"
          title="Workspace settings"
          className="size-8 gap-1.5 px-0 sm:size-auto sm:px-2.5"
        >
          <SlidersHorizontal className="size-3.5" />
          <span className="hidden sm:inline">Settings</span>
        </Button>
      </SheetTrigger>
      <SheetContent
        className="!inset-y-3 !right-3 !h-auto !w-[min(calc(100vw-1.5rem),640px)] gap-0 overflow-hidden rounded-2xl border border-border bg-popover p-0 shadow-2xl sm:!max-w-[640px]"
      >
        <SheetHeader className="border-b border-border px-7 py-5">
          <div className="flex items-center justify-between gap-3">
            <div className="flex items-center gap-2">
              <Settings2 className="size-4 text-primary" />
              <SheetTitle>Workspace settings</SheetTitle>
            </div>
            <SaveStatusBadge status={saveStatus} lastSavedAt={lastSavedAt} />
          </div>
          <SheetDescription>
            Variables, defaults, and request policy for this playground.
            Autosaved to your browser; click <strong>Save now</strong> at the
            bottom if you want to verify a write landed.
          </SheetDescription>
        </SheetHeader>

        <div className="flex-1 overflow-y-auto px-7 py-7">
          <div className="space-y-7">
            <section className="space-y-2">
              <div className="flex items-center justify-between gap-3">
                <Label htmlFor="base-url" className="text-xs font-semibold">
                  Base URL
                </Label>
                <Badge variant="outline" className="font-mono text-[10px]">
                  optional
                </Badge>
              </div>
              <Input
                id="base-url"
                value={settings.baseUrl}
                onChange={(event) => setBaseUrl(event.target.value)}
                placeholder="https://api.example.com"
                className="h-9 font-mono text-sm"
              />
            </section>

            <section className="space-y-2">
              <div className="flex items-center justify-between gap-3">
                <Label className="text-xs font-semibold">Variables</Label>
                <Badge variant="secondary" className="font-mono text-[10px]">
                  {variableCount} active
                </Badge>
              </div>
              <div className="rounded-md border border-border bg-muted/25 p-2">
                <KeyValueEditor
                  rows={settings.variables}
                  onChange={setVariables}
                  keyPlaceholder="name"
                  valuePlaceholder="value"
                  maskValues
                />
              </div>
              <p className="text-[11px] text-muted-foreground">
                Use <code className="font-mono text-foreground">{"{{name}}"}</code>{" "}
                in URLs, params, headers, auth, or request bodies. Values are
                masked by default — click the eye icon to reveal.
              </p>
            </section>

            <section className="space-y-2">
              <div className="flex items-center justify-between gap-3">
                <Label className="text-xs font-semibold">Default headers</Label>
                <Badge variant="secondary" className="font-mono text-[10px]">
                  {defaultHeaderCount} active
                </Badge>
              </div>
              <div className="rounded-md border border-border bg-muted/25 p-2">
                <KeyValueEditor
                  rows={settings.defaultHeaders}
                  onChange={setDefaultHeaders}
                  keyPlaceholder="Header"
                  valuePlaceholder="Value"
                />
              </div>
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={applyDefaultHeaders}
              >
                Apply to current request
              </Button>
            </section>

            <section className="space-y-2">
              <div className="flex items-center justify-between gap-3">
                <Label className="text-xs font-semibold">
                  Global authorization
                </Label>
                <Badge
                  variant={settings.globalAuth.enabled ? "secondary" : "outline"}
                  className="font-mono text-[10px]"
                >
                  {settings.globalAuth.enabled ? "on" : "off"}
                </Badge>
              </div>
              <div className="flex items-start gap-3 rounded-md border border-border bg-muted/25 p-3">
                <Checkbox
                  id="global-auth-enabled"
                  checked={settings.globalAuth.enabled}
                  onCheckedChange={(checked) =>
                    setGlobalAuth({ enabled: checked === true })
                  }
                  className="mt-2"
                />
                <div className="flex-1 space-y-2">
                  <div className="flex items-center gap-2">
                    <Input
                      value={settings.globalAuth.scheme}
                      onChange={(event) =>
                        setGlobalAuth({ scheme: event.target.value })
                      }
                      placeholder="Bearer"
                      className="w-28 font-mono text-sm h-9"
                    />
                    <PasswordInput
                      value={settings.globalAuth.token}
                      onChange={(event) =>
                        setGlobalAuth({ token: event.target.value })
                      }
                      placeholder="token"
                      className="font-mono text-sm h-9"
                      wrapperClassName="flex-1"
                    />
                  </div>
                  <p className="text-[11px] leading-relaxed text-muted-foreground">
                    Sent as{" "}
                    <code className="font-mono text-foreground">
                      Authorization: {settings.globalAuth.scheme || "<scheme>"}{" "}
                      {settings.globalAuth.token ? "<token>" : "<empty>"}
                    </code>{" "}
                    on every request that doesn't set its own. Per-request
                    auth always wins.
                  </p>
                </div>
              </div>
            </section>

            <section className="flex items-start gap-3 rounded-md border border-border bg-muted/25 p-3">
              <Checkbox
                id="include-credentials"
                checked={settings.includeCredentials}
                onCheckedChange={(checked) =>
                  setIncludeCredentials(checked === true)
                }
                className="mt-0.5"
              />
              <div className="space-y-1">
                <Label
                  htmlFor="include-credentials"
                  className="flex items-center gap-2 text-xs font-semibold"
                >
                  <Cookie className="size-3.5 text-muted-foreground" />
                  Include credentials
                </Label>
                <p className="text-[11px] leading-relaxed text-muted-foreground">
                  Sends cookies with playground requests.
                </p>
              </div>
            </section>
          </div>
        </div>

        <SheetFooter className="flex flex-row items-center justify-between border-t border-border px-7 py-4">
          <Button
            type="button"
            variant="ghost"
            size="sm"
            onClick={resetSettings}
            className="text-muted-foreground hover:text-foreground"
          >
            Reset settings
          </Button>
          <Button
            type="button"
            variant={saveStatus === "dirty" ? "destructive" : "default"}
            size="sm"
            onClick={() => {
              void saveSettingsNow();
            }}
          >
            {saveStatus === "dirty" ? "Retry save" : "Save now"}
          </Button>
        </SheetFooter>
      </SheetContent>
    </Sheet>
  );
}

/** Visible save indicator next to the settings sheet title.
 *  - "saving" — pulse + grey badge during the autosave burst.
 *  - "saved"  — green check + "Saved" (or "Saved <n>s ago").
 *  - "dirty"  — red alert + "Unsaved" so the user knows the last
 *               write attempt didn't actually land in localStorage. */
function SaveStatusBadge({
  status,
  lastSavedAt,
}: {
  status: "saved" | "saving" | "dirty";
  lastSavedAt: number | null;
}) {
  // Re-render every 5s so the "Saved <n>s ago" text creeps without
  // demanding an upstream tick.
  const [, force] = useReducer((x: number) => x + 1, 0);
  useEffect(() => {
    if (status !== "saved" || lastSavedAt === null) return;
    const handle = setInterval(force, 5000);
    return () => clearInterval(handle);
  }, [status, lastSavedAt]);

  if (status === "saving") {
    return (
      <Badge variant="outline" className="gap-1.5">
        <span className="size-2 animate-pulse rounded-full bg-amber-500" />
        Saving…
      </Badge>
    );
  }
  if (status === "dirty") {
    return (
      <Badge variant="destructive" className="gap-1.5">
        <AlertCircle className="size-3" />
        Unsaved
      </Badge>
    );
  }
  return (
    <Badge variant="secondary" className="gap-1.5 text-emerald-600 dark:text-emerald-400">
      <CheckCircle2 className="size-3" />
      {lastSavedAt === null ? "Saved" : `Saved ${formatAgo(lastSavedAt)}`}
    </Badge>
  );
}

function formatAgo(timestamp: number): string {
  const seconds = Math.max(0, Math.floor((Date.now() - timestamp) / 1000));
  if (seconds < 5) return "just now";
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  return `${hours}h ago`;
}

export function App() {
  const spec = usePlayground((s) => s.spec);
  const specError = usePlayground((s) => s.specError);
  const loadingSpec = usePlayground((s) => s.loadingSpec);
  const loadSpec = usePlayground((s) => s.loadSpec);
  const selectedOperationId = usePlayground((s) => s.selectedOperationId);
  const settings = usePlayground((s) => s.settings);
  const history = usePlayground((s) => s.history);
  const lastResponse = usePlayground((s) => s.lastResponse);
  const [theme, toggleTheme] = useTheme();

  useEffect(() => {
    void loadSpec();
  }, [loadSpec]);

  useEffect(() => {
    let active = true;
    void loadHistory().then((history) => {
      if (active) usePlayground.setState({ history });
    });
    return () => {
      active = false;
    };
  }, []);

  // After the localStorage boot cache renders, async-load the
  // authoritative settings out of Dexie. Replaces in-memory state
  // when another tab — or a stale cache — caused a divergence.
  // Single shot; Dexie's tab-sync would go here later if we want it.
  useEffect(() => {
    void usePlayground.getState().hydrateFromDexie();
  }, []);

  // Cmd/Ctrl+S — manual settings save. Bypasses the silent
  // auto-save path and fires a toast so the user gets explicit
  // confirmation. The browser's "Save Page As" dialog is
  // suppressed via preventDefault. Suppressed when focus is
  // inside an editable element so a user mid-typing in a
  // header value can still use the OS-level Save shortcut
  // … actually, no — they probably want OUR save, not the
  // browser's. We swallow the event unconditionally when the
  // modifier is held.
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey)) return;
      if (e.key.toLowerCase() !== "s") return;
      e.preventDefault();
      void usePlayground.getState().saveSettingsNow();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, []);

  // Restore the open tab list from Dexie once after mount.
  // If a snapshot is present, set `openTabs` and pick the
  // first valid active id (or the first tab when the persisted
  // active is gone). The store's openTab/setActiveTab actions
  // are reused so the rest of the app (RequestBuilder,
  // ResponseViewer) hydrates through the existing selectEndpoint
  // path.
  useEffect(() => {
    let active = true;
    void loadTabs().then((snapshot) => {
      if (!active) return;
      if (!snapshot) return;
      const { tabs, activeTabId } = snapshot;
      if (tabs.length === 0) return;
      const target = tabs.find((t) => t.id === activeTabId) ?? tabs[0]!;
      usePlayground.setState({ openTabs: tabs });
      usePlayground.getState().setActiveTab(target.id);
    });
    return () => {
      active = false;
    };
  }, []);

  const operations = useMemo(() => collectOperations(spec), [spec]);
  const selectedOperation = useMemo(
    () => operations.find((operation) => operation.id === selectedOperationId),
    [operations, selectedOperationId],
  );
  const countsByMethod = useMemo(() => methodCounts(operations), [operations]);
  const historyCount = useMemo(
    () =>
      Object.values(history).reduce(
        (total, records) => total + records.length,
        0,
      ),
    [history],
  );
  const variableCount = settings.variables.filter(
    (row) => row.enabled && row.key,
  ).length;
  const defaultHeaderCount = settings.defaultHeaders.filter(
    (row) => row.enabled && row.key,
  ).length;
  const specStatus = loadingSpec
    ? "Loading"
    : specError
      ? "Spec issue"
      : spec
        ? "Ready"
        : "No spec";

  return (
    <TooltipProvider delayDuration={100}>
      <SidebarProvider>
        <Sidebar
          collapsible="offcanvas"
          className="border-r border-sidebar-border bg-sidebar"
        >
          <SidebarHeader className="h-[60px] min-h-[60px] shrink-0 justify-center px-4 py-0">
            <div className="flex items-center gap-3">
              <div className="flex size-8 items-center justify-center rounded-md border border-sidebar-border bg-background">
                <Zap className="size-4 text-primary" />
              </div>
              <div className="min-w-0 flex-1">
                <span className="block truncate text-sm font-semibold leading-tight tracking-tight">
                  Umbra Playground
                </span>
                <span className="block truncate text-[11px] leading-tight text-muted-foreground">
                  {spec?.info?.title ?? "API Explorer"}
                  {spec?.info?.version ? (
                    <span className="text-muted-foreground/70">
                      {" "}
                      v{spec.info.version}
                    </span>
                  ) : null}
                </span>
              </div>
            </div>
          </SidebarHeader>

          <div className="border-y border-sidebar-border px-4 py-3">
            <div className="mb-2 flex items-center justify-between text-[11px]">
              <span className="font-medium text-muted-foreground">Methods</span>
              <span className="font-mono text-muted-foreground">
                {operations.length} ops
              </span>
            </div>
            <div className="flex flex-wrap gap-1.5">
              {countsByMethod.length > 0 ? (
                countsByMethod.map(([method, count]) => (
                  <Badge
                    key={method}
                    variant="outline"
                    className="h-5 gap-1 rounded-md px-1.5 font-mono text-[10px]"
                  >
                    {method}
                    <span className="text-muted-foreground">{count}</span>
                  </Badge>
                ))
              ) : (
                <span className="text-[11px] text-muted-foreground">
                  Waiting for spec
                </span>
              )}
            </div>
          </div>

          {/* `overflow-hidden` overrides the shadcn SidebarContent
              default (`overflow-auto`) so the EndpointTree's
              ScrollArea is the only scroller. Without this override,
              the SidebarContent's invisible native scroll wins and
              the ScrollArea grows to fit content (no scrollbar). */}
          <SidebarContent className="p-0 overflow-hidden">
            <EndpointTree />
          </SidebarContent>

          <SidebarFooter className="border-t border-sidebar-border p-3">
            <div className="space-y-2 text-[11px]">
              <div className="flex items-center justify-between gap-3">
                <span className="text-muted-foreground">Spec</span>
                <span className="flex items-center gap-1.5 font-medium">
                  <CheckCircle2 className="size-3 text-emerald-600" />
                  {specStatus}
                </span>
              </div>
              <div className="flex items-center justify-between gap-3">
                <span className="text-muted-foreground">Paths</span>
                <span className="font-mono">
                  {Object.keys(spec?.paths ?? {}).length}
                </span>
              </div>
            </div>
            <div className="mt-3 flex items-center justify-between gap-2">
              <Button
                variant="outline"
                size="sm"
                onClick={() => void loadSpec()}
                className="flex-1 justify-start text-xs"
              >
                <RotateCcw className="size-3.5" />
                Reload spec
              </Button>
              <Button
                variant="ghost"
                size="icon-xs"
                onClick={() => void loadSpec()}
                title="Reload spec"
                className="text-muted-foreground hover:text-foreground sm:hidden"
              >
                <RotateCcw className="size-3" />
              </Button>
            </div>
          </SidebarFooter>
        </Sidebar>

        <SidebarInset className="min-w-0 bg-background">
          <header className="flex h-[60px] min-h-[60px] shrink-0 items-center gap-3 border-b border-border bg-card/80 px-4">
            <SidebarTrigger />
            <div className="sm:hidden">
              <SettingsSheetButton />
            </div>
            <Separator orientation="vertical" className="hidden h-5 sm:block" />
            <div className="min-w-0 flex-1">
              <div className="flex min-w-0 items-center gap-2">
                <h1 className="truncate text-sm font-semibold">
                  {selectedOperation?.operation.summary ??
                    selectedOperation?.operation.operationId ??
                    spec?.info?.title ??
                    "Playground"}
                </h1>
                {selectedOperation ? (
                  <Badge
                    variant="outline"
                    className="h-5 shrink-0 rounded-md px-1.5 font-mono text-[10px]"
                  >
                    {selectedOperation.method}
                  </Badge>
                ) : null}
              </div>
              <p className="truncate text-[11px] text-muted-foreground">
                {selectedOperation
                  ? selectedOperation.path
                  : settings.baseUrl || "Select an endpoint to build a request"}
              </p>
            </div>

            {specError && (
              <span className="hidden max-w-[24rem] truncate font-mono text-xs text-destructive md:block">
                {specError}
              </span>
            )}
            <div className="hidden items-center gap-2 lg:flex">
              <Badge variant="secondary" className="gap-1.5 text-[11px]">
                <Database className="size-3" />
                {variableCount} vars
              </Badge>
              <Badge variant="secondary" className="gap-1.5 text-[11px]">
                <Globe className="size-3" />
                {defaultHeaderCount} headers
              </Badge>
            </div>
            <div className="hidden sm:block">
              <SaveStatusIndicator />
            </div>
            <div className="hidden sm:block">
              <SettingsSheetButton />
            </div>
            <Button
              type="button"
              variant="ghost"
              size="icon-sm"
              onClick={toggleTheme}
              title={`Switch to ${theme === "dark" ? "light" : "dark"} mode`}
              className="hidden shrink-0 text-muted-foreground hover:text-foreground sm:inline-flex"
            >
              {theme === "dark" ? (
                <Sun className="size-4" />
              ) : (
                <Moon className="size-4" />
              )}
            </Button>
          </header>

          <div className="grid min-h-0 flex-1 grid-rows-[auto_auto_minmax(0,1fr)] overflow-hidden">
            <section className="border-b border-border bg-muted/25 px-4 py-3">
              <div className="grid gap-3 md:grid-cols-4">
                <div className="flex items-center gap-2">
                  <Activity className="size-4 text-muted-foreground" />
                  <div className="min-w-0">
                    <p className="text-[10px] font-semibold uppercase text-muted-foreground">
                      Spec
                    </p>
                    <p className="truncate text-xs font-medium">
                      {specStatus}
                    </p>
                  </div>
                </div>
                <div className="flex items-center gap-2">
                  <Database className="size-4 text-muted-foreground" />
                  <div className="min-w-0">
                    <p className="text-[10px] font-semibold uppercase text-muted-foreground">
                      Operations
                    </p>
                    <p className="truncate font-mono text-xs">
                      {operations.length}
                    </p>
                  </div>
                </div>
                <div className="flex items-center gap-2">
                  <History className="size-4 text-muted-foreground" />
                  <div className="min-w-0">
                    <p className="text-[10px] font-semibold uppercase text-muted-foreground">
                      History
                    </p>
                    <p className="truncate font-mono text-xs">
                      {historyCount} requests
                    </p>
                  </div>
                </div>
                <div className="flex items-center gap-2">
                  <CheckCircle2
                    className={`size-4 ${responseTone(lastResponse?.status)}`}
                  />
                  <div className="min-w-0">
                    <p className="text-[10px] font-semibold uppercase text-muted-foreground">
                      Last response
                    </p>
                    <p className="truncate font-mono text-xs">
                      {lastResponse
                        ? `${lastResponse.status || "ERR"} ${lastResponse.durationMs}ms`
                        : "None"}
                    </p>
                  </div>
                </div>
              </div>
            </section>

            <TabStrip spec={spec} />

            <div className="grid min-h-0 grid-cols-1 lg:grid-cols-2 min-h-[640px] lg:min-h-[720px]">
              <section className="flex min-h-0 flex-col overflow-hidden border-b border-border lg:border-b-0 lg:border-r">
                <RequestBuilder />
              </section>
              <section className="flex min-h-0 flex-col overflow-hidden">
                <ResponseViewer />
              </section>
            </div>
          </div>
        </SidebarInset>
      </SidebarProvider>
      <Toaster />
    </TooltipProvider>
  );
}

export default App;
