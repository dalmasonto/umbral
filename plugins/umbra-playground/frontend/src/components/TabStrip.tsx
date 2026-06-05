import { useEffect, useMemo } from "react";
import type { OpenAPIV3 } from "openapi-types";
import { usePlayground, type Tab } from "@/state/store";
import { OpenTabPopover } from "./OpenTabPopover";
import { ExportImportControls } from "./ExportImportControls";
import { cn } from "@/lib/utils";
import { X } from "lucide-react";

interface TabStripProps {
  /** The full parsed OpenAPI spec, or null while loading.
   *  Passed through to the OpenTabPopover. */
  spec: OpenAPIV3.Document | null;
}

const METHODS = [
  ["GET", "get"],
  ["POST", "post"],
  ["PUT", "put"],
  ["PATCH", "patch"],
  ["DELETE", "delete"],
] as const;

interface OperationLookup {
  method: string;
  path: string;
}

function buildOperationLookup(
  spec: OpenAPIV3.Document | null,
): Map<string, OperationLookup> {
  const map = new Map<string, OperationLookup>();
  if (!spec) return map;
  for (const [path, pathItem] of Object.entries(spec.paths ?? {})) {
    if (!pathItem) continue;
    for (const [method, key] of METHODS) {
      const operation = pathItem[key];
      if (!operation) continue;
      const id = operation.operationId ?? `${method} ${path}`;
      map.set(id, { method, path });
    }
  }
  return map;
}

function isDraftDirty(
  tab: Tab,
  selectedOperationId: string | null,
  current: ReturnType<typeof usePlayground.getState>["current"],
): boolean {
  if (tab.operationId !== selectedOperationId) {
    // Dirty check is only meaningful for the active tab; we
    // don't track dirtiness for inactive tabs.
    return false;
  }
  return JSON.stringify(tab.pristineDraft) !== JSON.stringify(current);
}

/** The tab strip — sits above the request/response panels.
 *  Renders one pill per open tab, a + popover, and the
 *  export/import controls. Captures Cmd/Ctrl+W to close the
 *  active tab. */
export function TabStrip({ spec }: TabStripProps) {
  const openTabs = usePlayground((s) => s.openTabs);
  const activeTabId = usePlayground((s) => s.activeTabId);
  const current = usePlayground((s) => s.current);
  const selectedOperationId = usePlayground((s) => s.selectedOperationId);
  const setActiveTab = usePlayground((s) => s.setActiveTab);
  const closeTab = usePlayground((s) => s.closeTab);

  const lookup = useMemo(() => buildOperationLookup(spec), [spec]);

  // Keyboard shortcut: Cmd/Ctrl+W closes the active tab.
  // Suppressed when focus is inside an editable element.
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey)) return;
      if (e.key.toLowerCase() !== "w") return;
      const target = e.target as HTMLElement | null;
      if (target) {
        const tag = target.tagName;
        if (
          tag === "INPUT" ||
          tag === "TEXTAREA" ||
          target.isContentEditable
        ) {
          return;
        }
      }
      if (!activeTabId) return;
      e.preventDefault();
      closeTab(activeTabId);
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [activeTabId, closeTab]);

  if (openTabs.length === 0) {
    return (
      <div className="flex h-10 shrink-0 items-center gap-2 border-b border-border bg-muted/30 px-2">
        <span className="text-[11px] italic text-muted-foreground">
          No tabs open — pick an endpoint from the sidebar to start a request.
        </span>
        <span className="flex-1" />
        <ExportImportControls />
      </div>
    );
  }

  return (
    <div className="flex h-10 shrink-0 items-center gap-1 border-b border-border bg-muted/30 px-2">
      <div className="flex min-w-0 flex-1 items-center gap-1 overflow-x-auto">
        {openTabs.map((tab) => {
          const info = lookup.get(tab.operationId);
          const isActive = tab.id === activeTabId;
          const dirty = isActive && isDraftDirty(tab, selectedOperationId, current);
          return (
            <div
              key={tab.id}
              role="tab"
              aria-selected={isActive}
              className={cn(
                "group inline-flex shrink-0 items-center gap-2 h-7 rounded-md border px-2 text-[11px] font-mono whitespace-nowrap transition-colors",
                isActive
                  ? "bg-background border-border text-foreground shadow-sm"
                  : "bg-muted/50 border-transparent text-muted-foreground hover:text-foreground hover:bg-muted",
              )}
            >
              <button
                type="button"
                onClick={() => setActiveTab(tab.id)}
                className="flex min-w-0 items-center gap-2"
                title={`${info?.method ?? "?"} ${info?.path ?? tab.operationId}`}
              >
                <span className="font-semibold uppercase tracking-wider text-[9px] text-muted-foreground">
                  {info?.method ?? "?"}
                </span>
                <span className="truncate max-w-[180px]">
                  {info?.path ?? tab.operationId}
                </span>
                {dirty ? (
                  <span
                    className="size-1.5 shrink-0 rounded-full bg-amber-500"
                    aria-label="unsaved edits"
                    title="This tab has edits not present at open time"
                  />
                ) : null}
              </button>
              <button
                type="button"
                onClick={(e) => {
                  e.stopPropagation();
                  closeTab(tab.id);
                }}
                className="ml-0.5 -mr-1 grid size-4 shrink-0 place-items-center rounded text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
                title="Close tab"
                aria-label={`Close ${info?.path ?? tab.operationId}`}
              >
                <X className="size-3" />
              </button>
            </div>
          );
        })}
        <OpenTabPopover spec={spec} />
      </div>
      <span className="flex-1" />
      <ExportImportControls />
    </div>
  );
}
