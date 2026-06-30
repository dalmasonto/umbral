# Admin dashboard visual refresh — design

Date: 2026-06-30
Area: `plugins/umbral-admin`
Status: design, pending implementation plan

## Goal

Make the umbral admin look like a professional, modern SaaS dashboard — **not** Django admin. Replace the warm "brownish" neutral palette with a true neutral (pure white in light, near-black neutral in dark), standardize a readable card system, fix several dev-vs-production styling drift bugs, keep table pagination in its long (numbered) form, make the create/edit sheet responsive, and ensure the changelist refreshes after every successful create/save/save-and-continue.

This is a styling + small-bugfix pass. It does **not** restructure the dashboard layout, add chart types, or add dependencies. ApexCharts (all charts incl. sparklines), Lucide (icons), and Inter (type) remain the only providers per the project's standing rule.

## Root-cause theme: collapse the duplicated renderers

Three of the reported problems are the same class of bug — a renderer that exists in **two copies that fell out of sync**. The fix in each case is to collapse to one source, not to patch the broken copy (CLAUDE.md "fix the contract, not the symptom"):

1. **Tailwind config drift** — dev uses the Tailwind **CDN** with an inline config in `wrapper.html` that defines `fontSize`, `fontFamily`, `borderRadius`, and the full color palette. Production uses the compiled `css/tailwind.config.js`, which defines **only colors + spacing**. So in production `text-label-sm`, `font-label-sm`, the custom radii, and `secondary`/`tertiary` colors generate **nothing** and fall back to browser defaults. This is why sidebar plugin-group labels and table `<th>` headers render at ~16px ("huge") in production.
2. **Pagination drift** — the numbered/windowed footer lives in `_macros/data_table.html` (first page load). The HTMX `/rows` swap renders `rows_fragment.html`, which has a *compact* `page X / Y` footer. So the first load shows `1 … 4 5 6 … 50` and any sort/search/page swap degrades to the short form.
3. **Card drift** — every surface re-declares an ad-hoc `bg-surface-container border border-outline-variant rounded-xl px-md py-md` instead of one shared card recipe, so density/elevation/padding wander.

## Work items

### 1. Neutral palette (`wrapper.html` token blocks)

Rewrite the `:root` (light) and `.dark` token blocks to a **zero-chroma achromatic grey ramp** (`oklch(L 0 0)`), removing the warm hue (75–95) and chroma currently baked into every neutral.

- **Light:** `--background` pure white `oklch(1 0 0)`; surface-container ramp `1.0 → 0.985 → 0.97 → 0.95 → 0.93`; `--surface` white `oklch(1 0 0)`; `--on-surface oklch(0.20 0 0)`; `--on-surface-variant oklch(0.45 0 0)`; `--outline oklch(0.70 0 0)`; `--outline-variant oklch(0.90 0 0)` (hairlines); `--divider oklch(0 0 0 / 0.08)`, `--divider-soft oklch(0 0 0 / 0.05)`.
- **Dark (near-black neutral, not true black):** `--background oklch(0.16 0 0)`; surface ramp `0.14 → 0.18 → 0.21 → 0.25 → 0.29`; `--surface oklch(0.18 0 0)`; `--on-surface oklch(0.96 0 0)`; `--on-surface-variant oklch(0.70 0 0)`; `--outline oklch(0.55 0 0)`; `--outline-variant oklch(0.32 0 0)`; `--divider oklch(1 0 0 / 0.08)`.
- **Primary = monochrome.** Light: `--primary oklch(0.20 0 0)`, `--on-primary oklch(1 0 0)`, `--primary-container oklch(0.95 0 0)`. Dark: `--primary oklch(0.96 0 0)`, `--on-primary oklch(0.18 0 0)`, `--primary-container oklch(0.28 0 0)`. So primary buttons / active nav are ink-on-paper (light) / paper-on-ink (dark).
- **Status colors keep chroma.** `--error` stays red. Add `--success` (green) and `--warning` (amber) token pairs (+ `-container`/`on-` variants) for KPI deltas and badges that carry meaning. Wire these into the color map (work item 2) so `text-success`/`bg-warning-container` etc. exist.
- **`secondary` / `tertiary` neutralized** to grey (they currently render a stray blue/orange). Keep the token names so existing templates don't break; just point them at neutral values (or at the status tokens where a template genuinely meant "warning"/"info").
- `--surface-tint` neutralized; `{% if brand_color %}` override path preserved (a developer-supplied brand color still wins for `--primary`).

