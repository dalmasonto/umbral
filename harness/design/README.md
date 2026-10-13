# umbral bench — design export

The UI design for the benchmark harness, exported from the TaskFlow design surface (project 8, "Bench" group) so it can be ported into `harness/frontend/` (React + Vite) and iterated on locally. This directory is the **design source of truth**: the screenshots below were captured from exactly these files.

## Layout

```
harness/design/
├── tokens.css                  # shadcn globals.css — paste into the React app as-is
├── pages/                      # one HTML fragment per route (body content only)
│   ├── bench.html              # "/bench"         — score cards, latency & throughput charts, scenario matrix
│   ├── bench-layers.html       # "/bench-layers"  — layer ledger, slowest statements, N+1 fingerprints
│   ├── bench-live.html         # "/bench-live"    — live p95 chart, ceiling search, machine meters
│   └── bench-runs.html         # "/bench-runs"    — run comparison + run history
├── components/                 # custom elements (the JS lives here, never in pages)
│   ├── bench-app-header.js     # sticky bar: nav, backend legend, run + version chips
│   ├── bench-chart.js          # ApexCharts wrapper — modes: ceiling | throughput | live
│   └── bench-layer-ledger.js   # the signature instrument (see "Load-bearing decisions")
└── screenshots/
    └── bench-layers-light-dark.png   # light+dark capture of the ledger incl. expandable middleware
```

## Port recipe (design surface → React + Vite)

1. `tokens.css` → `src/index.css` unchanged. It is a Tailwind v4 `globals.css` (`:root`, dark theme, `@theme inline` bridge). Every class used by the fragments (`bg-card`, `text-muted-foreground`, `bg-chart-1`, …) then works.
2. Fonts: load Inter Variable + IBM Plex Mono 400/500/600 (fontsource via jsdelivr, same links as the design surface's `styles/resources.json`) in `index.html`, then alias `--font-sans` / `--font-mono` to them.
3. Pages → one React component per route. The fragments are plain JSX-compatible HTML; classes survive verbatim. Swap `<a href>` for the router's `Link`.
4. Components → React components:
   - `bench-app-header` → `<BenchHeader/>` (nav items = the routes).
   - `bench-chart` → `<BenchChart mode="ceiling" data={...}/>` — the `DATA` constant becomes a prop fed by the backend; the ApexCharts option-building stays identical. Install `apexcharts` from npm (the component's pinned CDN script is design-surface-only; in React use the npm package). Colours must keep being read from CSS custom properties so both themes stay correct.
   - `bench-layer-ledger` → `<LayerLedger run={...}/>` — `LEDGER` becomes a prop; segment widths become inline styles (`style={{width: pct+"%"}}`), as the component already does post-insertion.
5. Pages in the design use the flat routes `/bench-layers` etc. In the React router, mount them under a `/bench` scope if you prefer (`/bench/layers`); just keep `NAV` in sync.

## Load-bearing decisions (do not "simplify" these away)

- **Chart hues are pinned per theme** (`chart-1` amber = SQLite, `chart-2` azure = Postgres, `chart-3` violet = baseline). shadcn's stock chart colours shift hue with the theme, which silently breaks series identity. The `tokens.css` comment says this; keep it true.
- **The layer ledger is a neutral sequential ramp** (`bg-foreground/15…85` in pipeline order), not a hue per layer — the backend hue stays the only identity colour on the card. The dominant layer gets a ring.
- **Middleware is expandable, per backend** (`<details>` panels): every middleware gets its own row, bar and ms, and the panel total reconciles **exactly** with the Middleware segment in the bar above. The same rule applies to real data: the backend must emit per-middleware timings that sum to the middleware layer total (see `harness/backend` — the beacon seam in `src/telemetry.rs`).
- **Form over decoration**: solid = p95, dashed = p99, one data scale per chart, budget reference lines at 500 ms / 1 s (the acceptance thresholds), y-caps so the budget lines stay legible.
- **Sample data is labelled as such** in every page footer — real runs replace it wholesale; don't let mock numbers migrate in unmarked.
- **Numbers must never contradict across views**: the same run's p95 appears on the overview, the ledger and the run history; they come from one store (the backend's run records), not three literals.

## What the backend feeds these views

- `/bench` ← per-run summaries: p50/p95/p99, error %, ceiling (highest passing VU level), req/s, scenario matrix.
- `/bench-layers` ← per-layer aggregates (from `Server-Timing`/JSONL traces) + statement fingerprints + N+1 detector output.
- `/bench-live` ← SSE stream from a running load test (per-second p95/p99 per backend, escalation state, machine meters).
- `/bench-runs` ← run history + delta vs previous run of the same scenario.

See `harness/backend/` for where each number is produced, and `planning/review_4/` + `docs/decisions/2026-08-08-*.md` for why the harness exists.
