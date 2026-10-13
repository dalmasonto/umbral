import { useEffect, useState } from "react";
import BenchChart, { type Series } from "../components/BenchChart";
import { api, type RunListItem, type Telemetry } from "../lib/api";
import { assessRun, type Level } from "../lib/health";

function VerdictChip({ level, label }: { level: Level; label: string }) {
  if (level === "pass") {
    return (
      <span className="rounded-md border border-border bg-muted px-2 py-0.5 font-mono text-xs">
        ✓ {label}
      </span>
    );
  }
  if (level === "warn") {
    return (
      <span
        className="rounded-md border px-2 py-0.5 font-mono text-xs"
        style={{
          borderColor: "color-mix(in oklab, var(--chart-1) 45%, transparent)",
          background: "color-mix(in oklab, var(--chart-1) 12%, transparent)",
          color: "var(--chart-1)",
        }}
      >
        ⚠ {label}
      </span>
    );
  }
  return (
    <span className="rounded-md border border-destructive/40 bg-destructive/10 px-2 py-0.5 font-mono text-xs text-destructive">
      ✗ {label}
    </span>
  );
}

// Overview: score card per backend from the latest completed run, and the
// latency/throughput curves built from ALL completed runs of one scenario —
// each run contributes one point (its VU level), so the ramp emerges as you
// run higher levels.
const SCENARIOS = ["feed", "write-heavy", "profile", "static", "readers", "community"] as const;