### 2. One Tailwind config (fixes huge sidebar labels + huge table headers)

Single source of truth for the theme tokens so dev (CDN) and prod (compiled) cannot drift:

- Extract the shared `theme.extend` object — `colors`, `spacing`, `borderRadius`, `fontFamily`, `fontSize` — into `plugins/umbral-admin/css/theme.cjs` (a CommonJS module exporting the object).
- `css/tailwind.config.js` becomes `theme: { extend: require('./theme.cjs') }`.
- Feed the **same** object into the CDN `<script id="tailwind-config">` in `wrapper.html`. Preferred: inject it at render time so there is literally one definition. If render-time injection is out of scope for this pass, the fallback is to generate the inline `<script>` body from `theme.cjs` at build time; a hand-maintained second copy is **not** acceptable (it reintroduces the drift). Implementation chooses the mechanism in the plan; the contract is "one definition, both consumers read it."
- Add the missing `success`/`warning` colors (from work item 1) to the color map.

Outcome: in production `text-label-sm` = 11px again → sidebar plugin-group labels and `<th>` headers render slender; custom radii and the full color set resolve.

### 3. Standard card system

One documented card recipe, applied everywhere instead of ad-hoc per-template classes.

- Define a `shadow-card` elevation token (light: `0 1px 2px rgba(0,0,0,.04), 0 1px 3px rgba(0,0,0,.06)`; dark: effectively none — dark cards separate via the lighter `surface-container` + hairline border). Add it to the Tailwind `boxShadow` extend in `theme.cjs`.
- Card recipe: `bg-surface` (light, → white) / `bg-surface-container` (dark) + `border border-outline-variant` + `rounded-xl` + `shadow-card` + consistent padding (`px-lg py-md`), with a standard header row (title left, optional action right) and a readable body type scale (`text-body-sm`/`text-body-md` for content, `text-label-sm` uppercase for eyebrows).
- Apply to: dashboard quick-stats strip, model cards, the 12-col widget cards (`_macros/widgets/*`), and the changelist/detail/form container panels. Replace repeated literal class strings with the shared recipe (a Jinja macro `card()` wrapper or a documented utility class — plan decides).
- Readability tuning: ensure card content has enough contrast on the new neutral surfaces (muted text uses `on-surface-variant`, not `outline`, for body copy that must be read).

### 4. Long-form pagination everywhere (fixes the short-form-after-swap bug)

