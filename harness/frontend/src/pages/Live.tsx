import { useEffect, useRef, useState } from "react";
import BenchChart, { type Series } from "../components/BenchChart";
import { api, type ErrorKind, type LabSettings, type RunRecord, type Sample } from "../lib/api";

// Live run: configure, launch, and watch the SSE stream. The chart plots the
// per-second p50/p95/p99 the load generator observes plus the SERVER-observed
// p95 (from each response's Server-Timing header) — the gap between them is
// network + load-generator overhead.
const SCENARIOS = ["feed", "write-heavy", "profile", "static", "readers", "community"];

export default function Live() {
  const [scenario, setScenario] = useState("feed");
  const [vus, setVus] = useState(50);
  const [duration, setDuration] = useState(20);
  const [run, setRun] = useState<RunRecord | null>(null);
  const [samples, setSamples] = useState<Sample[]>([]);
  const [status, setStatus] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [liveErrors, setLiveErrors] = useState<ErrorKind[]>([]);
  const [lab, setLab] = useState<LabSettings | null>(null);
  const labTimer = useRef<number | null>(null);
  const esClose = useRef<(() => void) | null>(null);

  useEffect(() => {
    api
      .lab()
      .then(setLab)
      .catch(() => {});
    return () => {
      esClose.current?.();
      if (labTimer.current) window.clearTimeout(labTimer.current);
    };
  }, []);

  const applyLab = (patch: Partial<LabSettings>) => {
    setLab((prev) => (prev ? { ...prev, ...patch } : prev));
    if (labTimer.current) window.clearTimeout(labTimer.current);
    labTimer.current = window.setTimeout(() => {
      void api.setLab(patch).catch(() => {});
    }, 250);
  };

  const start = async () => {
    setError(null);
    setSamples([]);
    setRun(null);
    setLiveErrors([]);
    setStatus("starting…");
    esClose.current?.();
    try {
      const r = await api.startRun(scenario, vus, duration);
      setRun(r);
      setStatus("running");
      esClose.current = api.stream(r.id, (ev) => {
        if (ev.new_errors?.length) {
          // Broadcast carries each kind once (at first occurrence); the full
          // per-kind counts arrive in the final summary.
          setLiveErrors((prev) => {
            const seen = new Set(prev.map((e) => `${e.status}:${e.path}`));
            return [...prev, ...ev.new_errors!.filter((e) => !seen.has(`${e.status}:${e.path}`))];
          });
        }
        if (ev.status === "done") {
          setStatus("done");
          esClose.current?.();
          if (ev.summary?.error_breakdown?.length) {
            setLiveErrors(ev.summary.error_breakdown);
          }
          void api.run(r.id).then((full) => {
            setSamples(full.samples);
            setRun(full);
          });
          return;
        }
        if (ev.sample) setSamples((prev) => [...prev, ev.sample as Sample]);
      });
    } catch (e) {
      setStatus(null);
      setError(String(e));
    }
  };

  const x = samples.map((s) => s.sec);
  const series: Series[] = [
    { name: "p50", varName: "--chart-3", data: samples.map((s) => s.p50_ms) },
    { name: "p95", varName: "--chart-1", data: samples.map((s) => s.p95_ms) },
    { name: "p99", varName: "--chart-2", data: samples.map((s) => s.p99_ms) },
    {
      name: "server p95",
      varName: "--foreground",
      dash: 6,
      data: samples.map((s) => s.server_p95_ms),
    },
  ];
  const last = samples.at(-1);

  return (
    <div className="space-y-6">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <h1 className="text-xl font-semibold tracking-tight">Load run, streamed live</h1>
          <p className="mt-1 text-sm text-muted-foreground">
            In-process virtual users hammer this server over loopback; samples arrive by SSE.
          </p>
        </div>
        <div className="flex items-center gap-2 font-mono text-xs">
          <span className="inline-flex items-center gap-1.5 rounded-md border border-border px-2 py-1">
            <span
              className={`h-2 w-2 rounded-full ${
                status === "running" ? "animate-pulse bg-destructive" : "bg-muted-foreground"
              }`}
            />
            {status ?? "idle"}
          </span>
          {run && <span className="rounded-md border border-border px-2 py-1 text-muted-foreground">r-{run.id}</span>}
        </div>
      </div>

      {error && (
        <p className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">
          {error} — is the backend running on :8113?
        </p>
      )}

      <section className="grid gap-4 lg:grid-cols-[2fr_1fr]">
        <div className="rounded-lg border border-border bg-card p-4 sm:p-5">
          <div className="mb-3 flex flex-wrap items-baseline justify-between gap-2">
            <h2 className="text-base font-semibold tracking-tight">Live latency</h2>
            <div className="flex flex-wrap items-center gap-x-3 gap-y-1 font-mono text-xs text-muted-foreground">
              <span className="inline-flex items-center gap-1.5"><span className="h-2 w-2 rounded-sm" style={{ background: "var(--chart-3)" }} />p50</span>
              <span className="inline-flex items-center gap-1.5"><span className="h-2 w-2 rounded-sm" style={{ background: "var(--chart-1)" }} />p95</span>
              <span className="inline-flex items-center gap-1.5"><span className="h-2 w-2 rounded-sm" style={{ background: "var(--chart-2)" }} />p99</span>
              <span>dashed: server-side p95 (Server-Timing)</span>
            </div>
          </div>
          {samples.length === 0 ? (
            <div className="grid h-64 place-items-center rounded-lg border border-dashed border-border font-mono text-sm text-muted-foreground">
              configure a run and start it →
            </div>
          ) : (
            <BenchChart
              series={series}
              x={x}
              xTitle="elapsed"
              yTitle="latency (ms)"
              numericX
              xFormatter={(v) => `${Math.floor(v / 60)}:${String(Math.round(v % 60)).padStart(2, "0")}`}
              budgets={[{ y: 500, label: "p95 budget · 500 ms" }]}
              height={280}
            />
          )}
        </div>

        <div className="space-y-4">
          <section className="rounded-lg border border-border bg-card p-4">
            <h2 className="text-sm font-semibold">Run controls</h2>
            <div className="mt-3 space-y-3 text-sm">
              <label className="block">
                <span className="text-xs text-muted-foreground">scenario</span>
                <select
                  value={scenario}
                  onChange={(e) => setScenario(e.target.value)}
                  className="mt-1 w-full rounded-md border border-border bg-background px-2 py-1.5 font-mono text-xs"
                >
                  {SCENARIOS.map((s) => (
                    <option key={s} value={s}>
                      {s}
                    </option>
                  ))}
                </select>
              </label>
              <label className="block">
                <span className="text-xs text-muted-foreground">
                  virtual users: {vus.toLocaleString()}
                </span>
                <input
                  type="range"
                  min={1}
                  max={2000}
                  value={vus}
                  onChange={(e) => setVus(Number(e.target.value))}
                  className="mt-1 w-full accent-primary"
                />
                <span className="mt-1 flex flex-wrap gap-1">
                  {[25, 50, 100, 250, 500, 1000, 2000].map((n) => (
                    <button
                      key={n}
                      onClick={() => setVus(n)}
                      className={`rounded border px-1.5 py-0.5 font-mono text-[10px] ${
                        vus === n
                          ? "border-border bg-muted font-medium text-foreground"
                          : "border-transparent text-muted-foreground hover:bg-accent"
                      }`}
                    >
                      {n >= 1000 ? `${n / 1000}k` : n}
                    </button>
                  ))}
                </span>
              </label>
              <label className="block">
                <span className="text-xs text-muted-foreground">duration: {duration}s</span>
                <input
                  type="range"
                  min={5}
                  max={120}
                  value={duration}
                  onChange={(e) => setDuration(Number(e.target.value))}
                  className="mt-1 w-full accent-primary"
                />
              </label>
              <button
                onClick={() => void start()}
                disabled={status === "running"}
                className="w-full rounded-md bg-primary px-3 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
              >
                {status === "running" ? "running…" : "Start run"}
              </button>
            </div>
          </section>

          <section className="rounded-lg border border-border bg-card p-4">
            <h2 className="text-sm font-semibold">This second</h2>
            <dl className="mt-3 space-y-1.5 font-mono text-xs">
              <div className="flex justify-between"><dt className="text-muted-foreground">req/s</dt><dd>{last ? last.rps.toFixed(0) : "—"}</dd></div>
              <div className="flex justify-between"><dt className="text-muted-foreground">p95</dt><dd>{last ? `${last.p95_ms.toFixed(1)} ms` : "—"}</dd></div>
              <div className="flex justify-between"><dt className="text-muted-foreground">p99</dt><dd>{last ? `${last.p99_ms.toFixed(1)} ms` : "—"}</dd></div>
              <div className="flex justify-between"><dt className="text-muted-foreground">server p95</dt><dd>{last ? `${last.server_p95_ms.toFixed(1)} ms` : "—"}</dd></div>
              <div className="flex justify-between"><dt className="text-muted-foreground">errors</dt><dd className={last && last.errors > 0 ? "text-destructive" : ""}>{last ? last.errors : "—"}</dd></div>
              <div className="flex justify-between"><dt className="text-muted-foreground">seconds</dt><dd>{samples.length}{run ? ` / ${run.duration_s}` : ""}</dd></div>
            </dl>
            {run?.summary && status === "done" && (
              <p className="mt-3 border-t border-border/60 pt-3 text-xs text-muted-foreground">
                Run finished: {run.summary.rps_avg.toFixed(0)} req/s avg · p50{" "}
                {run.summary.p50_ms.toFixed(1)} · p95 {run.summary.p95_ms.toFixed(1)} · p99{" "}
                {run.summary.p99_ms.toFixed(1)} ms · {run.summary.errors} errors over{" "}
                {run.summary.sent.toLocaleString()} requests.
              </p>
            )}
          </section>

          {lab && (
            <section className="rounded-lg border border-border bg-card p-4">
              <h2 className="text-sm font-semibold">Framework lab</h2>
              <p className="mt-1 text-xs text-muted-foreground">
                Live middlewares you can switch on. Watch its row in Layers → Middleware, and the
                latency curve here, change as you drag.
              </p>
              <div className="mt-3 space-y-3">
                <label className="block">
                  <span className="text-xs text-muted-foreground">
                    inject middleware latency: {(lab.fake_middleware_us / 1000).toFixed(2)} ms
                    {lab.fake_middleware_us === 0 && " (off)"}
                  </span>
                  <input
                    type="range"
                    min={0}
                    max={10000}
                    step={250}
                    value={lab.fake_middleware_us}
                    onChange={(e) => applyLab({ fake_middleware_us: Number(e.target.value) })}
                    className="mt-1 w-full accent-primary"
                  />
                </label>
                <label className="block">
                  <span className="text-xs text-muted-foreground">
                    rate limit:{" "}
                    {lab.rate_limit_per_sec === 0 ? "off" : `${lab.rate_limit_per_sec}/s per user`}
                  </span>
                  <input
                    type="range"
                    min={0}
                    max={500}
                    step={5}
                    value={lab.rate_limit_per_sec}
                    onChange={(e) => applyLab({ rate_limit_per_sec: Number(e.target.value) })}
                    className="mt-1 w-full accent-primary"
                  />
                </label>
                <p className="text-xs text-muted-foreground">
                  Latency injection sleeps inside a real middleware (order −70) — p95 moves by
                  roughly that much everywhere. The rate limit short-circuits with{" "}
                  <span className="font-mono">429</span> before handlers run; tripped requests show
                  up in the Errors panel below and in the run's Failures breakdown.
                </p>
              </div>
            </section>
          )}

          <section className="rounded-lg border border-border bg-card p-4">
            <h2 className="text-sm font-semibold">
              Errors{" "}
              {liveErrors.length > 0 && (
                <span className="ml-1 rounded-md border border-destructive/40 bg-destructive/10 px-1.5 py-0.5 font-mono text-xs text-destructive">
                  {liveErrors.reduce((a, e) => a + e.count, 0)}
                </span>
              )}
            </h2>
            {liveErrors.length === 0 ? (
              <p className="mt-2 text-xs text-muted-foreground">
                None {status ? "so far" : "— errors appear here live, with status + sample body."}
              </p>
            ) : (
              <ul className="mt-2 space-y-2">
                {liveErrors.map((e) => (
                  <li key={`${e.status}:${e.path}`} className="rounded-md border border-border/60 p-2 text-xs">
                    <p className="font-mono">
                      <span className={e.status >= 400 || e.status === 0 ? "text-destructive" : ""}>
                        {e.status === 0 ? "network" : e.status}
                      </span>{" "}
                      {e.path} {e.count > 1 && <span className="text-muted-foreground">× {e.count}</span>}
                    </p>
                    {e.sample && (
                      <p className="mt-1 line-clamp-3 break-all text-muted-foreground">{e.sample}</p>
                    )}
                  </li>
                ))}
              </ul>
            )}
          </section>
        </div>
      </section>

      <p className="pb-2 text-xs text-muted-foreground">
        Load is generated in-process (loopback HTTP). The generator's own CPU is on the same
        machine — treat absolute numbers as dev-profile figures until a release build and an
        external generator are used.
      </p>
    </div>
  );
}
