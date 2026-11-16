---
name: taskflow-design-surface
description: Use when designing UI in the TaskFlow design surface (the design_* MCP tools) for this project — pages, components, tokens, screenshots, or when a design_write_page/design_write_component call is rejected. Triggers on "design a screen", design-room work, "design_compare", "design_screenshot".
---

# TaskFlow design surface

## Context
The TaskFlow MCP server ships a design subsystem for this project: **pages** (body fragments styled with shadcn classes over project tokens), **components** (custom elements carrying JS), a **token system** whose served `tokens.css` IS a shadcn `globals.css`, and a **headless-Chromium screenshot** tool so an agent can look at what it built. Used for the bench-UI design (project 8, `Bench` group, 2026-10-10) and available for any future UI work.

## Approach — the rules that actually bite

1. **Routes are FLAT single segments.** `/bench-layers` is accepted; `/bench/layers` returns a bare `400: Bad Request`. Keep every route to one segment.
2. **Pages** are fragments: no `<!doctype>`/`<html>`/`<head>`/`<body>`, no `<style>` blocks, no raw `<header>/<nav>/<footer>/<aside>` (register a component instead), no `<script src>` (the server refuses the markup). Cross-page links are plain `<a href="/route">` — the composer rewrites them into sandbox URLs. Back controls are `<button onclick="history.back()">`.
3. **Components** are where JS lives: light DOM (no `attachShadow`), exactly one `customElements.define`, and **CDN scripts ARE allowed in components** (that is the supported way to load a chart library — ApexCharts from cdnjs works; guard the load behind a `window.__yourFlag` promise so N instances share one fetch). Banned inside components: `fetch`/XHR, `eval`/`new Function`/`import()`, `localStorage`/cookies. The 6 KB size warning is soft.
4. **Classes** must be shadcn-scale: `bg-card`, `text-muted-foreground`, `bg-primary/10`, `rounded-lg`. Arbitrary values are rejected at write time with a `422` naming the exact rule (`rounded-[2px]`, `bg-blue-500`, `p-[13px]`). Inline `style="width:62%"` is accepted.
5. **Tokens**: only shadcn names, but project-only tokens (`typography.font-mono`, `colors.brand`) become vars/classes automatically. **Set `chart-1..5` deliberately**: the shadcn defaults change hue between light and dark, which silently breaks series identity across themes (a data-viz bug, not a taste question). Patch with `design_write_tokens` `{"colors":{"chart-1":{"light":"oklch(...)","dark":"oklch(...)"}}}`.
6. **Fonts** go in two global writes: the `typography.font-sans` token + `styles/resources.json` (fontsource css links via jsdelivr); never a `<link>` in a page.
7. **Look at your work**: `design_screenshot` (route, `theme:"both"`, `full_page:true`) renders light+dark in one image. **Disable chart animations** (`animations:{enabled:false}`) or the static capture shows empty series.
8. **Structure once**: pages that share chrome should share a component; the header/page/nav pattern is `bench-app-header` in project 8's Bench design as the reference implementation.

## Why
The surface is a shadcn-native design system, so anything drawn here ports to a real app in two writes: paste `tokens_css` (it is a `globals.css`) into the Vite/React app, and translate fragments into JSX (classes survive verbatim; each component maps to one React component; embedded data consts become props). Using it also gives the agent eyes — screenshots — which is the difference between designing and guessing.

## Pitfalls
- **The sandbox rewrites route links**, so in-component active-nav detection must compare route *tails* (`location.pathname.endsWith("/bench-layers")`), not strict equality with the manifest route.
- **Nested routes** (above) — the single most likely 400s you will chase.
- `422` rejections carry `message`/`rule`/`suggest`; read them and fix the class — do not retry verbatim.
- Pages can only contain one `<h1>` worth of hierarchy; the shell composes the rest.
- A page written without `base_version` overwrites silently; that is usually what you want while solo-designing, but pass it when collaborating on the same page.

## See also
- `planning/review_4/` — the bench-UI design came out of review_4's benchmark epic (task #674).
- `docs/decisions/2026-08-08-incident-readiness-profiling-benchmarks-health.md` — the profiling/metrics decisions the UI visualizes.
