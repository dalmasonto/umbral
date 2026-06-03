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
import {
  Search,
  ChevronRight,
  FileCode2,
  Shield,
  User,
  MessageSquare,
  Settings,
  Box,
} from "lucide-react";

interface TreeEntry {
  operationId: string;
  method: string;
  path: string;
  summary?: string;
  tag: string;
}

const TAG_ICONS: Record<string, React.ReactNode> = {
  Authentication: <Shield className="size-3.5" />,
  Auth: <Shield className="size-3.5" />,
  Users: <User className="size-3.5" />,
  User: <User className="size-3.5" />,
  Posts: <MessageSquare className="size-3.5" />,
  Post: <MessageSquare className="size-3.5" />,
  Comments: <MessageSquare className="size-3.5" />,
  Comment: <MessageSquare className="size-3.5" />,
  Articles: <MessageSquare className="size-3.5" />,
  Article: <MessageSquare className="size-3.5" />,
  Blog: <MessageSquare className="size-3.5" />,
  Settings: <Settings className="size-3.5" />,
  Config: <Settings className="size-3.5" />,
  System: <Settings className="size-3.5" />,
  Admin: <Settings className="size-3.5" />,
  default: <Box className="size-3.5" />,
};

const TAG_COLORS: Record<string, string> = {
  Authentication: "border-amber-500/40 text-amber-600",
  Auth: "border-amber-500/40 text-amber-600",
  Users: "border-sky-500/40 text-sky-600",
  User: "border-sky-500/40 text-sky-600",
  Posts: "border-emerald-500/40 text-emerald-600",
  Post: "border-emerald-500/40 text-emerald-600",
  Comments: "border-violet-500/40 text-violet-600",
  Comment: "border-violet-500/40 text-violet-600",
  Articles: "border-violet-500/40 text-violet-600",
  Article: "border-violet-500/40 text-violet-600",
  Blog: "border-violet-500/40 text-violet-600",
  Settings: "border-rose-500/40 text-rose-600",
  Config: "border-rose-500/40 text-rose-600",
  System: "border-rose-500/40 text-rose-600",
  Admin: "border-rose-500/40 text-rose-600",
};

function getTagIcon(tag: string): React.ReactNode {
  return TAG_ICONS[tag] ?? TAG_ICONS.default;
}

function getTagColor(tag: string): string {
  return TAG_COLORS[tag] ?? "border-muted-foreground/30 text-muted-foreground";
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
      <div className="p-3 space-y-4">
        <Skeleton className="h-8 w-full rounded-lg" />
        {Array.from({ length: 4 }).map((_, i) => (
          <div key={i} className="space-y-2">
            <Skeleton className="h-5 w-28 rounded-md" />
            <Skeleton className="h-8 w-full rounded-lg" />
            <Skeleton className="h-8 w-full rounded-lg" />
          </div>
        ))}
      </div>
    );
  }

  if (specError) {
    return (
      <div className="flex flex-col items-center justify-center h-full p-6 text-center gap-3">
        <div className="flex items-center justify-center size-12 rounded-full bg-muted">
          <FileCode2 className="size-6 text-muted-foreground" />
        </div>
        <div className="space-y-1">
          <p className="text-sm font-medium text-foreground">Could not load spec</p>
          <p className="text-xs text-muted-foreground">{specError}</p>
        </div>
      </div>
    );
  }

  if (!spec || !grouped) {
    return (
      <div className="flex flex-col items-center justify-center h-full p-6 text-center gap-3">
        <div className="flex items-center justify-center size-12 rounded-full bg-muted">
          <FileCode2 className="size-6 text-muted-foreground" />
        </div>
        <p className="text-sm font-medium text-foreground">No spec loaded</p>
        <p className="text-xs text-muted-foreground">Select an endpoint to begin.</p>
      </div>
    );
  }

  const totalEndpoints = grouped.reduce((sum, [, entries]) => sum + entries.length, 0);

  return (
    <div className="flex flex-col h-full">
      {/* Search */}
      <div className="p-3">
        <div className="relative">
          <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 size-3.5 text-muted-foreground" />
          <Input
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder="Search endpoints…"
            className="pl-8 h-8 text-xs rounded-lg bg-muted/50 border-transparent focus-visible:bg-background focus-visible:border-input"
          />
          {search && (
            <span className="absolute right-2.5 top-1/2 -translate-y-1/2 text-[10px] text-muted-foreground font-medium">
              {totalEndpoints}
            </span>
          )}
        </div>
      </div>

      <Separator className="mx-3 w-auto" />

      {/* Endpoint Groups */}
      <div className="flex-1 overflow-y-auto p-2 space-y-3">
        {grouped.length === 0 && (
          <div className="py-8 text-center">
            <p className="text-xs text-muted-foreground">No endpoints match your search.</p>
          </div>
        )}

        {grouped.map(([tag, entries]) => {
          const tagColor = getTagColor(tag);
          const tagIcon = getTagIcon(tag);
          return (
            <Collapsible key={tag} defaultOpen={search.length > 0}>
              <CollapsibleTrigger asChild>
                <button
                  type="button"
                  className="w-full flex items-center gap-2 px-2 py-1.5 rounded-md text-xs font-semibold text-foreground hover:bg-muted/80 transition-colors select-none group"
                >
                  <ChevronRight className="size-3.5 text-muted-foreground transition-transform duration-150 group-data-[state=open]:rotate-90" />
                  <span className={`flex items-center justify-center size-5 rounded border ${tagColor}`}>
                    {tagIcon}
                  </span>
                  <span className="flex-1 text-left">{tag}</span>
                  <span className="text-[10px] font-medium text-muted-foreground bg-muted px-1.5 py-0.5 rounded-full">
                    {entries.length}
                  </span>
                </button>
              </CollapsibleTrigger>

              <CollapsibleContent>
                <ul className="mt-1 space-y-0.5 pl-4 border-l border-border ml-4">
                  {entries.map((e) => (
                    <li key={e.operationId}>
                      <Tooltip>
                        <TooltipTrigger asChild>
                          <button
                            type="button"
                            onClick={() => select(e.operationId)}
                            className={`w-full text-left px-3 py-2 rounded-lg text-xs flex items-center gap-2.5 transition-all ${
                              selected === e.operationId
                                ? "bg-primary/8 text-primary ring-1 ring-primary/15 shadow-sm"
                                : "hover:bg-muted/60 text-muted-foreground hover:text-foreground"
                            }`}
                          >
                            <MethodBadge method={e.method} />
                            <span className="font-mono truncate flex-1">{e.path}</span>
                          </button>
                        </TooltipTrigger>
                        {e.summary && (
                          <TooltipContent side="right" className="max-w-xs">
                            <p className="font-medium text-sm">{e.summary}</p>
                          </TooltipContent>
                        )}
                      </Tooltip>
                    </li>
                  ))}
                </ul>
              </CollapsibleContent>
            </Collapsible>
          );
        })}
      </div>
    </div>
  );
}
