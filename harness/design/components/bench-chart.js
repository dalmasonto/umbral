// bench-chart — the measuring instrument's plot area. ApexCharts (pinned)
// reads its colours from the project tokens, so both themes stay correct.
// modes: "ceiling" (latency vs load, with p95/p99 budget lines),
//        "throughput" (rps vs load), "live" (time-domain during a run).
// Port note: maps to <BenchChart mode="ceiling" data={...}/> — the DATA maps
// become props; option building stays identical.
if (!customElements.get("bench-chart")) {
  const APEX_URL = "https://cdnjs.cloudflare.com/ajax/libs/apexcharts/3.54.1/apexcharts.min.js";

  const loadApex = () => {
    if (window.ApexCharts) return Promise.resolve(window.ApexCharts);
    if (!window.__benchApex) {
      window.__benchApex = new Promise((resolve, reject) => {
        const s = document.createElement("script");
        s.src = APEX_URL;
        s.onload = () => (window.ApexCharts ? resolve(window.ApexCharts) : reject(new Error("ApexCharts missing after load")));
        s.onerror = () => reject(new Error("could not load ApexCharts from cdnjs"));
        document.head.appendChild(s);
      });
    }
    return window.__benchApex;
  };

  const css = (name, fallback) => {
    const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
    return v || fallback;
  };
  const isDark = () =>
    document.documentElement.getAttribute("data-theme") === "dark" ||
    document.documentElement.classList.contains("dark") ||
    (!document.documentElement.getAttribute("data-theme") && matchMedia("(prefers-color-scheme: dark)").matches);

  const VU_LABELS = ["500", "1k", "1.75k", "2.5k", "3.25k", "4k", "4.75k", "5k", "5.12k"];
  const DATA = {
    ceiling: {
      x: VU_LABELS, xTitle: "virtual users", yTitle: "latency (ms)", yMax: 1100,
      series: [
        { name: "SQLite p95", data: [3, 6, 13, 42, 96, 208, 348, 468, 505], color: "chart-1", dash: 0 },
        { name: "SQLite p99", data: [7, 14, 29, 92, 171, 396, 650, 860, 1010], color: "chart-1", dash: 6 },
        { name: "Postgres p95", data: [4, 9, 19, 61, 141, 331, 479, 560, 640], color: "chart-2", dash: 0 },
        { name: "Postgres p99", data: [9, 20, 45, 134, 268, 560, 760, 935, 1100], color: "chart-2", dash: 6 },
      ],
      budgets: [
        { y: 500, label: "p95 budget · 500 ms" },
        { y: 1000, label: "p99 budget · 1 s" },
      ],
    },
    throughput: {
      x: VU_LABELS, xTitle: "virtual users", yTitle: "requests / s", yMax: 4500,
      series: [
        { name: "SQLite", data: [880, 1690, 2540, 3060, 3450, 3820, 4120, 4180, 4210], color: "chart-1", dash: 0 },
        { name: "Postgres", data: [830, 1610, 2380, 2870, 3230, 3510, 3640, 3600, 3560], color: "chart-2", dash: 0 },
      ],
      budgets: [],
    },
    live: {
      x: Array.from({ length: 21 }, (_, i) => i * 0.25), xTitle: "elapsed", yTitle: "p95 (ms)",
      series: [
        { name: "SQLite p95", data: [34, 39, 36, 42, 38, 41, 44, 39, 43, 46, 41, 44, 42, 47, 43, 41, 45, 42, 44, 43, 42], color: "chart-1", dash: 0 },
        { name: "Postgres p95", data: [52, 58, 55, 61, 57, 62, 66, 60, 64, 69, 63, 66, 65, 71, 66, 63, 68, 64, 67, 65, 64], color: "chart-2", dash: 0 },
      ],
      budgets: [{ y: 500, label: "p95 budget · 500 ms" }],
      marker: 3.4,
      clock: true,
    },
  };

  const clock = (v) => {
    const m = Math.floor(v);
    const s = Math.round((v - m) * 60);
    return `${m}:${String(s).padStart(2, "0")}`;
  };

  class BenchChart extends HTMLElement {
    connectedCallback() {
      const mode = this.getAttribute("mode") || "ceiling";
      this.className = "block min-h-64 w-full";
      this.innerHTML = `<div class="grid h-64 place-items-center rounded-lg border border-dashed border-border text-sm text-muted-foreground font-mono">loading ${mode} chart…</div>`;
      this._mode = mode;

      loadApex()
        .then(() => this._render())
        .catch((err) => {
          this.innerHTML = `<div class="grid h-64 place-items-center rounded-lg border border-destructive/40 text-sm text-destructive">chart failed to load: ${err.message}</div>`;
        });

      const flip = () => this._render();
      matchMedia("(prefers-color-scheme: dark)").addEventListener("change", flip);
      this._obs = new MutationObserver(flip);
      this._obs.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme", "class"] });
    }
    disconnectedCallback() { try { this._obs && this._obs.disconnect(); } catch (e) {} }

    _render() {
      if (!window.ApexCharts) return;
      const d = DATA[this._mode];
      if (this._chart) { this._chart.destroy(); this._chart = null; }

      const dark = isDark();
      const c1 = css("--chart-1", "#e39a3b"), c2 = css("--chart-2", "#3b82f6"), c3 = css("--chart-3", "#9333ea");
      const muted = css("--muted-foreground", "#777"), border = css("--border", "#ddd"), fg = css("--foreground", "#111");
      const mono = "'IBM Plex Mono', ui-monospace, monospace";
      const pick = (key) => (key === "chart-1" ? c1 : key === "chart-2" ? c2 : c3);
      const height = this._mode === "ceiling" ? 360 : this._mode === "throughput" ? 300 : 260;
      const categorical = this._mode !== "live";

      this.className = "block w-full";
      this.style.height = height + "px";
      this.innerHTML = "";

      const annotations = {
        yaxis: d.budgets.map((b) => ({
          y: b.y, borderColor: muted, strokeDashArray: 4,
          label: {
            text: b.label, position: "left", textAnchor: "start", offsetX: 6, offsetY: -4,
            style: { background: "transparent", color: muted, fontFamily: mono, fontSize: "11px" },
          },
        })),
        xaxis: [],
      };
      if (d.marker !== undefined) {
        annotations.xaxis.push({
          x: d.marker, borderColor: fg, borderWidth: 2,
          label: { text: "now", orientation: "horizontal", style: { background: "transparent", color: fg, fontFamily: mono, fontSize: "11px" } },
        });
      }

      const options = {
        chart: {
          type: "line", height, fontFamily: mono, background: "transparent",
          toolbar: { show: false }, zoom: { enabled: false },
          animations: { enabled: false }, parentHeightOffset: 0,
          foreColor: muted,
        },
        series: d.series.map((s) => ({
          name: s.name, data: s.data, color: pick(s.color),
          stroke: { width: 2.5, curve: "straight", dashArray: s.dash },
          markers: { size: 0, strokeWidth: 0, hover: { size: 5 } },
        })),
        colors: d.series.map((s) => pick(s.color)),
        legend: { show: false },
        dataLabels: { enabled: false },
        grid: { borderColor: border, strokeDashArray: 3, padding: { left: 10, right: 16, top: 4 } },
        xaxis: categorical
          ? {
              type: "category", categories: d.x,
              title: { text: d.xTitle, style: { fontFamily: mono, fontSize: "11px", color: muted } },
              labels: { style: { fontFamily: mono, fontSize: "11px" }, hideOverlappingLabels: true },
              axisTicks: { show: false }, axisBorder: { color: border },
            }
          : {
              type: "numeric", min: Math.min(...d.x), max: Math.max(...d.x),
              tickAmount: 5, title: { text: d.xTitle, style: { fontFamily: mono, fontSize: "11px", color: muted } },
              labels: { style: { fontFamily: mono, fontSize: "11px" }, formatter: (v) => (d.clock ? clock(v) : v.toFixed(2)) },
              axisTicks: { show: false }, axisBorder: { color: border },
            },
        yaxis: {
          ...(d.yMax ? { max: d.yMax } : {}),
          title: { text: d.yTitle, style: { fontFamily: mono, fontSize: "11px", color: muted } },
          labels: { style: { fontFamily: mono, fontSize: "11px" }, formatter: (v) => Math.round(v) },
        },
        tooltip: { theme: dark ? "dark" : "light", shared: true, intersect: false, style: { fontFamily: mono, fontSize: "12px" } },
        annotations,
      };

      this._chart = new window.ApexCharts(this, options);
      this._chart.render();
    }
  }
  customElements.define("bench-chart", BenchChart);
}
