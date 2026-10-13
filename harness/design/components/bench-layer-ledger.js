// bench-layer-ledger — the signature instrument: where a request's time goes,
// per backend, in pipeline order, with the Middleware segment expandable into
// each middleware's own time. Port note: maps to <LayerLedger run={...}/>
// (DATA becomes a prop; segment widths become inline styles in JSX).
//
// Encoding: segments are a NEUTRAL sequential ramp (pipeline order), so the two
// backend hues (chart-1/2) stay the only strong colours on the card; the
// dominant layer per backend gets a ring + full-contrast label. Middleware
// rows reuse the backend's hue — inside that segment, the hue IS the identity.
const LAYERS = ["Middleware", "Routing", "Query build", "Database", "Serialize", "Render"];
const RAMP = [15, 28, 40, 55, 70, 85]; // bg-foreground opacity steps, pipeline order
const LEDGER = {
  scenario: "feed (read-heavy, 8% writes)",
  run: "r-248 · 2,500 virtual users · 5:00",
  backends: [
    {
      key: "sqlite", name: "SQLite", dot: "bg-chart-1", bar: "bg-chart-1", p95: 42.1,
      ms: [5.9, 2.1, 3.4, 13.9, 3.8, 13.0],
      delta: [-0.1, 0.0, 0.2, 0.8, -0.1, -0.6], // vs r-247, ms
      middlewares: [
        { name: "Host guard", ms: 0.4 },
        { name: "Security headers", ms: 0.6 },
        { name: "CSRF", ms: 0.5 },
        { name: "Session", ms: 2.1 },
        { name: "Request log", ms: 0.9 },
        { name: "After hooks", ms: 1.4 },
      ],
    },
    {
      key: "postgres", name: "Postgres", dot: "bg-chart-2", bar: "bg-chart-2", p95: 61.0,
      ms: [7.9, 2.4, 4.3, 28.7, 4.9, 12.8],
      delta: [0.3, 0.1, 0.3, 6.1, -0.2, -0.5],
      middlewares: [
        { name: "Host guard", ms: 0.5 },
        { name: "Security headers", ms: 0.8 },
        { name: "CSRF", ms: 0.7 },
        { name: "Session", ms: 2.9 },
        { name: "Request log", ms: 1.2 },
        { name: "After hooks", ms: 1.8 },
      ],
    },
  ],
};