- Extract the numbered/windowed footer (currently `_macros/data_table.html` ~lines 631–720: first / prev / `1 … window … last` / next / last + page-size select + range text) into a shared macro `_macros/pagination.html`, parameterized by `admin_base, table, search_val, filter_qs, sort_col, sort_order, pagination`.
- Use the shared macro in **both** `data_table.html` (initial load) and `rows_fragment.html` (HTMX `/rows` swap). Delete the compact footer from `rows_fragment.html`.
- Result: the long numbered form persists across every sort/search/page interaction. The `/rows` endpoint already preserves URL kwargs (see work item 6's listener), so context is retained.

### 5. Responsive create/edit sheet (fixes small-screen overflow)

`_macros/sheet.html` panel is `fixed top-4 right-4 bottom-4` with a hard `width: var(--sheet-width, 640px)` and no viewport clamp; the drag-resize JS also restores a saved px width from `localStorage` that can exceed a phone's width.

- Clamp width: `width: min(var(--sheet-width, 640px), 100vw - 2rem)` so it shrinks to fit when the viewport is narrower than the target.
- Below the `sm` breakpoint, make it a near-full-width drawer (`inset-x-2` or `left-2 right-2`), and disable the drag-resize handle (no horizontal room to resize on a phone).
- On load, clamp the `localStorage`-restored width to the current viewport before applying it (`Math.min(saved, window.innerWidth - 32)`), so a width saved on a desktop never overflows a phone.
- Audit the sheet body fields/grids for `min-w-0` so long values wrap instead of forcing horizontal scroll; verify the FK picker / multiselect popovers and the inline tables reflow.

### 6. Changelist refresh on create / save / save-and-continue

Contract: after a successful **create**, **save**, or **save-and-continue** from the sheet, the changelist table refreshes **in place** using the current URL kwargs (search / sort / page / filters). Save-and-continue keeps the sheet open **and** refreshes the underlying table.

Current state:
- The front-end `refreshTable` listener (`admin.js:1493`) already reads `window.location.search` → `GET {path}/rows{search}` into `#table-body`, falling back to a full reload when there's no table. **URL kwargs are already preserved.**
- `sheet::sheet_create` success → emits `closeSheet + refreshTable + showToast`. ✓
- `crud::update` plain Save → emits `closeSheet + refreshTable + showToast`. ✓
- `crud::update` **save-and-continue** (`crud.rs:694`, `if form.contains_key("_save_continue")`) → re-renders the sheet via `edit_sheet_handler`, emits **no `refreshTable`**. ✗ — the underlying list goes stale.

Fix:
- In the save-and-continue branch, attach an `HX-Trigger: {"refreshTable": {}}` header to the sheet re-render response (keep the sheet open; do **not** emit `closeSheet`). Optionally add a `showToast` "saved" confirmation.
- Verify end-to-end during implementation that create and plain-save genuinely refresh in the running app (the existing triggers should already work; confirm, don't assume).

### 7. General responsive + professional polish

- Verify the changelist toolbar, filter dialog, and dashboard grids reflow cleanly at `sm`/`md` (stack, don't overflow).
- Hairline dividers via the `divider` token; tuned hover/active states on the new neutral scale (subtle `surface-container-high` hover, not heavy fills).
- Density and rhythm: consistent control heights, consistent radii, generous-but-tight spacing — the difference between "professional dashboard" and "Django admin" is mostly consistent type scale, neutral palette, real elevation, and disciplined spacing, all of which the items above deliver.
- Apply the `frontend-design` skill's guidance during implementation for type rhythm, spacing, and elevation choices.

## Out of scope

- Dashboard layout / widget restructuring; new chart types.
- Rust render-path changes beyond (a) the save-and-continue `HX-Trigger` header and (b) injecting the shared theme object into `wrapper.html` if render-time injection is chosen for work item 2.
- New dependencies.

## Affected files (anticipated)

- `plugins/umbral-admin/templates/wrapper.html` — palette token blocks; CDN config source.
- `plugins/umbral-admin/css/theme.cjs` (new) — shared theme.extend.
- `plugins/umbral-admin/css/tailwind.config.js` — `require('./theme.cjs')`.
- `plugins/umbral-admin/css/input.css` — `shadow-card` / card utility if added at the CSS layer.
- `plugins/umbral-admin/templates/_macros/pagination.html` (new) — shared numbered footer.
- `plugins/umbral-admin/templates/_macros/data_table.html`, `rows_fragment.html` — use the shared footer.
- `plugins/umbral-admin/templates/_macros/sheet.html` — responsive panel + width clamp + drag JS guard.
- `plugins/umbral-admin/templates/dashboard.html` + `_macros/widgets/*` — card recipe.
- `plugins/umbral-admin/src/handlers/crud.rs` — save-and-continue `refreshTable` trigger.

## Verification

- Build the production CSS (`cd plugins/umbral-admin/css && npm install && npm run build`) and confirm `text-label-sm` resolves (sidebar labels + `<th>` render at the slender 11px, not 16px).
- Toggle light/dark: light background is pure white, dark is neutral near-black (no warm tint anywhere); primary button is black-on-white / white-on-black.
- Cards read consistently with one elevation/border/padding recipe across dashboard, list, detail, form.
- Paginate/sort/search a table with > 7 pages: the numbered `1 … window … 50` footer stays after every HTMX swap.
- Open create/edit sheet on a ~375px viewport: no horizontal overflow; drawer fits; fields wrap.
- Create a row, plain-save an edit, and save-and-continue an edit: in all three the changelist updates in place preserving the active search/sort/page/filters; save-and-continue leaves the sheet open.
- `cargo fmt && cargo clippy --all-targets && cargo build && cargo test` clean before commit.
