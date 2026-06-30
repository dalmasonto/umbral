---
name: admin-tailwind-theme-pipeline
description: Use when changing umbral-admin colors, fonts, radii, shadows, or any Tailwind token — or when a utility class works in dev but renders wrong (e.g. too-large) in production. Explains the single-source theme.json pipeline and the dev-CDN-vs-compiled-CSS split.
---

# umbral-admin Tailwind theme pipeline

## Context

The admin UI (`plugins/umbral-admin`) styles itself with Tailwind. There are **two** ways the CSS reaches the browser, and they must stay in sync or you get bugs that only appear in production:

- **Dev:** `templates/wrapper.html` loads the Tailwind **Play CDN** (`<script src="https://cdn.tailwindcss.com...">`) and configures it inline via `<script id="tailwind-config">`. The CDN JIT-compiles classes in the browser.
- **Prod:** `build.rs` runs `npx tailwindcss` against `css/tailwind.config.js` to produce the minified `src/assets/admin.css`, which is what ships when there's no Node at build time. `wrapper.html` gates CDN-vs-compiled on the `environment` global.

**The trap that bit us (fixed 2026-06-30):** these two configs were *separate copies* that drifted. The CDN inline config defined `fontSize`/`fontFamily`/`borderRadius`; the compiled `tailwind.config.js` defined only `colors`+`spacing`. So `text-label-sm` (a custom 11px size) generated **nothing** in production and fell back to the browser default 16px — sidebar plugin-group labels and table `<th>` headers rendered huge in prod but looked fine in dev.

## Approach

**There is now ONE theme definition: `plugins/umbral-admin/css/theme.json`.** It is the entire Tailwind `theme.extend` (colors, spacing, borderRadius, boxShadow, fontFamily, fontSize). Both consumers read it:

1. `css/tailwind.config.js` → `theme: { extend: require('./theme.json') }` (Node reads JSON natively).
2. `src/engine.rs` exposes it as a MiniJinja global: `env.add_global("admin_theme_json", minijinja::Value::from_safe_string(include_str!("../css/theme.json").to_string()))`. `wrapper.html`'s CDN config renders `theme: { extend: {{ admin_theme_json }} }`.

`from_safe_string` is essential — without it MiniJinja HTML-escapes the JSON's `"`/`{}` inside the `<script>` and the config is broken.

### To change any admin token (color, font size, radius, shadow):

1. Edit **`css/theme.json` only**. Never re-add tokens inline in `wrapper.html` or `tailwind.config.js` — that re-introduces the drift.
2. Color *values* (the actual oklch) live as CSS custom properties in `wrapper.html`'s `:root` (light) and `.dark` blocks; `theme.json`'s `colors` map just points at `var(--token)`. So a new *color* needs both: the `--token` value in `wrapper.html` AND the `name: "var(--token)"` entry in `theme.json`'s `colors`.
3. A new *fontSize/spacing/radius/shadow* needs only the `theme.json` entry.
4. Rebuild the production CSS so the change ships: `cd plugins/umbral-admin/css && npm install && npm run build` (writes `src/assets/admin.css`). **Commit the rebuilt `admin.css`** — it's tracked, not gitignored, and is what prod serves.

### Verifying a class actually compiles in prod

After `npm run build`, grep the minified output for the utility:
`grep -o '\.text-label-sm{[^}]*}' plugins/umbral-admin/src/assets/admin.css` → should print `.text-label-sm{font-size:11px;...}`. If a class you use renders wrong only in prod, this is the first check — a missing rule means the token isn't in `theme.json` (or the CSS wasn't rebuilt).

## Why

JSON is the single source because both a Node build step and a Rust render path can read it natively (Rust can't `require()` a `.cjs`/`.js` module; a hand-maintained second copy drifts). Tailwind's `theme.extend` is fully JSON-serializable — `fontSize` entries are `["11px", { "lineHeight": "14px", ... }]` arrays, all valid JSON.

## Pitfalls

- **Editing the wrong file.** If you add a token to `wrapper.html`'s inline config or to `tailwind.config.js` directly, dev may look right while prod (or the other path) silently lacks it. Always edit `theme.json`.
- **Forgetting to rebuild `admin.css`.** Template/theme edits don't change the committed compiled CSS until you re-run the Tailwind build. Prod ships the stale CSS otherwise.
- **`cargo fmt` churn.** `cargo fmt -p umbral-admin` reformats the WHOLE crate (the committed code isn't rustfmt-canonical), touching ~20 unrelated files. Always `git restore` the files your task didn't intend to change before committing. Never run bare `cargo fmt` (workspace-wide).
- **HTML-escaping.** The `admin_theme_json` global must use `from_safe_string`; `{{ admin_theme_json }}` (not `| escape`) in the template.
- **Dead tokens.** A token in `theme.json` that no template references is purged by Tailwind and never reaches `admin.css` (harmless, but tests asserting it "exists in theme.json" don't prove it ships).

## See also
- Design spec: `docs/superpowers/specs/2026-06-30-admin-visual-refresh-design.md`
- Plan: `docs/superpowers/plans/2026-06-30-admin-visual-refresh.md`
- `plugins/umbral-admin/build.rs` (the prod build invocation + CDN fallback)
- `CLAUDE.md` → "Fix, don't patch" (the drift was fixed at the contract, not patched per-label)
