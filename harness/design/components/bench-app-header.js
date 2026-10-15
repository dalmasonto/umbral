// bench-app-header — the shared top bar for every bench page.
// Port note: maps 1:1 to a React <BenchHeader/> (nav items = route links).
const NAV = [
  { href: "/bench", label: "Overview" },
  { href: "/bench-layers", label: "Layers" },
  { href: "/bench-live", label: "Live run" },
  { href: "/bench-runs", label: "Runs" },
];

class BenchAppHeader extends HTMLElement {
  connectedCallback() {
    if (this.dataset.ready) return;
    this.dataset.ready = "1";

    // The sandbox may rewrite routes into its own URL space, so match by the
    // route's tail (exact segment), not strict equality.
    const here = (location.pathname + location.hash).replace(/\/+$/, "");
    const active = (href) => here.endsWith(href) && !(href === "/bench" && here.endsWith("/bench-layers"));

    this.className = "block sticky top-0 z-40";
    this.innerHTML = `
      <div class="w-full border-b border-border bg-background/90 backdrop-blur">
        <div class="mx-auto flex h-14 max-w-6xl items-center gap-6 px-4">
          <a href="/bench" class="flex items-center gap-2.5">
            <span class="grid h-7 w-7 place-items-center rounded-md bg-primary text-primary-foreground">
              <svg viewBox="0 0 16 16" class="h-3.5 w-3.5" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                <path d="M2 13h12"/><path d="M4 13V8"/><path d="M8 13V3"/><path d="M12 13v-3"/>
              </svg>
            </span>
            <span class="text-sm font-semibold tracking-tight">umbral <span class="text-muted-foreground font-normal">bench</span></span>
          </a>
          <nav class="flex items-center gap-1 text-sm" aria-label="Bench sections">
            ${NAV.map(
              (n) => `<a href="${n.href}" aria-current="${active(n.href) ? "page" : "false"}"
                class="rounded-md border px-2.5 py-1.5 ${active(n.href) ? "border-border bg-muted font-medium text-foreground" : "border-transparent text-muted-foreground hover:bg-accent hover:text-accent-foreground"}">${n.label}</a>`
            ).join("")}
          </nav>
          <div class="ml-auto hidden items-center gap-3 md:flex">
            <span class="inline-flex items-center gap-3 text-xs text-muted-foreground font-mono">
              <span class="inline-flex items-center gap-1.5">
                <span class="h-2 w-2 rounded-full bg-chart-1"></span>SQLite
              </span>
              <span class="inline-flex items-center gap-1.5">
                <span class="h-2 w-2 rounded-full bg-chart-2"></span>Postgres
              </span>
            </span>
            <span class="rounded-md border border-border bg-muted px-2 py-1 text-xs font-mono text-muted-foreground">r-248 · sample</span>
            <span class="rounded-md border border-border px-2 py-1 text-xs font-mono">v0.0.13+e0d5e18</span>
          </div>
        </div>
      </div>`;
  }
}
customElements.define("bench-app-header", BenchAppHeader);
