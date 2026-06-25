/**
 * Per-app scope read from the server-rendered HTML shell. Closes
 * gap #71 — two umbral apps served from the same browser (e.g.
 * `127.0.0.1:8000` and `127.0.0.1:8001`) used to share localStorage
 * keys + the Dexie database, so app A would see app B's request
 * history / theme / settings.
 *
 * The Rust-side `PlaygroundPlugin::new(app_name)` injects the name
 * into the shell HTML two ways:
 *
 *  - `<meta name="umbral-playground-app" content="...">`
 *  - `window.__UMBRAL_PLAYGROUND_APP__ = "..."`
 *
 * We read the window global first (cheaper than DOM lookup) and
 * fall back to the meta tag. Either resolution lands in time —
 * the inline script that sets the global runs before this module
 * is imported, since the bundle is loaded with `type="module"`
 * after the head's inline scripts execute.
 *
 * If both shapes are missing — e.g. during a Vitest run where
 * there's no shell HTML — fall back to `"default"`. That matches
 * the Rust-side `Default::default()` warning fallback, so a unit
 * test exercising the same store doesn't trip a typeof guard.
 */
export function getAppScope(): string {
  if (typeof window !== "undefined") {
    const g = (window as unknown as { __UMBRAL_PLAYGROUND_APP__?: unknown })
      .__UMBRAL_PLAYGROUND_APP__;
    if (typeof g === "string" && g.length > 0) return g;
  }
  if (typeof document !== "undefined") {
    const meta = document.querySelector<HTMLMetaElement>(
      'meta[name="umbral-playground-app"]',
    );
    const content = meta?.content?.trim();
    if (content) return content;
  }
  return "default";
}

/** Prefix a storage key with the app scope so two apps don't share
 *  the same slot. Convention: `<app>:<key>`. */
export function scopedKey(key: string): string {
  return `${getAppScope()}:${key}`;
}
