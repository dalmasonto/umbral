import { Badge } from "@/components/ui/badge";

const METHOD_VARIANTS: Record<string, React.ComponentProps<typeof Badge>["variant"]> = {
  GET: "default",
  POST: "secondary",
  PUT: "outline",
  PATCH: "ghost",
  DELETE: "destructive",
};

const METHOD_COLORS: Record<string, string> = {
  GET: "bg-sky-500/15 text-sky-600 border-sky-500/25 hover:bg-sky-500/25",
  POST: "bg-emerald-500/15 text-emerald-600 border-emerald-500/25 hover:bg-emerald-500/25",
  PUT: "bg-amber-500/15 text-amber-600 border-amber-500/25 hover:bg-amber-500/25",
  PATCH: "bg-violet-500/15 text-violet-600 border-violet-500/25 hover:bg-violet-500/25",
  DELETE: "bg-rose-500/15 text-rose-600 border-rose-500/25 hover:bg-rose-500/25",
};

export function MethodBadge({ method }: { method: string }) {
  const upper = method.toUpperCase();
  const cls = METHOD_COLORS[upper] ?? "bg-muted text-muted-foreground border-transparent";
  return (
    <Badge
      variant={METHOD_VARIANTS[upper] ?? "outline"}
      className={`font-mono text-[10px] font-bold tracking-wide px-1.5 py-0 min-w-[3.2rem] justify-center ${cls}`}
    >
      {upper}
    </Badge>
  );
}