export default function Overview() {
  const [runs, setRuns] = useState<RunListItem[]>([]);
  const [telemetry, setTelemetry] = useState<Telemetry | null>(null);
  const [scenario, setScenario] = useState<string>("feed");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    const load = () => {
      api
        .runs()
        .then((r) => alive && setRuns(r.runs))
        .catch((e) => alive && setError(String(e)));
      api
        .telemetry()
        .then((t) => alive && setTelemetry(t))
        .catch(() => {});
    };
    load();
    const t = setInterval(load, 5000);
    return () => {
      alive = false;
      clearInterval(t);
    };
  }, []);

  const done = runs.filter((r) => r.status === "done" && r.summary && r.scenario === scenario);
  const latestPer = (backend: string) =>
    runs.find((r) => r.status === "done" && r.backend === backend && r.summary);

  const byVus = new Map<number, RunListItem[]>();
  for (const r of done) {
    byVus.set(r.vus, [...(byVus.get(r.vus) ?? []), r]);
  }
  const levels = [...byVus.keys()].sort((a, b) => a - b);
  const pick = (backend: string, lvl: number) =>
    byVus.get(lvl)?.find((r) => r.backend === backend);

  const latency: Series[] = [
    { name: "sqlite p95", varName: "--chart-1", data: levels.map((l) => pick("sqlite", l)?.summary?.p95_ms ?? NaN) },
    { name: "sqlite p99", varName: "--chart-1", dash: 6, data: levels.map((l) => pick("sqlite", l)?.summary?.p99_ms ?? NaN) },
    { name: "postgres p95", varName: "--chart-2", data: levels.map((l) => pick("postgres", l)?.summary?.p95_ms ?? NaN) },
    { name: "postgres p99", varName: "--chart-2", dash: 6, data: levels.map((l) => pick("postgres", l)?.summary?.p99_ms ?? NaN) },
  ].filter((s) => s.data.some((v) => !Number.isNaN(v)));

  const throughput: Series[] = [
    { name: "sqlite", varName: "--chart-1", data: levels.map((l) => pick("sqlite", l)?.summary?.rps_avg ?? NaN) },
    { name: "postgres", varName: "--chart-2", data: levels.map((l) => pick("postgres", l)?.summary?.rps_avg ?? NaN) },
  ].filter((s) => s.data.some((v) => !Number.isNaN(v)));

  const card = (backend: string, dot: string) => {
    const r = latestPer(backend);
    return (
      <article className="rounded-lg border border-border bg-card p-4 sm:p-5">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            <span className={`h-2.5 w-2.5 rounded-sm ${dot}`} />
            <h2 className="text-sm font-semibold capitalize">{backend}</h2>
          </div>
          {r && (
            <span className="inline-flex items-center gap-1.5 rounded-md border border-border px-2 py-0.5 font-mono text-xs">
              r-{r.id} · {r.scenario} @ {r.vus} vus
            </span>
          )}
        </div>
        {r?.summary ? (
          <>
            <div className="mt-3 flex items-baseline gap-2">
              <span className="font-mono text-3xl font-medium tabular-nums">
                {r.summary.p95_ms.toFixed(1)}
              </span>
              <span className="text-sm text-muted-foreground">ms p95 · budget 500 ms</span>
            </div>
            <dl className="mt-3 grid grid-cols-3 gap-2 border-t border-border/60 pt-3 text-xs">
              <div>
                <dt className="text-muted-foreground">p50</dt>
                <dd className="mt-0.5 font-mono tabular-nums">{r.summary.p50_ms.toFixed(1)} ms</dd>
              </div>
              <div>
                <dt className="text-muted-foreground">p99</dt>
                <dd className="mt-0.5 font-mono tabular-nums">{r.summary.p99_ms.toFixed(1)} ms</dd>
              </div>
              <div>
                <dt className="text-muted-foreground">errors</dt>
                <dd className="mt-0.5 font-mono tabular-nums">
                  {((r.summary.errors / Math.max(1, r.summary.sent)) * 100).toFixed(2)}%
                </dd>
              </div>
            </dl>
            <p className="mt-3 border-t border-border/60 pt-3 text-xs text-muted-foreground">
              {r.summary.rps_avg.toFixed(0)} req/s sustained ·{" "}
              {r.summary.sent.toLocaleString()} requests sent
            </p>
          </>
        ) : (
          <p className="mt-3 text-sm text-muted-foreground">
            No completed {backend} run yet — start one from{" "}
            <a href="#/live" className="underline underline-offset-2">
              Live run
            </a>
            .
          </p>
        )}
      </article>
    );
  };

  return (
    <div className="space-y-6">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h1 className="text-xl font-semibold tracking-tight">umbral bench</h1>
          <p className="mt-1 text-sm text-muted-foreground">
            Every run on this backend, and where each request's time goes — measured by the
            harness itself (<span className="font-mono">harness/backend</span>).
          </p>
        </div>
        <a
          href="#/live"
          className="rounded-md bg-primary px-3 py-1.5 text-sm font-medium text-primary-foreground hover:bg-primary/90"
        >
          Watch a run live
        </a>
      </div>

      {error && (
        <p className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">
          {error} — is the backend running on :8113?
        </p>
      )}

      <section className="rounded-lg border border-border bg-card p-4 sm:p-5">
        <div className="mb-1">
          <h2 className="text-base font-semibold tracking-tight">Is it holding up?</h2>
          <p className="text-sm text-muted-foreground">
            Verdict from the latest completed run per backend, against the acceptance budgets:
            p95 ≤ 500 ms · p99 ≤ 1 s · errors &lt; 1%.
          </p>
        </div>
        <div className="mt-4 grid gap-4 sm:grid-cols-2">
          {(["sqlite", "postgres"] as const).map((backend) => {
            const r = latestPer(backend);
            const v = r?.summary ? assessRun(r, runs, telemetry) : null;
            return (
              <div key={backend} className="rounded-md border border-border/60 p-3">
                <div className="flex items-center justify-between gap-2">
                  <span className="inline-flex items-center gap-2 text-sm font-medium capitalize">
                    <span
                      className={`h-2.5 w-2.5 rounded-sm ${
                        backend === "postgres" ? "bg-chart-2" : "bg-chart-1"
                      }`}
                    />
                    {backend}
                  </span>
                  {v && <VerdictChip level={v.level} label={v.label} />}
                </div>
                {v ? (
                  <ul className="mt-2.5 space-y-1.5 text-sm text-muted-foreground">
                    {v.lines.map((line) => (
                      <li key={line.slice(0, 24)} className="flex gap-2">
                        <span className="mt-2 h-1 w-1 shrink-0 rounded-full bg-muted-foreground" />
                        <span>{line}</span>
                      </li>
                    ))}
                  </ul>
                ) : (
                  <p className="mt-2 text-sm text-muted-foreground">
                    No completed runs yet — start one from{" "}
                    <a href="#/live" className="underline underline-offset-2">
                      Live run
                    </a>
                    .
                  </p>
                )}
              </div>
            );
          })}
        </div>
      </section>

      <section className="grid gap-4 sm:grid-cols-2" aria-label="Backend score summary">
        {card("sqlite", "bg-chart-1")}
        {card("postgres", "bg-chart-2")}
      </section>

      <section className="rounded-lg border border-border bg-card p-4 sm:p-5">
        <div className="mb-4 flex flex-wrap items-baseline justify-between gap-2">
          <div>
            <h2 className="text-base font-semibold tracking-tight">Latency as load climbs</h2>
            <p className="text-sm text-muted-foreground">
              One point per completed run level (budget: p95 &lt; 500 ms, p99 &lt; 1 s).
            </p>
          </div>
          <select
            value={scenario}
            onChange={(e) => setScenario(e.target.value)}
            className="rounded-md border border-border bg-background px-2 py-1 font-mono text-xs"
          >
            {SCENARIOS.map((s) => (
              <option key={s} value={s}>
                {s}
              </option>
            ))}
          </select>
        </div>
        {latency.length > 0 ? (
          <>
            <div className="mb-2 flex flex-wrap items-center gap-x-4 gap-y-1 font-mono text-xs text-muted-foreground">
              <span className="inline-flex items-center gap-1.5">
                <span className="h-2 w-2 rounded-sm bg-chart-1" />
                SQLite
              </span>
              <span className="inline-flex items-center gap-1.5">
                <span className="h-2 w-2 rounded-sm bg-chart-2" />
                Postgres
              </span>
              <span>solid p95 · dashed p99</span>
            </div>
            <BenchChart
              series={latency}
              x={levels.map(String)}
              xTitle="virtual users"
              yTitle="latency (ms)"
              budgets={[
                { y: 500, label: "p95 budget · 500 ms" },
                { y: 1000, label: "p99 budget · 1 s" },
              ]}
              height={340}
            />
          </>
        ) : (
          <p className="text-sm text-muted-foreground">
            No completed runs of <span className="font-mono">{scenario}</span> yet.
          </p>
        )}
      </section>

      {throughput.length > 0 && (
        <section className="rounded-lg border border-border bg-card p-4 sm:p-5">
          <h2 className="mb-3 text-base font-semibold tracking-tight">Throughput</h2>
          <BenchChart
            series={throughput}
            x={levels.map(String)}
            xTitle="virtual users"
            yTitle="requests / s"
            height={280}
          />
        </section>
      )}

      <p className="pb-2 text-xs text-muted-foreground">
        Runs are stored per backend × scenario × VU level; records land in{" "}
        <span className="font-mono">traces/runs.jsonl</span> and per-request traces in{" "}
        <span className="font-mono">traces/requests.jsonl</span>.
      </p>
    </div>
  );
}
