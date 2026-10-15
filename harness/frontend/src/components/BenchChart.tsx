import { useEffect, useRef } from "react";
import ApexCharts from "apexcharts";

// Chart wrapper reading its colours from the CSS tokens (both themes stay
// correct — chart hues are pinned in index.css). Ported from the design
// surface component; the DATA consts there became the `series` prop here.
export interface Series {
  name: string;
  data: number[];
  /** CSS custom property name, e.g. "--chart-1". */
  varName: string;
  dash?: number;
}

interface Props {
  series: Series[];
  x: (string | number)[];
  xTitle: string;
  yTitle: string;
  yMax?: number;
  budgets?: { y: number; label: string }[];
  numericX?: boolean;
  xFormatter?: (v: number) => string;
  marker?: number;
  height?: number;
}

const cssVar = (name: string, fallback: string): string => {
  const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return v || fallback;
};

const isDark = (): boolean =>
  document.documentElement.classList.contains("dark") ||
  window.matchMedia("(prefers-color-scheme: dark)").matches;

export default function BenchChart({
  series,
  x,
  xTitle,
  yTitle,
  yMax,
  budgets = [],
  numericX = false,
  xFormatter,
  marker,
  height = 320,
}: Props) {
  const ref = useRef<HTMLDivElement | null>(null);
  const chart = useRef<ApexCharts | null>(null);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;

    const render = () => {
      chart.current?.destroy();
      const mono = "'IBM Plex Mono', ui-monospace, monospace";
      const muted = cssVar("--muted-foreground", "#71717a");
      const border = cssVar("--border", "#e4e4e7");
      const fg = cssVar("--foreground", "#18181b");
      const colors = series.map((s) => cssVar(s.varName, "#888"));

      const annotations = {
        yaxis: budgets.map((b) => ({
          y: b.y,
          borderColor: muted,
          strokeDashArray: 4,
          label: {
            text: b.label,
            position: "left" as const,
            textAnchor: "start" as const,
            offsetX: 6,
            offsetY: -4,
            style: {
              background: "transparent",
              color: muted,
              fontFamily: mono,
              fontSize: "11px",
            },
          },
        })),
        xaxis:
          marker !== undefined
            ? [
                {
                  x: marker,
                  borderColor: fg,
                  borderWidth: 2,
                  label: {
                    text: "now",
                    orientation: "horizontal" as const,
                    style: {
                      background: "transparent",
                      color: fg,
                      fontFamily: mono,
                      fontSize: "11px",
                    },
                  },
                },
              ]
            : [],
      };

      chart.current = new ApexCharts(el, {
        chart: {
          type: "line",
          height,
          fontFamily: mono,
          background: "transparent",
          toolbar: { show: false },
          zoom: { enabled: false },
          animations: { enabled: false },
          parentHeightOffset: 0,
          foreColor: muted,
        },
        series: series.map((s, i) => ({
          name: s.name,
          data: s.data,
          color: colors[i],
          stroke: { width: 2.5, curve: "straight", dashArray: s.dash ?? 0 },
          markers: { size: 0, strokeWidth: 0, hover: { size: 5 } },
        })),
        colors,
        legend: { show: false },
        dataLabels: { enabled: false },
        grid: { borderColor: border, strokeDashArray: 3, padding: { left: 10, right: 16, top: 4 } },
        xaxis: numericX
          ? {
              type: "numeric",
              min: x.length ? Math.min(...(x as number[])) : 0,
              max: x.length ? Math.max(...(x as number[])) : 1,
              tickAmount: 5,
              title: { text: xTitle, style: { fontFamily: mono, fontSize: "11px", color: muted } },
              labels: {
                style: { fontFamily: mono, fontSize: "11px" },
                formatter: (v: string) => (xFormatter ? xFormatter(Number(v)) : Number(v).toFixed(1)),
              },
              axisTicks: { show: false },
              axisBorder: { color: border },
            }
          : {
              type: "category",
              categories: x,
              title: { text: xTitle, style: { fontFamily: mono, fontSize: "11px", color: muted } },
              labels: { style: { fontFamily: mono, fontSize: "11px" }, hideOverlappingLabels: true },
              axisTicks: { show: false },
              axisBorder: { color: border },
            },
        yaxis: {
          ...(yMax ? { max: yMax } : {}),
          title: { text: yTitle, style: { fontFamily: mono, fontSize: "11px", color: muted } },
          labels: { style: { fontFamily: mono, fontSize: "11px" }, formatter: (v: number) => Math.round(v).toString() },
        },
        tooltip: {
          theme: isDark() ? "dark" : "light",
          shared: true,
          intersect: false,
          style: { fontFamily: mono, fontSize: "12px" },
        },
        annotations,
      });
      void chart.current.render();
    };

    render();
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    mq.addEventListener("change", render);
    return () => {
      mq.removeEventListener("change", render);
      chart.current?.destroy();
      chart.current = null;
    };
  }, [series, x, xTitle, yTitle, yMax, budgets, numericX, xFormatter, marker, height]);

  return <div ref={ref} className="w-full" style={{ minHeight: height }} />;
}
