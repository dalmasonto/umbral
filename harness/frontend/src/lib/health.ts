import type { RunListItem, Telemetry } from "./api";

// Plain-language assessment of "is this holding up?" — thresholds are the
// acceptance criteria the whole harness is built around:
//   p95 < 500 ms · p99 < 1 s · errors < 1%
// Verdict is computed here (not in the backend) so live views and history
// interpret the same numbers the same way.

export const BUDGET_P95_MS = 500;
export const BUDGET_P99_MS = 1000;
export const BUDGET_ERR_PCT = 1;

export type Level = "pass" | "warn" | "fail";

export interface Verdict {
  level: Level;
  label: string;
  lines: string[];
}

export function chipLevel(summary: {
  p95_ms: number;
  p99_ms: number;
  errors: number;
  sent: number;
}): Level {
  const errPct = summary.sent > 0 ? (summary.errors / summary.sent) * 100 : 0;
  if (summary.p95_ms > BUDGET_P95_MS || summary.p99_ms > BUDGET_P99_MS || errPct >= 5) {
    return "fail";
  }
  if (
    summary.p95_ms > BUDGET_P95_MS * 0.8 ||
    summary.p99_ms > BUDGET_P99_MS * 0.8 ||
    errPct >= BUDGET_ERR_PCT
  ) {
    return "warn";
  }
  return "pass";
}

const LABEL: Record<Level, string> = {
  pass: "holding up",
  warn: "getting close",
  fail: "over budget",
};

/** Full assessment for a completed run, with explanatory sentences. */
export function assessRun(
  run: RunListItem,
  allRuns: RunListItem[],
  telemetry: Telemetry | null,
): Verdict {
  const s = run.summary;
  if (!s) {
    return { level: "warn", label: "no data", lines: ["This run has no summary yet."] };
  }
  const level = chipLevel(s);
  const lines: string[] = [];

  // 1. Headroom against the p95 budget.
  const ratio = BUDGET_P95_MS / Math.max(s.p95_ms, 0.1);
  if (s.p95_ms <= BUDGET_P95_MS) {
    lines.push(
      `p95 is ${ratio >= 10 ? ratio.toFixed(0) : ratio.toFixed(1)}× under the 500 ms budget ` +
        `(${s.p95_ms.toFixed(1)} ms) at ${run.vus.toLocaleString()} virtual users — ` +
        (ratio >= 5 ? "comfortable headroom." : "usable, but a higher load level will find the edge."),
    );
  } else {
    lines.push(
      `p95 is over budget: ${s.p95_ms.toFixed(1)} ms vs 500 ms at ${run.vus.toLocaleString()} VUs — ` +
        "this level fails the acceptance bar.",
    );
  }

  // 2. Errors.
  const errPct = s.sent > 0 ? (s.errors / s.sent) * 100 : 0;
  if (s.errors === 0) {
    lines.push(`No failed requests in ${s.sent.toLocaleString()} sent.`);
  } else {
    lines.push(
      `${errPct.toFixed(2)}% of requests failed (${s.errors.toLocaleString()}) — ` +
        "open this run's Failures panel for status codes and sample bodies.",
    );
  }

  // 3. Where the time goes (server-side layer profiler, all captured traffic).
  if (telemetry && telemetry.total_requests > 0) {
    const total = telemetry.layers.reduce((a, l) => a + l.avg_us, 0);
    const top = telemetry.layers.reduce((a, l) => (l.avg_us > a.avg_us ? l : a), telemetry.layers[0]);
    if (total > 0) {
      const share = (top.avg_us / total) * 100;
      const naming: Record<string, string> = {
        database: "the database",
        middleware: "middleware",
        routing: "routing",
        render: "template rendering",
        serialize: "JSON serialization",
        handler_logic: "handler logic",
        query_build: "ORM/pool overhead",
        tail: "the response path",
      };
      lines.push(
        `The layer profiler puts the largest share of each request in ` +
          `${naming[top.layer] ?? top.layer} (${share.toFixed(0)}%) — that's the lever to pull first.`,
      );
    }
  }

  // 4. Ceiling trend: compare the two highest completed levels of this
  //    scenario on this backend.
  const same = allRuns
    .filter((r) => r.scenario === run.scenario && r.backend === run.backend && r.summary)
    .sort((a, b) => a.vus - b.vus);
  const below = same.filter((r) => r.vus < run.vus);
  const prev = below.at(-1);
  if (prev?.summary && run.summary) {
    const growth = run.summary.rps_avg / Math.max(prev.summary.rps_avg, 1);
    if (growth < 1.03) {
      lines.push(
        `Throughput has plateaued: ${prev.vus.toLocaleString()} → ${run.vus.toLocaleString()} VUs ` +
          `only moved req/s ${prev.summary.rps_avg.toFixed(0)} → ${run.summary.rps_avg.toFixed(0)}. ` +
          "You're at this backend's ceiling — more load will only add latency, not throughput.",
      );
    } else {
      lines.push(
        `Still climbing: ${prev.vus.toLocaleString()} → ${run.vus.toLocaleString()} VUs grew ` +
          `req/s ${prev.summary.rps_avg.toFixed(0)} → ${run.summary.rps_avg.toFixed(0)} (${((growth - 1) * 100).toFixed(0)}%). ` +
          "The ceiling is further up.",
      );
    }
  } else {
    lines.push("Run one more level (e.g. 2× the VUs) so the ceiling trend has two points to compare.");
  }

  return { level, label: LABEL[level], lines };
}
