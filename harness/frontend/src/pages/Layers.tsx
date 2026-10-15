import { useEffect, useMemo, useRef, useState } from "react";
import LayerLedger from "../components/LayerLedger";
import { ms, num, api, type MiddlewareInventory, type RunListItem, type Telemetry } from "../lib/api";

// The Layers view, scoped to ONE run by default: pick a run and the ledger,
// middleware rows, routes and slowest statements all reflect only that
// run's requests (tagged `x-bench-run` server-side). "All traffic" is the
// since-last-reset process view.
export default function Layers() {
  const [runs, setRuns] = useState<RunListItem[]>([]);
  const [selected, setSelected] = useState<"all" | number>("all");
  const [picked, setPicked] = useState(false); // default-select the latest done run once
  const [telemetry, setTelemetry] = useState<Telemetry | null>(null);
  const [captured, setCaptured] = useState(true);
  const [inventory, setInventory] = useState<MiddlewareInventory | null>(null);
  const [live, setLive] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const selectedRef = useRef(selected);
  selectedRef.current = selected;

  // Runs list (for the selector).
  useEffect(() => {
    let alive = true;
    const load = () =>
      api
        .runs()
        .then((r) => {
          if (!alive) return;
          setRuns(r.runs);
          if (!picked) {
            const latestDone = [...r.runs].reverse().find((x) => x.status === "done");
            if (latestDone) {
              setSelected(latestDone.id);
              setPicked(true);
            }
          }
        })
        .catch(() => {});
    load();
    const t = setInterval(load, 5000);
    return () => {
      alive = false;
      clearInterval(t);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [picked]);

  // Telemetry for the selection.
  useEffect(() => {
    let alive = true;
    const load = async () => {
      try {
        const sel = selectedRef.current;
        const [t, m] = await Promise.all([
          sel === "all" ? api.telemetry() : api.telemetryForRun(sel),
          api.middleware(),
        ]);
        if (!alive) return;
        setTelemetry(t);
        setCaptured("captured" in t ? (t as { captured: boolean }).captured : true);
        setInventory(m);
        setError(null);
      } catch (e) {
        if (alive) setError(String(e));
      }
    };
    void load();
    if (!live) return;
    const t = setInterval(load, 3000);
    return () => {
      alive = false;
      clearInterval(t);
    };
  }, [selected, live]);

  const selectedRun = useMemo(
    () => (selected === "all" ? null : runs.find((r) => r.id === selected) ?? null),
    [runs, selected],
  );
  const caption =
    selected === "all"
      ? "all traffic since the last reset"
      : selectedRun
        ? `run r-${selectedRun.id} · ${selectedRun.scenario} @ ${selectedRun.vus.toLocaleString()} vus`
        : `run r-${selected}`;

  if (error && !telemetry) {
    return (
      <p className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">
        {error} — is the backend running on :8113?
      </p>
    );
  }
  if (!telemetry || !inventory) {
    return <p className="text-sm text-muted-foreground">Loading telemetry…</p>;
  }

  return (
    <div className="space-y-6">
      <div className="flex flex-wrap items-end justify-between gap-3">
        <div>
          <h1 className="text-xl font-semibold tracking-tight">Where is the time actually going?</h1>
          <p className="mt-1 text-sm text-muted-foreground">
            Pick a run to profile it alone — every request it generated is tagged, so the ledger,
            middleware rows and statements below reflect only that run. "All traffic" is the
            process view since the last reset.
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <select
            value={selected === "all" ? "all" : String(selected)}
            onChange={(e) => {
              setPicked(true);
              setSelected(e.target.value === "all" ? "all" : Number(e.target.value));
            }}
            className="rounded-md border border-border bg-background px-2 py-1.5 font-mono text-xs"
            aria-label="Layer profile source"
          >
            <option value="all">all traffic (since reset)</option>
            {[...runs].reverse().map((r) => (
              <option key={r.id} value={r.id}>
                r-{r.id} · {r.scenario} @ {r.vus.toLocaleString()} vus · {r.status}
              </option>
            ))}
          </select>
          <label className="flex items-center gap-1.5 font-mono text-xs text-muted-foreground">
            <input
              type="checkbox"
              checked={live}
              onChange={(e) => setLive(e.target.checked)}
              className="accent-primary"
            />
            auto-refresh
          </label>
          <button
            onClick={() => void api.resetTelemetry().then(() => setTelemetry(null))}
            title="Clears the process-wide aggregates. Per-run profiles are kept."
            className="rounded-md border border-border px-2.5 py-1 font-mono text-xs text-muted-foreground hover:bg-accent hover:text-accent-foreground"
          >
            reset global
          </button>
        </div>
      </div>

      {!captured ? (
        <section className="rounded-lg border border-border bg-card p-4 sm:p-5">
          <p className="text-sm">
            Run <span className="font-mono">r-{selected}</span> was recorded before per-run capture
            existed (its requests weren't tagged), so there is no layer profile for it. New runs
            record one automatically.
          </p>
          <button
            onClick={() => setSelected("all")}
            className="mt-3 rounded-md border border-border px-3 py-1.5 text-sm font-medium hover:bg-accent hover:text-accent-foreground"
          >
            Show all traffic instead
          </button>
        </section>
      ) : (
        <>
          <LayerLedger telemetry={telemetry} inventory={inventory} caption={caption} />

          <section className="rounded-lg border border-border bg-card p-4 sm:p-5">
            <div className="mb-3 flex flex-wrap items-baseline justify-between gap-2">
              <div>
                <h2 className="text-base font-semibold tracking-tight">Routes</h2>
                <p className="text-sm text-muted-foreground">
                  Request mix and latency per route — {caption}.
                </p>
              </div>
              <span className="font-mono text-xs text-muted-foreground">
                {num(telemetry.total_requests)} requests
              </span>
            </div>
            <div className="overflow-x-auto">
              <table className="w-full min-w-[36rem] text-sm">
                <thead>
                  <tr className="text-xs text-muted-foreground">
                    <th className="py-2 pr-3 text-left font-medium">Route</th>
                    <th className="py-2 pr-3 text-right font-medium">Requests</th>
                    <th className="py-2 pr-3 text-right font-medium">p50</th>
                    <th className="py-2 pr-3 text-right font-medium">p95</th>
                    <th className="py-2 pr-3 text-right font-medium">p99</th>
                    <th className="py-2 text-right font-medium">Errors</th>
                  </tr>
                </thead>
                <tbody className="font-mono tabular-nums">
                  {telemetry.routes.slice(0, 12).map((r) => (
                    <tr key={r.route} className="border-t border-border/60">
                      <td className="py-2 pr-3 text-xs">{r.route}</td>
                      <td className="py-2 pr-3 text-right">{num(r.count)}</td>
                      <td className="py-2 pr-3 text-right">{ms(r.p50_us)}</td>
                      <td className="py-2 pr-3 text-right">{ms(r.p95_us)}</td>
                      <td className="py-2 pr-3 text-right">{ms(r.p99_us)}</td>
                      <td className={`py-2 text-right ${r.errors > 0 ? "text-destructive" : "text-muted-foreground"}`}>
                        {r.errors}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </section>

          <section className="rounded-lg border border-border bg-card p-4 sm:p-5">
            <div className="mb-3 flex flex-wrap items-baseline justify-between gap-2">
              <div>
                <h2 className="text-base font-semibold tracking-tight">Slowest statements</h2>
                <p className="text-sm text-muted-foreground">
                  Fingerprinted over {selected === "all" ? "the whole process" : "this run's window"}.
                </p>
              </div>
              <span className="font-mono text-xs text-muted-foreground">top {telemetry.sql.length} by avg</span>
            </div>
            <div className="overflow-x-auto">
              <table className="w-full min-w-[36rem] text-sm">
                <thead>
                  <tr className="text-xs text-muted-foreground">
                    <th className="py-2 pr-3 text-left font-medium">Statement</th>
                    <th className="py-2 pr-3 text-right font-medium">Calls</th>
                    <th className="py-2 pr-3 text-right font-medium">avg</th>
                    <th className="py-2 text-right font-medium">max</th>
                  </tr>
                </thead>
                <tbody className="font-mono tabular-nums">
                  {telemetry.sql.slice(0, 10).map((s) => (
                    <tr key={s.fingerprint} className="border-t border-border/60">
                      <td className="max-w-[28rem] truncate py-2 pr-3 text-xs" title={s.fingerprint}>
                        {s.sample.replace(/\s+/g, " ").slice(0, 90)}
                      </td>
                      <td className="py-2 pr-3 text-right">{num(s.calls)}</td>
                      <td className="py-2 pr-3 text-right">{ms(s.avg_us)}</td>
                      <td className="py-2 text-right">{ms(s.max_us)}</td>
                    </tr>
                  ))}
                  {telemetry.sql.length === 0 && (
                    <tr>
                      <td colSpan={4} className="py-2 text-xs text-muted-foreground">
                        No statements captured for this selection yet.
                      </td>
                    </tr>
                  )}
                </tbody>
              </table>
            </div>
          </section>

          <section className="rounded-lg border border-border bg-card p-4 sm:p-5">
            <div className="mb-3 flex flex-wrap items-baseline justify-between gap-2">
              <h2 className="text-base font-semibold tracking-tight">N+1 fingerprints</h2>
              {telemetry.n1.length > 0 ? (
                <span className="rounded-md border border-destructive/40 bg-destructive/10 px-2 py-0.5 font-mono text-xs text-destructive">
                  {telemetry.n1.length} flagged
                </span>
              ) : (
                <span className="rounded-md border border-border px-2 py-0.5 font-mono text-xs text-muted-foreground">
                  none
                </span>
              )}
            </div>
            {telemetry.n1.length === 0 && (
              <p className="text-sm text-muted-foreground">
                No repeated-statement bursts detected. Hit{" "}
                <a className="underline underline-offset-2" href="/api/feed-naive?limit=25" target="_blank" rel="noreferrer">
                  /api/feed-naive
                </a>{" "}
                (the <span className="font-mono text-xs">N+1</span> demo endpoint) to see the
                detector work — then re-run a scenario and select it here. (N+1 flags are tracked
                process-wide, not per run.)
              </p>
            )}
            <div className="space-y-2">
              {telemetry.n1.map((f) => (
                <div key={`${f.route}:${f.fingerprint}`} className="flex items-start gap-3 rounded-md border border-border/60 p-3">
                  <span className="mt-1 h-2.5 w-2.5 shrink-0 rounded-sm bg-destructive" />
                  <div className="min-w-0 text-sm">
                    <p className="truncate font-mono text-xs" title={f.fingerprint}>
                      {f.sample.replace(/\s+/g, " ").slice(0, 110)}
                    </p>
                    <p className="mt-1 text-muted-foreground">
                      repeated <span className="font-mono">{f.times}×</span> {f.route}
                      {f.route.startsWith("GET") ? "" : " — candidate fix: select_related / batch load."}
                    </p>
                  </div>
                </div>
              ))}
            </div>
          </section>
        </>
      )}
    </div>
  );
}
