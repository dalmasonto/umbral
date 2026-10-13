import { BarChart3 } from "lucide-react";

const NAV = [
  { href: "#/", label: "Overview" },
  { href: "#/layers", label: "Layers" },
  { href: "#/live", label: "Live run" },
  { href: "#/runs", label: "Runs" },
];

export default function BenchHeader({ route, backend }: { route: string; backend: string | null }) {
  return (
    <header className="sticky top-0 z-40 w-full border-b border-border bg-background/90 backdrop-blur">
      <div className="mx-auto flex h-14 max-w-6xl items-center gap-6 px-4">
        <a href="#/" className="flex items-center gap-2.5">
          <span className="grid h-7 w-7 place-items-center rounded-md bg-primary text-primary-foreground">
            <BarChart3 className="h-3.5 w-3.5" />
          </span>
          <span className="text-sm font-semibold tracking-tight">
            umbral <span className="font-normal text-muted-foreground">bench</span>
          </span>
        </a>
        <nav className="flex items-center gap-1 text-sm" aria-label="Bench sections">
          {NAV.map((n) => {
            const active = route === n.href.slice(1) || (route === "/" && n.href === "#/");
            return (
              <a
                key={n.href}
                href={n.href}
                aria-current={active ? "page" : undefined}
                className={`rounded-md border px-2.5 py-1.5 transition-colors ${
                  active
                    ? "border-border bg-muted font-medium text-foreground"
                    : "border-transparent text-muted-foreground hover:bg-accent hover:text-accent-foreground"
                }`}
              >
                {n.label}
              </a>
            );
          })}
        </nav>
        <div className="ml-auto hidden items-center gap-3 md:flex">
          {backend && (
            <span
              className={`inline-flex items-center gap-1.5 rounded-md border border-border px-2 py-1 font-mono text-xs ${
                backend === "postgres" ? "text-foreground" : "text-foreground"
              }`}
            >
              <span
                className={`h-2 w-2 rounded-full ${
                  backend === "postgres" ? "bg-chart-2" : "bg-chart-1"
                }`}
              />
              {backend}
            </span>
          )}
          <span className="rounded-md border border-border bg-muted px-2 py-1 font-mono text-xs text-muted-foreground">
            :8113
          </span>
        </div>
      </div>
    </header>
  );
}