class BenchLayerLedger extends HTMLElement {
  connectedCallback() {
    if (this.dataset.ready) return;
    this.dataset.ready = "1";

    const pct = (b) => b.ms.map((m) => (m / b.p95) * 100);
    const dominant = (b) => pct(b).indexOf(Math.max(...pct(b)));

    const bar = (b) => {
      const p = pct(b);
      const dom = dominant(b);
      return `
        <div class="grid grid-cols-[7.5rem_1fr] items-center gap-4">
          <div class="flex items-center gap-2">
            <span class="h-2.5 w-2.5 shrink-0 rounded-sm ${b.dot}"></span>
            <span class="text-sm font-medium">${b.name}</span>
          </div>
          <div class="min-w-0">
            <div class="flex h-7 w-full overflow-hidden rounded-md border border-border" role="img"
                 aria-label="${b.name}: ${LAYERS.map((l, i) => `${l} ${p[i].toFixed(0)}%`).join(", ")}">
              ${LAYERS.map(
                (l, i) => `<div class="ledger-seg h-full bg-foreground/${RAMP[i]} ${i > 0 ? "border-l border-card" : ""} ${i === dom ? "outline-2 -outline-offset-2 outline-foreground/80 outline" : ""}"
                    data-w="${p[i].toFixed(2)}" title="${l}: ${b.ms[i].toFixed(1)} ms (${p[i].toFixed(0)}%)"></div>`
              ).join("")}
            </div>
            <div class="mt-1 flex justify-between text-xs text-muted-foreground font-mono">
              <span>p95 ${b.p95.toFixed(1)} ms</span>
              <span>${LAYERS.map((l, i) => `${p[i].toFixed(0)}%`).join(" · ")}</span>
            </div>
          </div>
        </div>`;
    };

    const rows = LAYERS.map((l, i) => {
      const s = LEDGER.backends[0], g = LEDGER.backends[1];
      const fmt = (b) => `${b.ms[i].toFixed(1)} ms <span class="text-muted-foreground">· ${pct(b)[i].toFixed(0)}%</span>`;
      const d = (v) =>
        v === 0
          ? `<span class="text-muted-foreground">±0.0</span>`
          : `<span class="${v > 1 ? "text-destructive" : "text-muted-foreground"}">${v > 0 ? "+" : ""}${v.toFixed(1)}</span>`;
      return `<tr class="border-t border-border/60">
        <td class="py-1.5 pr-3 font-medium">
          <span class="mr-2 inline-block h-2 w-2 rounded-sm bg-foreground/${RAMP[i]} align-middle"></span>${l}
        </td>
        <td class="py-1.5 pr-3 text-right font-mono tabular-nums">${fmt(s)}</td>
        <td class="py-1.5 pr-3 text-right font-mono tabular-nums">${fmt(g)}</td>
        <td class="py-1.5 text-right font-mono tabular-nums">${d(s.delta[i])} <span class="text-muted-foreground">/</span> ${d(g.delta[i])}</td>
      </tr>`;
    }).join("");

    const mwPanel = (b, open) => {
      const total = b.middlewares.reduce((a, m) => a + m.ms, 0);
      const worst = Math.max(...b.middlewares.map((m) => m.ms));
      return `
        <details ${open ? "open" : ""} class="rounded-md border border-border/60">
          <summary class="flex cursor-pointer items-center gap-2 px-3 py-2 text-sm">
            <span class="h-2.5 w-2.5 shrink-0 rounded-sm ${b.dot}"></span>
            <span class="font-medium">${b.name}</span>
            <span class="ml-auto font-mono text-xs text-muted-foreground">${total.toFixed(1)} ms in ${b.middlewares.length} middlewares</span>
          </summary>
          <div class="space-y-2 border-t border-border/60 px-3 py-3">
            ${b.middlewares
              .map(
                (m) => `<div class="grid grid-cols-[8.5rem_1fr_3.5rem] items-center gap-3 text-xs">
                  <span class="${m.ms === worst ? "font-medium text-foreground" : "text-muted-foreground"}">${m.name}${m.ms === worst ? " ·" : ""}</span>
                  <span class="h-1.5 w-full overflow-hidden rounded-full bg-muted">
                    <span class="mw-fill block h-full rounded-full ${b.bar}/70" data-w="${((m.ms / total) * 100).toFixed(1)}"></span>
                  </span>
                  <span class="text-right font-mono tabular-nums">${m.ms.toFixed(1)} ms</span>
                </div>`
              )
              .join("")}
            <p class="pt-1 text-xs text-muted-foreground">Measured per middleware through the beacon seam — every hook's own before + after time, plugin layers included.</p>
          </div>
        </details>`;
    };

    const dbPct = (b) => (b.ms[3] / b.p95) * 100;
    const s = LEDGER.backends[0], g = LEDGER.backends[1];
    const gap = dbPct(g) - dbPct(s);

    this.className = "block";
    this.innerHTML = `
      <section class="rounded-lg border border-border bg-card p-4 sm:p-5">
        <div class="mb-4 flex flex-wrap items-baseline justify-between gap-2">
          <div>
            <h2 class="text-base font-semibold tracking-tight">Where the time goes</h2>
            <p class="text-sm text-muted-foreground">${LEDGER.scenario} — server-side p95 decomposed in pipeline order.</p>
          </div>
          <span class="text-xs font-mono text-muted-foreground">${LEDGER.run}</span>
        </div>

        <div class="space-y-4">${LEDGER.backends.map(bar).join("")}</div>

        <div class="mt-4 flex items-start gap-2 rounded-md bg-muted px-3 py-2 text-sm">
          <span class="mt-0.5 h-4 w-1 shrink-0 rounded-full bg-chart-2"></span>
          <p>Postgres spends <span class="font-mono">${dbPct(g).toFixed(0)}%</span> of its p95 in the database layer —
          <span class="font-mono">${gap.toFixed(0)}</span> points more than SQLite. That gap is pool checkout plus row
          decode; the query builder itself costs the same on both.</p>
        </div>

        <div class="mt-5">
          <h3 class="text-sm font-medium">Middleware, expanded</h3>
          <p class="mt-0.5 text-xs text-muted-foreground">Open a backend to see every middleware's own time.</p>
          <div class="mt-3 grid gap-2 sm:grid-cols-2">
            ${mwPanel(s, true)}
            ${mwPanel(g, false)}
          </div>
        </div>

        <div class="mt-5 overflow-x-auto">
          <table class="w-full min-w-[34rem] text-sm">
            <thead>
              <tr class="text-xs text-muted-foreground">
                <th class="py-1.5 pr-3 text-left font-medium">Layer</th>
                <th class="py-1.5 pr-3 text-right font-medium">SQLite p95</th>
                <th class="py-1.5 pr-3 text-right font-medium">Postgres p95</th>
                <th class="py-1.5 text-right font-medium">Δ vs r-247 <span class="font-mono">SQLite / PG</span></th>
              </tr>
            </thead>
            <tbody>${rows}</tbody>
          </table>
        </div>
      </section>`;

    // widths after insertion (keeps markup portable — React sets style directly)
    this.querySelectorAll(".ledger-seg, .mw-fill").forEach((el) => {
      el.style.width = el.dataset.w + "%";
    });
  }
}
customElements.define("bench-layer-ledger", BenchLayerLedger);
