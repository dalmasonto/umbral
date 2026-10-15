import { ChevronRight } from "lucide-react";
import type { MiddlewareInventory, Telemetry } from "../lib/api";
import { ms, pct } from "../lib/api";

// The signature instrument, now fed by real data: layer decomposition over
// all captured requests (averages) + the expandable EVERY-middleware view
// the registry and probe seam make possible.
const LAYERS: { key: string; label: string; hint: string }[] = [
  { key: "middleware", label: "Middleware", hint: "typed middlewares + bracketed plugin wrap layers" },
  { key: "routing", label: "Routing", hint: "route match + extractors" },
  { key: "query_build", label: "Query build", hint: "ORM build + pool checkout + decode (Postgres mode)" },
  { key: "database", label: "Database", hint: "sqlx-reported execution (Postgres) or handler ORM wall (SQLite)" },
  { key: "serialize", label: "Serialize", hint: "payload assembly in JSON handlers" },
  { key: "render", label: "Render", hint: "template rendering in HTML handlers" },
  { key: "handler_logic", label: "Handler logic", hint: "loop/format work inside handlers" },
  { key: "tail", label: "Tail", hint: "response path after the handler + unattributed residue" },
];
const RAMP = [14, 25, 36, 48, 60, 72, 84, 94]; // bg-foreground opacity steps

