import { useMemo, useState } from "react";
import { usePlayground } from "@/state/store";
import type { OpenAPIV3 } from "openapi-types";
import { Input } from "@/components/ui/input";
import { Separator } from "@/components/ui/separator";
import { Skeleton } from "@/components/ui/skeleton";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
import { MethodBadge } from "./MethodBadge";
import { Search, ChevronRight, FileCode2 } from "lucide-react";

interface TreeEntry {
  operationId: string;
  method: string;
  path: string;
  summary?: string;
  tag: string;
}

function extractOperations(spec: OpenAPIV3.Document): TreeEntry[] {
  const out: TreeEntry[] = [];
  for (const [path, pathItem] of Object.entries(spec.paths ?? {})) {
    if (!pathItem) continue;
    const methods: Array<[string, OpenAPIV3.OperationObject | undefined]> = [
      ["GET", pathItem.get],
      ["POST", pathItem.post],
      ["PUT", pathItem.put],
      ["PATCH", pathItem.patch],
      ["DELETE", pathItem.delete],
    ];
    for (const [method, op] of methods) {
      if (!op) continue;
      const id = op.operationId ?? `${method} ${path}`;
      const tag = op.tags?.[0] ?? "default";
      out.push({ operationId: id, method, path, summary: op.summary, tag });
    }
  }
  return out;
}

function groupByTag(entries: TreeEntry[]): Map<string, TreeEntry[]> {
  const map = new Map<string, TreeEntry[]>();
  for (const e of entries) {
    const list = map.get(e.tag) ?? [];
    list.push(e);
    map.set(e.tag, list);
  }
  return map;
}

export function EndpointTree() {
  const spec = usePlayground((s) => s.spec);
  const loadingSpec = usePlayground((s) => s.loadingSpec);
  const specError = usePlayground((s) => s.specError);
  const selected = usePlayground((s) => s.selectedOperationId);
  const select = usePlayground((s) => s.selectEndpoint);
  const [search, setSearch] = useState("");

  const grouped = useMemo(() => {
    if (!spec) return null;
    const all = extractOperations(spec);
    const q = search.trim().toLowerCase();
    const filtered = q
      ? all.filter(
          (e) =>
            e.path.toLowerCase().includes(q) ||
            e.method.toLowerCase().includes(q) ||
            (e.summary?.toLowerCase().includes(q) ?? false) ||
            e.tag.toLowerCase().includes(q),
        )
      : all;
    const map = groupByTag(filtered);
    return Array.from(map.entries()).sort(([a], [b]) => a.localeCompare(b));
  }, [spec, search]);

  if (loadingSpec) {
    return (
      <div className="p-3 space-y-3">
        <Skeleton className="h-7 w-full" />
        {Array.from({ length: 6 }).map((_, i) => (
          <div key={i} className="space-y-2">
            <Skeleton className="h-4 w-24" />
            <Skeleton className="h-6 w-full" />
            <Skeleton className="h-6 w-full" />
          </div>
        ))}
      </div>
    );
  }

  if (specError) {
    return (
      <div className="p-4 text-center">
        <FileCode2 className="size-8 text-muted-foreground mx-auto mb-2 opacity-50" />
        <p className="text-xs text-muted-foreground">Could not load OpenAPI spec.</p>
        <p className="text-[10px] text-muted-foreground/60 mt-1">{specError}</p>
      </div>
    );
  }

  if (!spec || !grouped) {
    return (
      <div className="p-4 text-center">
        <FileCode2 className="size-8 text-muted-foreground mx-auto mb-2 opacity-50" />
        <p className="text-xs text-muted-foreground">No spec loaded.</p>
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full">
      <div className="p-2">
        <div className="relative">
          <Search className="absolute left-2 top-1/2 -translate-y-1/2 size-3.5 text-muted-foreground" />
          <Input
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder="Search endpoints…"
            className="pl-7 h-7 text-xs"
          />
        </div>
      </div>
      <Separator />
      <div className="flex-1 overflow-y-auto p-1 space-y-0.5">
        {grouped.length === 0 && (
          <div className="p-4 text-center text-xs text-muted-foreground">
            No endpoints match your search.
          </div>
        )}
        {grouped.map(([tag, entries]) => (
          <Collapsible key={tag} defaultOpen={search.length > 0}>
            <CollapsibleTrigger asChild>
              <button
                type="button"
                className="w-full flex items-center gap-1 px-2 py-1 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground hover:text-foreground transition-colors select-none"
              >
                <ChevronRight className="size-3 transition-transform data-[state=open]:rotate-90" />
                <span className="flex-1 text-left">{tag}</span>
                <span className="text-[9px] text-muted-foreground/60">{entries.length}</span>
              </button>
            </CollapsibleTrigger>
            <CollapsibleContent>
              <ul className="space-y-0.5">
                {entries.map((e) => (
                  <li key={e.operationId}>
                    <Tooltip>
                      <TooltipTrigger asChild>
                        <button
                          type="button"
                          onClick={() => select(e.operationId)}
                          className={`w-full text-left px-2 py-1.5 rounded-md text-xs flex items-center gap-2 transition-colors ${
                            selected === e.operationId
                              ? "bg-primary/10 text-primary border border-primary/20"
                              : "hover:bg-muted text-muted-foreground hover:text-foreground border border-transparent"
                          }`}
                        >
                          <MethodBadge method={e.method} />
                          <span className="font-mono truncate flex-1">{e.path}</span>
                        </button>
                      </TooltipTrigger>
                      {e.summary && (
                        <TooltipContent side="right" className="max-w-xs">
                          <p className="font-medium">{e.summary}</p>
                          <p className="text-[10px] text-muted-foreground mt-0.5 font-mono">{e.method} {e.path}</p>
                        </TooltipContent>
                      )}
                    </Tooltip>
                  </li>
                ))}
              </ul>
            </CollapsibleContent>
          </Collapsible>
        ))}
      </div>
    </div>
  );
}
