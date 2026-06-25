import { useMemo, useState } from "react";
import type { OpenAPIV3 } from "openapi-types";
import { usePlayground } from "@/state/store";
import { MethodBadge } from "./MethodBadge";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Plus, Search } from "lucide-react";

interface OpenTabPopoverProps {
  /** The full parsed OpenAPI spec, or null while loading. */
  spec: OpenAPIV3.Document | null;
}

interface Candidate {
  operationId: string;
  method: string;
  path: string;
  summary?: string;
}

const METHODS = [
  ["GET", "get"],
  ["POST", "post"],
  ["PUT", "put"],
  ["PATCH", "patch"],
  ["DELETE", "delete"],
] as const;

/** Build a list of every operation in the spec, deduped by
 *  operationId. Mirrors the way `App.tsx`'s `collectOperations`
 *  walks the spec, so the popover shows the same endpoints the
 *  sidebar would. */
function collectCandidates(spec: OpenAPIV3.Document | null): Candidate[] {
  if (!spec) return [];
  const candidates: Candidate[] = [];
  for (const [path, pathItem] of Object.entries(spec.paths ?? {})) {
    if (!pathItem) continue;
    for (const [method, key] of METHODS) {
      const operation = pathItem[key];
      if (!operation) continue;
      candidates.push({
        operationId: operation.operationId ?? `${method} ${path}`,
        method,
        path,
        summary: operation.summary,
      });
    }
  }
  return candidates;
}

/** The "+" popover that lists operations not yet open. Filtering
 *  is by operationId (a candidate is hidden if a tab for its
 *  operationId is already in `openTabs`). A small search input
 *  narrows the list by path or operationId substring. */
export function OpenTabPopover({ spec }: OpenTabPopoverProps) {
  const openTab = usePlayground((s) => s.openTab);
  const openTabs = usePlayground((s) => s.openTabs);
  const [search, setSearch] = useState("");
  const [open, setOpen] = useState(false);

  const candidates = useMemo(() => {
    const all = collectCandidates(spec);
    const openIds = new Set(openTabs.map((t) => t.operationId));
    const remaining = all.filter((c) => !openIds.has(c.operationId));
    const q = search.trim().toLowerCase();
    if (!q) return remaining;
    return remaining.filter(
      (c) =>
        c.operationId.toLowerCase().includes(q) ||
        c.path.toLowerCase().includes(q) ||
        (c.summary ?? "").toLowerCase().includes(q),
    );
  }, [spec, openTabs, search]);

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button
          type="button"
          variant="ghost"
          size="icon-xs"
          className="shrink-0 text-muted-foreground hover:text-foreground"
          title="Open in new tab"
          aria-label="Open endpoint in new tab"
        >
          <Plus className="size-3.5" />
        </Button>
      </PopoverTrigger>
      <PopoverContent align="start" className="w-80 p-0">
        <div className="flex items-center gap-2 border-b border-border px-2.5 py-2">
          <Search className="size-3.5 text-muted-foreground" />
          <Input
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder="Search endpoints…"
            className="h-7 border-0 bg-transparent px-0 text-xs font-mono shadow-none focus-visible:ring-0"
            autoFocus
          />
        </div>
        <div className="max-h-72 overflow-y-auto p-1">
          {candidates.length === 0 ? (
            <p className="px-2.5 py-3 text-center text-[11px] italic text-muted-foreground">
              {openTabs.length === 0
                ? "No endpoints available — the spec is empty."
                : "Every endpoint in the spec is already open."}
            </p>
          ) : (
            candidates.map((c) => (
              <button
                key={c.operationId}
                type="button"
                onClick={() => {
                  openTab(c.operationId);
                  setOpen(false);
                  setSearch("");
                }}
                className="flex w-full items-start gap-2 rounded-sm px-2 py-1.5 text-left text-xs hover:bg-muted/60"
              >
                <MethodBadge method={c.method} />
                <span className="min-w-0 flex-1">
                  <span className="block truncate font-mono text-[11px] text-foreground">
                    {c.path}
                  </span>
                  {c.summary ? (
                    <span className="block truncate text-[10px] text-muted-foreground">
                      {c.summary}
                    </span>
                  ) : null}
                </span>
              </button>
            ))
          )}
        </div>
      </PopoverContent>
    </Popover>
  );
}