export default function LayerLedger({
  telemetry,
  inventory,
  caption,
}: {
  telemetry: Telemetry;
  inventory: MiddlewareInventory;
  /** What window this profile covers ("run r-6 · readers @ 10 vus" etc.). */
  caption?: string;
}) {
  const avg = (layer: string): number =>
    telemetry.layers.find((l) => l.layer === layer)?.avg_us ?? 0;
  const total = LAYERS.reduce((acc, l) => acc + avg(l.key), 0);
  const dominant = LAYERS.reduce(
    (best, l) => (avg(l.key) > avg(best.key) ? l : best),
    LAYERS[0],
  );
  const hue = telemetry.backend === "postgres" ? "var(--chart-2)" : "var(--chart-1)";

  const measured = new Map(inventory.measured.map((m) => [m.name, m]));
  const chain = inventory.chain;
  // Every middleware the framework knows about: the typed registry plus the
  // wrap-layer rows the probe seam measures. Measured rows that are not in
  // the chain (shouldn't happen) still render.
  const all: { name: string; source: string; order: number | null; us: number | null }[] = [];
  const seen = new Set<string>();
  for (const c of chain) {
    seen.add(c.name);
    const m = measured.get(c.name);
    all.push({ name: c.name, source: c.source, order: c.order, us: m ? m.avg_us : null });
  }
  for (const m of inventory.measured) {
    if (!seen.has(m.name)) all.push({ name: m.name, source: "measured", order: null, us: m.avg_us });
  }
  const measuredTotal = all.reduce((acc, r) => acc + (r.us ?? 0), 0);
  const mwTotal = avg("middleware");
  const worst = all.reduce((b, r) => ((r.us ?? 0) > (b.us ?? 0) ? r : b), all[0]);

  return (
    <section className="rounded-lg border border-border bg-card p-4 sm:p-5">
      <div className="mb-4 flex flex-wrap items-baseline justify-between gap-2">
        <div>
          <h2 className="text-base font-semibold tracking-tight">Where the time goes</h2>
          <p className="text-sm text-muted-foreground">
            {caption ? <span className="font-mono">{caption}</span> : "Averages"} —{" "}
            {telemetry.total_requests.toLocaleString()} captured requests on{" "}
            <span className="font-mono">{telemetry.backend}</span>, in pipeline order.
          </p>
        </div>
        <span className="font-mono text-xs text-muted-foreground">
          Σ {ms(total)} avg · database attributed:{" "}
          {avg("query_build") > 0 ? "sqlx per request" : "SQLite orm-wall mode"}
        </span>
      </div>

      <div className="grid grid-cols-[7.5rem_1fr] items-center gap-4">
        <div className="flex items-center gap-2">
          <span
            className="h-2.5 w-2.5 shrink-0 rounded-sm"
            style={{ background: hue }}
          />
          <span className="text-sm font-medium">{telemetry.backend}</span>
        </div>
        <div className="min-w-0">
          <div className="flex h-7 w-full overflow-hidden rounded-md border border-border">
            {LAYERS.map((l, i) => (
              <div
                key={l.key}
                title={`${l.label}: ${ms(avg(l.key))} (${pct(avg(l.key), total).toFixed(0)}%) — ${l.hint}`}
                className={`h-full bg-foreground ${pct(avg(l.key), total) > 0 ? "" : "hidden"} ${
                  l.key === dominant.key ? "outline-2 -outline-offset-2 outline-foreground/80" : ""
                }`}
                style={{ width: `${pct(avg(l.key), total).toFixed(2)}%`, opacity: RAMP[i] / 100 }}
              />
            ))}
          </div>
          <div className="mt-1 flex justify-between font-mono text-xs text-muted-foreground">
            <span>Σ {ms(total)} avg</span>
            <span>
              {LAYERS.map((l) => `${pct(avg(l.key), total).toFixed(0)}%`).join(" · ")}
            </span>
          </div>
        </div>
      </div>

      <div className="mt-4 flex items-start gap-2 rounded-md bg-muted px-3 py-2 text-sm">
        <span
          className="mt-0.5 h-4 w-1 shrink-0 rounded-full"
          style={{ background: hue }}
        />
        <p>
          <span className="font-medium">{dominant.label}</span> dominates at{" "}
          <span className="font-mono">{pct(avg(dominant.key), total).toFixed(0)}%</span> of the
          average request ({ms(avg(dominant.key))}). {LAYERS.find((l) => l.key === dominant.key)?.hint}.
        </p>
      </div>

      <details className="group mt-5 rounded-md border border-border/60">
        <summary className="flex cursor-pointer list-none items-center gap-2 px-3 py-2 text-sm">
          <ChevronRight className="h-4 w-4 text-muted-foreground transition-transform group-open:rotate-90" />
          <span className="font-medium">Middleware, expanded</span>
          <span className="ml-auto font-mono text-xs text-muted-foreground">
            {all.length} in the chain · {ms(measuredTotal)} avg measured
          </span>
        </summary>
        <div className="space-y-2 border-t border-border/60 px-3 py-3">
          {all.length === 0 && (
            <p className="text-xs text-muted-foreground">
              No middleware recorded yet — send a request first.
            </p>
          )}
          {all.map((m) => (
            <div
              key={`${m.name}:${m.source}`}
              className="grid grid-cols-[11rem_1fr_4.5rem] items-center gap-3 text-xs"
            >
              <span className={m.us !== null && m === worst ? "font-medium" : "text-muted-foreground"}>
                {m.name}
                {m.order !== null && (
                  <span className="ml-1 font-mono text-[10px] opacity-70">#{m.order}</span>
                )}
              </span>
              <span className="h-1.5 w-full overflow-hidden rounded-full bg-muted">
                {m.us !== null && measuredTotal > 0 && (
                  <span
                    className="block h-full rounded-full"
                    style={{ width: `${pct(m.us, measuredTotal).toFixed(1)}%`, background: hue, opacity: 0.7 }}
                  />
                )}
              </span>
              <span className="text-right font-mono tabular-nums">
                {m.us === null ? "—" : `${m.us.toFixed(1)} µs`}
              </span>
            </div>
          ))}
          <p className="pt-1 text-xs text-muted-foreground">
            Typed middlewares self-time their own before/after work; the bracketed wrap layers
            (`security`, `sessions`, probe) are measured by marker plugins via
            `Plugin::dependencies()`; `—` marks a registry entry with no per-request measurement
            (e.g. the trace root itself). Middleware layer total: {ms(mwTotal)}, of which{" "}
            {ms(measuredTotal)} is individually attributed.
          </p>
        </div>
      </details>
    </section>
  );
}
