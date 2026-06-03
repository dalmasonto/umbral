import { useMemo } from "react";
import {
  Area,
  AreaChart,
  CartesianGrid,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";
import { Activity, AlertTriangle, CheckCircle2, Clock, Gauge, HardDrive } from "lucide-react";
import type { ResponseRecord } from "@/state/store";

/** Percentile of a sorted ascending number array. Returns 0 for an
 *  empty input. Linear interpolation between adjacent samples — fine
 *  for the playground's small-N use case (rarely more than a few
 *  hundred records per op). */
function percentile(sorted: number[], p: number): number {
  if (sorted.length === 0) return 0;
  if (sorted.length === 1) return sorted[0];
  const idx = (p / 100) * (sorted.length - 1);
  const lo = Math.floor(idx);
  const hi = Math.ceil(idx);
  if (lo === hi) return sorted[lo];
  const frac = idx - lo;
  return sorted[lo] + (sorted[hi] - sorted[lo]) * frac;
}

function fmtMs(ms: number): string {
  if (ms < 1) return "<1 ms";
  if (ms < 1000) return `${Math.round(ms)} ms`;
  return `${(ms / 1000).toFixed(2)} s`;
}

function fmtBytes(b: number): string {
  if (b < 1024) return `${b} B`;
  if (b < 1024 * 1024) return `${(b / 1024).toFixed(1)} KB`;
  return `${(b / 1024 / 1024).toFixed(2)} MB`;
}

function statusBucket(status: number, error?: string): string {
  if (error) return "ERR";
  if (status >= 200 && status < 300) return "2xx";
  if (status >= 300 && status < 400) return "3xx";
  if (status >= 400 && status < 500) return "4xx";
  if (status >= 500) return "5xx";
  return "??";
}

function statusBucketColor(bucket: string): string {
  switch (bucket) {
    case "2xx":
      return "bg-emerald-500";
    case "3xx":
      return "bg-amber-500";
    case "4xx":
      return "bg-rose-500";
    case "5xx":
      return "bg-rose-700";
    case "ERR":
      return "bg-rose-600";
    default:
      return "bg-muted-foreground/50";
  }
}

interface StatCardProps {
  icon: React.ReactNode;
  label: string;
  value: string;
  hint?: string;
}

function StatCard({ icon, label, value, hint }: StatCardProps) {
  return (
    <div className="rounded-md border border-border bg-background p-2.5">
      <div className="flex items-center gap-1.5 text-muted-foreground">
        {icon}
        <p className="text-[10px] font-semibold uppercase tracking-wider">
          {label}
        </p>
      </div>
      <p className="mt-1 font-mono text-base font-semibold tabular-nums text-foreground">
        {value}
      </p>
      {hint && (
        <p className="mt-0.5 text-[10px] text-muted-foreground/80">{hint}</p>
      )}
    </div>
  );
}

interface EndpointStatsProps {
  opHistory: ResponseRecord[];
}

export function EndpointStats({ opHistory }: EndpointStatsProps) {
  const stats = useMemo(() => {
    if (opHistory.length === 0) return null;

    const durations = opHistory.map((r) => r.durationMs).filter((d) => d >= 0);
    const sortedDurations = [...durations].sort((a, b) => a - b);
    const successCount = opHistory.filter(
      (r) => !r.error && r.status >= 200 && r.status < 300,
    ).length;
    const errCount = opHistory.filter(
      (r) => r.error || r.status === 0 || r.status >= 400,
    ).length;
    const totalBytes = opHistory.reduce((sum, r) => sum + (r.sizeBytes ?? 0), 0);

    const buckets = new Map<string, number>();
    for (const r of opHistory) {
      const b = statusBucket(r.status, r.error);
      buckets.set(b, (buckets.get(b) ?? 0) + 1);
    }
    const statusBuckets = Array.from(buckets.entries())
      .map(([bucket, count]) => ({ bucket, count }))
      .sort((a, b) => a.bucket.localeCompare(b.bucket));

    const codeCounts = new Map<number, number>();
    for (const r of opHistory) {
      if (r.error || r.status === 0) continue;
      codeCounts.set(r.status, (codeCounts.get(r.status) ?? 0) + 1);
    }
    const statusCodes = Array.from(codeCounts.entries())
      .map(([code, count]) => ({ code, count }))
      .sort((a, b) => a.code - b.code);

    // Timeline data: every record indexed in chronological order, plus
    // a moving-average column to give the eye a trend line.
    const sortedByTime = [...opHistory].sort((a, b) => a.timestamp - b.timestamp);
    const window = Math.max(1, Math.min(5, Math.floor(sortedByTime.length / 4)));
    const timeline = sortedByTime.map((r, i) => {
      const sliceStart = Math.max(0, i - window + 1);
      const slice = sortedByTime.slice(sliceStart, i + 1);
      const avg = slice.reduce((s, x) => s + x.durationMs, 0) / slice.length;
      return {
        idx: i + 1,
        ms: r.durationMs,
        avg: Math.round(avg),
        ts: r.timestamp,
        status: r.status,
        ok: !r.error && r.status >= 200 && r.status < 400,
      };
    });

    const fastest = sortedDurations[0] ?? 0;
    const slowest = sortedDurations[sortedDurations.length - 1] ?? 0;
    const avg = durations.reduce((s, d) => s + d, 0) / Math.max(1, durations.length);

    return {
      total: opHistory.length,
      successCount,
      errCount,
      successRate: (successCount / opHistory.length) * 100,
      avg,
      p50: percentile(sortedDurations, 50),
      p95: percentile(sortedDurations, 95),
      p99: percentile(sortedDurations, 99),
      fastest,
      slowest,
      avgBytes: totalBytes / opHistory.length,
      lastTimestamp: sortedByTime[sortedByTime.length - 1].timestamp,
      statusBuckets,
      statusCodes,
      timeline,
    };
  }, [opHistory]);

  if (!stats) {
    return (
      <div className="flex flex-col items-center justify-center h-full text-muted-foreground py-12">
        <Activity className="size-8 opacity-40 mb-2" />
        <p className="text-xs font-medium">No data yet</p>
        <p className="text-[10px] text-muted-foreground/60 mt-1 max-w-[20rem] text-center">
          Send a request to start collecting timing, status, and throughput
          stats for this endpoint.
        </p>
      </div>
    );
  }

  const successTint =
    stats.successRate >= 99
      ? "text-emerald-600"
      : stats.successRate >= 90
        ? "text-amber-600"
        : "text-rose-600";

  return (
    <div className="space-y-4">
      {/* Key metrics row */}
      <div className="grid grid-cols-2 lg:grid-cols-4 gap-2">
        <StatCard
          icon={<Activity className="size-3" />}
          label="Requests"
          value={String(stats.total)}
          hint={`last ${new Date(stats.lastTimestamp).toLocaleTimeString()}`}
        />
        <StatCard
          icon={<Clock className="size-3" />}
          label="Avg duration"
          value={fmtMs(stats.avg)}
          hint={`p50 ${fmtMs(stats.p50)}`}
        />
        <StatCard
          icon={<Gauge className="size-3" />}
          label="p95 / p99"
          value={`${fmtMs(stats.p95)} / ${fmtMs(stats.p99)}`}
          hint={`fastest ${fmtMs(stats.fastest)} · slowest ${fmtMs(stats.slowest)}`}
        />
        <StatCard
          icon={
            stats.successRate >= 90 ? (
              <CheckCircle2 className={`size-3 ${successTint}`} />
            ) : (
              <AlertTriangle className={`size-3 ${successTint}`} />
            )
          }
          label="Success rate"
          value={`${stats.successRate.toFixed(1)}%`}
          hint={`${stats.successCount} ok · ${stats.errCount} err`}
        />
      </div>

      {/* Duration over time */}
      <section className="space-y-2 rounded-md border border-border bg-background p-3">
        <div className="flex items-center gap-1.5">
          <Clock className="size-3.5 text-primary" />
          <p className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
            Duration over time
          </p>
          <span className="ml-auto text-[10px] text-muted-foreground/70">
            ms per request · {stats.timeline.length} sample
            {stats.timeline.length === 1 ? "" : "s"}
          </span>
        </div>
        <div className="h-44 w-full">
          <ResponsiveContainer width="100%" height="100%">
            <AreaChart
              data={stats.timeline}
              margin={{ top: 4, right: 4, left: -16, bottom: 0 }}
            >
              <defs>
                <linearGradient id="dur-fill" x1="0" y1="0" x2="0" y2="1">
                  <stop
                    offset="0%"
                    stopColor="currentColor"
                    stopOpacity={0.18}
                  />
                  <stop
                    offset="100%"
                    stopColor="currentColor"
                    stopOpacity={0}
                  />
                </linearGradient>
              </defs>
              <CartesianGrid
                strokeDasharray="3 3"
                vertical={false}
                stroke="hsl(var(--border))"
                strokeOpacity={0.6}
              />
              <XAxis
                dataKey="idx"
                tickLine={false}
                axisLine={false}
                fontSize={10}
                stroke="hsl(var(--muted-foreground))"
                interval="preserveStartEnd"
              />
              <YAxis
                tickLine={false}
                axisLine={false}
                fontSize={10}
                stroke="hsl(var(--muted-foreground))"
                width={36}
                tickFormatter={(v: number) =>
                  v >= 1000 ? `${(v / 1000).toFixed(1)}s` : `${Math.round(v)}`
                }
              />
              <Tooltip
                cursor={{ stroke: "currentColor", strokeOpacity: 0.2 }}
                content={({ active, payload }) => {
                  if (!active || !payload || payload.length === 0) return null;
                  const d = payload[0].payload as (typeof stats.timeline)[number];
                  return (
                    <div className="rounded-md border border-border bg-popover px-2 py-1.5 text-[11px] shadow-md">
                      <div className="font-mono text-foreground">
                        #{d.idx} · {fmtMs(d.ms)}
                      </div>
                      <div className="text-muted-foreground">
                        {new Date(d.ts).toLocaleTimeString()} ·{" "}
                        {d.ok ? "ok" : "error"}
                      </div>
                      <div className="text-muted-foreground">
                        avg(window) {fmtMs(d.avg)}
                      </div>
                    </div>
                  );
                }}
              />
              <Area
                type="monotone"
                dataKey="ms"
                stroke="currentColor"
                strokeWidth={1.5}
                fill="url(#dur-fill)"
                isAnimationActive={false}
                className="text-primary"
              />
              <Area
                type="monotone"
                dataKey="avg"
                stroke="currentColor"
                strokeWidth={1}
                strokeDasharray="3 3"
                fill="none"
                isAnimationActive={false}
                className="text-muted-foreground"
              />
            </AreaChart>
          </ResponsiveContainer>
        </div>
      </section>

      {/* Status distribution */}
      <section className="space-y-2 rounded-md border border-border bg-background p-3">
        <div className="flex items-center gap-1.5">
          <CheckCircle2 className="size-3.5 text-primary" />
          <p className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
            Status distribution
          </p>
          <span className="ml-auto text-[10px] text-muted-foreground/70">
            HTTP class · exact codes
          </span>
        </div>
        <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
          {/* Class buckets (2xx/3xx/4xx/5xx/ERR) */}
          <div className="space-y-1.5">
            {stats.statusBuckets.map(({ bucket, count }) => {
              const pct = (count / stats.total) * 100;
              return (
                <div key={bucket} className="space-y-0.5">
                  <div className="flex items-center justify-between text-[10px]">
                    <span className="font-mono font-semibold text-foreground">
                      {bucket}
                    </span>
                    <span className="font-mono text-muted-foreground tabular-nums">
                      {count} · {pct.toFixed(0)}%
                    </span>
                  </div>
                  <div className="h-1.5 w-full rounded-full bg-muted overflow-hidden">
                    <div
                      className={`h-full ${statusBucketColor(bucket)} transition-[width] duration-200`}
                      style={{ width: `${pct}%` }}
                    />
                  </div>
                </div>
              );
            })}
          </div>
          {/* Exact codes */}
          <div className="space-y-1">
            {stats.statusCodes.length === 0 ? (
              <p className="text-[10px] italic text-muted-foreground">
                No completed responses recorded.
              </p>
            ) : (
              stats.statusCodes.map(({ code, count }) => (
                <div
                  key={code}
                  className="flex items-center justify-between rounded border border-border/60 px-2 py-1 text-[10px]"
                >
                  <span className="font-mono font-semibold text-foreground">
                    {code}
                  </span>
                  <span className="font-mono text-muted-foreground tabular-nums">
                    {count}
                  </span>
                </div>
              ))
            )}
          </div>
        </div>
      </section>

      {/* Throughput / footer */}
      <section className="rounded-md border border-border bg-muted/20 p-3 text-[11px]">
        <div className="flex flex-wrap items-center gap-x-4 gap-y-1.5">
          <span className="flex items-center gap-1.5">
            <HardDrive className="size-3 text-muted-foreground" />
            <span className="text-muted-foreground">Avg response size:</span>
            <span className="font-mono text-foreground">
              {fmtBytes(stats.avgBytes)}
            </span>
          </span>
          <span className="flex items-center gap-1.5">
            <Activity className="size-3 text-muted-foreground" />
            <span className="text-muted-foreground">Latest sample:</span>
            <span className="font-mono text-foreground">
              {new Date(stats.lastTimestamp).toLocaleString()}
            </span>
          </span>
        </div>
      </section>
    </div>
  );
}
