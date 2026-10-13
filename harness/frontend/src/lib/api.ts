// API client + shared types for the bench backend (see harness/backend/src/api.rs).
// All requests go through Vite's /api proxy (vite.config.ts).

export interface Sample {
  sec: number;
  rps: number;
  sent: number;
  errors: number;
  p50_ms: number;
  p95_ms: number;
  p99_ms: number;
  server_p95_ms: number;
}

export interface ErrorKind {
  status: number;
  path: string;
  count: number;
  sample: string;
}

export interface RunSummary {
  rps_avg: number;
  p50_ms: number;
  p95_ms: number;
  p99_ms: number;
  errors: number;
  sent: number;
  error_breakdown?: ErrorKind[];
}

export interface RunRecord {
  id: number;
  scenario: string;
  vus: number;
  duration_s: number;
  backend: string;
  status: string;
  started_at: string;
  samples: Sample[];
  summary: RunSummary | null;
}

export interface RunListItem {
  id: number;
  scenario: string;
  vus: number;
  duration_s: number;
  backend: string;
  status: string;
  started_at: string;
  summary: RunSummary | null;
}

export interface LayerStat {
  layer: string;
  count: number;
  avg_us: number;
}

export interface MwStat {
  name: string;
  count: number;
  avg_us: number;
  max_us: number;
}

export interface RouteStat {
  route: string;
  count: number;
  errors: number;
  avg_us: number;
  p50_us: number;
  p95_us: number;
  p99_us: number;
  max_us: number;
}

export interface SqlStat {
  fingerprint: string;
  sample: string;
  calls: number;
  avg_us: number;
  max_us: number;
}

export interface N1Flag {
  route: string;
  fingerprint: string;
  times: number;
  sample: string;
}

export interface Telemetry {
  backend: string;
  total_requests: number;
  routes: RouteStat[];
  middleware: MwStat[];
  layers: LayerStat[];
  sql: SqlStat[];
  n1: N1Flag[];
}

export interface MwSpec {
  name: string;
  order: number | null;
  source: string;
}

export interface MiddlewareInventory {
  chain: MwSpec[];
  measured: MwStat[];
}

export interface SampleEvent {
  run_id: number;
  status: string;
  sample: Sample | null;
  summary: RunSummary | null;
  new_errors?: ErrorKind[];
}

async function get<T>(path: string): Promise<T> {
  const res = await fetch(path);
  if (!res.ok) throw new Error(`${path} -> ${res.status}`);
  return (await res.json()) as T;
}

export interface LabSettings {
  fake_middleware_us: number;
  rate_limit_per_sec: number;
}

export interface RunTelemetry extends Telemetry {
  /** false when the run predates per-run capture (or sent no requests). */
  captured: boolean;
}

export const api = {
  telemetry: () => get<Telemetry>("/api/bench/telemetry"),
  telemetryForRun: (id: number) => get<RunTelemetry>(`/api/bench/runs/${id}/telemetry`),
  lab: () => get<LabSettings>("/api/bench/lab"),
  setLab: async (patch: Partial<LabSettings>): Promise<LabSettings> => {
    const res = await fetch("/api/bench/lab", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(patch),
    });
    if (!res.ok) throw new Error(await res.text());
    return (await res.json()) as LabSettings;
  },
  middleware: () => get<MiddlewareInventory>("/api/bench/middleware"),
  runs: () => get<{ runs: RunListItem[] }>("/api/bench/runs"),
  run: (id: number) => get<RunRecord>(`/api/bench/runs/${id}`),
  resetTelemetry: async () => {
    await fetch("/api/bench/reset", { method: "POST" });
  },
  startRun: async (scenario: string, vus: number, duration_s: number): Promise<RunRecord> => {
    const res = await fetch("/api/bench/runs", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ scenario, vus, duration_s }),
    });
    if (!res.ok) throw new Error(await res.text());
    return (await res.json()) as RunRecord;
  },
  /** SSE subscription for a run (or all runs when id is omitted). */
  stream: (runId: number | null, onEvent: (ev: SampleEvent) => void): (() => void) => {
    const url = runId == null ? "/api/bench/stream" : `/api/bench/stream?run=${runId}`;
    const es = new EventSource(url);
    es.addEventListener("sample", (e) => {
      try {
        onEvent(JSON.parse((e as MessageEvent).data) as SampleEvent);
      } catch {
        /* ignore malformed frames */
      }
    });
    return () => es.close();
  },
};

// --- formatting helpers ----------------------------------------------------

export function ms(us: number): string {
  return `${(us / 1000).toFixed(us >= 100_000 ? 0 : 1)} ms`;
}

export function pct(part: number, whole: number): number {
  return whole > 0 ? Math.max(0, Math.min(100, (part / whole) * 100)) : 0;
}

export function num(n: number): string {
  return n.toLocaleString("en-US");
}
