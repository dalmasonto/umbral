import { useEffect, useState } from "react";
import BenchChart, { type Series } from "../components/BenchChart";
import { api, type RunListItem } from "../lib/api";
import { chipLevel, type Level } from "../lib/health";

function Chip({ level }: { level: Level }) {
  const cls =
    level === "pass"
      ? "border-border bg-muted text-foreground"
      : level === "warn"
        ? "border-border text-muted-foreground"
        : "border-destructive/40 bg-destructive/10 text-destructive";
  const mark = level === "pass" ? "✓" : level === "warn" ? "⚠" : "✗";
  return (
    <span className={`rounded-md border px-1.5 py-0.5 font-mono text-xs ${cls}`}>{mark}</span>
  );
}

// Run history + a selected run's per-second profile.
export default function Runs() {
  const [runs, setRuns] = useState<RunListItem[]>([]);
  const [selected, setSelected] = useState<number | null>(null);
  const [samples, setSamples] = useState<{ sec: number; p50_ms: number; p95_ms: number; p99_ms: number; rps: number }[]>([]);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    const load = () =>
      api
        .runs()
        .then((r) => {
          if (!alive) return;
          setRuns(r.runs);
          setSelected((cur) => cur ?? r.runs.find((x) => x.status === "done")?.id ?? null);
        })
        .catch((e) => alive && setError(String(e)));
    load();
    const t = setInterval(load, 5000);
    return () => {
      alive = false;
      clearInterval(t);
    };
  }, []);

  useEffect(() => {
    if (selected == null) return;
    let alive = true;
    api
      .run(selected)
      .then((r) => alive && setSamples(r.samples))
      .catch(() => alive && setSamples([]));
    return () => {
      alive = false;
    };
  }, [selected]);

  const series: Series[] = [
    { name: "p50", varName: "--chart-3", data: samples.map((s) => s.p50_ms) },
    { name: "p95", varName: "--chart-1", data: samples.map((s) => s.p95_ms) },
    { name: "p99", varName: "--chart-2", data: samples.map((s) => s.p99_ms) },
  ];
  const selectedRun = runs.find((r) => r.id === selected);

  return (
    <div className="space-y-6">
      <div className="flex flex-wrap items-end justify-between gap-3">
        <div>
          <h1 className="text-xl font-semibold tracking-tight">Every run, kept</h1>
          <p className="mt-1 text-sm text-muted-foreground">
            Records persist to <span className="font-mono">traces/runs.jsonl</span>, so a number can
            always be traced back to the run that produced it.
          </p>
        </div>
        <a
          href="#/live"
          className="rounded-md border border-border px-3 py-1.5 text-sm font-medium hover:bg-accent hover:text-accent-foreground"
        >
          Start a run
        </a>
      </div>

      {error && (
        <p className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">
          {error} — is the backend running on :8113?
        </p>
      )}

      <section className="rounded-lg border border-border bg-card p-4 sm:p-5">
        <h2 className="mb-3 text-base font-semibold tracking-tight">
          Runs{selectedRun ? ` — r-${selectedRun.id} (${selectedRun.scenario} @ ${selectedRun.vus} vus)` : ""}
        </h2>
        <div className="overflow-x-auto">
          <table className="w-full min-w-[44rem] text-sm">
            <thead>
              <tr className="text-xs text-muted-foreground">
                <th className="py-2 pr-3 text-left font-medium">Run</th>
                <th className="py-2 pr-3 text-left font-medium">Started</th>
                <th className="py-2 pr-3 text-left font-medium">Scenario</th>
                <th className="py-2 pr-3 text-right font-medium">VUs</th>
                <th className="py-2 pr-3 text-right font-medium">req/s</th>
                <th className="py-2 pr-3 text-right font-medium">p50</th>
                <th className="py-2 pr-3 text-right font-medium">p95</th>
                <th className="py-2 pr-3 text-right font-medium">p99</th>
                <th className="py-2 pr-3 text-right font-medium">errors</th>
                <th className="py-2 pr-3 text-center font-medium" title="p95 ≤ 500 ms · p99 ≤ 1 s · errors < 1%">
                  verdict
                </th>
                <th className="py-2 pr-3 text-left font-medium">Backend</th>
                <th className="py-2 text-right font-medium">Status</th>
              </tr>
            </thead>
            <tbody className="font-mono tabular-nums">
              {runs.map((r) => (
                <tr
                  key={r.id}
                  onClick={() => setSelected(r.id)}
                  className={`cursor-pointer border-t border-border/60 transition-colors hover:bg-accent/50 ${
                    r.id === selected ? "bg-accent/40" : ""
                  }`}
                >
                  <td className="py-2 pr-3">r-{r.id}</td>
                  <td className="py-2 pr-3 text-muted-foreground">
                    {new Date(r.started_at).toLocaleString()}
                  </td>
                  <td className="py-2 pr-3 font-sans">{r.scenario}</td>
                  <td className="py-2 pr-3 text-right">{r.vus}</td>
                  <td className="py-2 pr-3 text-right">
                    {r.summary ? r.summary.rps_avg.toFixed(0) : "—"}
                  </td>
                  <td className="py-2 pr-3 text-right">
                    {r.summary ? r.summary.p50_ms.toFixed(1) : "—"}
                  </td>
                  <td className="py-2 pr-3 text-right">
                    {r.summary ? r.summary.p95_ms.toFixed(1) : "—"}
                  </td>
                  <td className="py-2 pr-3 text-right">
                    {r.summary ? r.summary.p99_ms.toFixed(1) : "—"}
                  </td>
                  <td className={`py-2 pr-3 text-right ${r.summary && r.summary.errors > 0 ? "text-destructive" : ""}`}>
                    {r.summary ? r.summary.errors : "—"}
                  </td>
                  <td className="py-2 pr-3 text-center">
                    {r.summary ? <Chip level={chipLevel(r.summary)} /> : "—"}
                  </td>
                  <td className="py-2 pr-3">
                    <span className="inline-flex items-center gap-1.5">
                      <span
                        className={`h-2 w-2 rounded-sm ${
                          r.backend === "postgres" ? "bg-chart-2" : "bg-chart-1"
                        }`}
                      />
                      {r.backend}
                    </span>
                  </td>
                  <td className="py-2 text-right">
                    {r.status === "done" ? (
                      <span className="rounded-md border border-border px-2 py-0.5 text-xs">done</span>
                    ) : (
                      <span className="rounded-md border border-border px-2 py-0.5 text-xs text-muted-foreground">
                        {r.status}
                      </span>
                    )}
                  </td>
                </tr>
              ))}
              {runs.length === 0 && (
                <tr>
                  <td colSpan={12} className="py-2 font-sans text-xs text-muted-foreground">
                    No runs yet — start one from{" "}
                    <a className="underline underline-offset-2" href="#/live">
                      Live run
                    </a>
                    .
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
      </section>

      {selectedRun?.status === "done" && samples.length > 0 && (
        <section className="rounded-lg border border-border bg-card p-4 sm:p-5">
          <h2 className="mb-3 text-base font-semibold tracking-tight">
            r-{selectedRun.id} — per-second latency
          </h2>
          <BenchChart
            series={series}
            x={samples.map((s) => s.sec)}
            xTitle="elapsed"
            yTitle="latency (ms)"
            numericX
            xFormatter={(v) => `${Math.floor(v / 60)}:${String(Math.round(v % 60)).padStart(2, "0")}`}
            height={280}
          />
        </section>
      )}

      {selectedRun?.summary && selectedRun.summary.errors > 0 && (
        <section className="rounded-lg border border-border bg-card p-4 sm:p-5">
          <div className="mb-3 flex flex-wrap items-baseline justify-between gap-2">
            <h2 className="text-base font-semibold tracking-tight">
              Failures in r-{selectedRun.id}
            </h2>
            <span className="rounded-md border border-destructive/40 bg-destructive/10 px-2 py-0.5 font-mono text-xs text-destructive">
              {selectedRun.summary.errors} of {selectedRun.summary.sent.toLocaleString()} requests
            </span>
          </div>
          {(selectedRun.summary.error_breakdown ?? []).length === 0 ? (
            <p className="text-xs text-muted-foreground">
              Per-kind detail was not captured for this run (older record) — run it again to get
              status codes and sample bodies.
            </p>
          ) : (
            <div className="space-y-2">
              {selectedRun.summary.error_breakdown!.map((e) => (
                <div key={`${e.status}:${e.path}`} className="rounded-md border border-border/60 p-3">
                  <p className="font-mono text-xs">
                    <span className="text-destructive">
                      {e.status === 0 ? "network error" : e.status}
                    </span>{" "}
                    <span className="font-medium">{e.path}</span>{" "}
                    <span className="text-muted-foreground">× {e.count}</span>
                  </p>
                  {e.sample && (
                    <p className="mt-1 break-all font-mono text-xs text-muted-foreground">
                      {e.sample}
                    </p>
                  )}
                </div>
              ))}
            </div>
          )}
        </section>
      )}

      <p className="pb-2 text-xs text-muted-foreground">
        Records are scoped to backend × scenario × VU level so comparisons stay honest. Failing
        requests record their status, path template and a response-body sample.
      </p>
    </div>
  );
}
