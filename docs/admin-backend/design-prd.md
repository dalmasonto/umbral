# Umbra Admin — Design PRD (UI/UX)

| | |
|---|---|
| **Scope** | Visual & interaction design for the `umbra-admin` interface |
| **Audience** | Design (Google Stitch), front-end |
| **Status** | Draft v0.2 · May 30, 2026 |
| **Companion** | `umbra-admin-backend-prd.md` (how it's powered) · `arch.md` (architecture) |

---

## 1. Vision — and why a dev picks this over raw Axum/Actix

Spinning up a backend on bare Axum/Actix means *you* build the admin: a table, forms, auth
screens, file handling, all of it. Most teams either skip it or ship something ugly. **Umbra's
admin is the payoff** — register a model and get a fast, modern, themeable back-office for free.
The bar is not "Django admin in Rust"; it's "the admin a team would otherwise spend a month
building, generated from their models."

What makes it a *reason to choose the framework*:

- **Zero-config, great-looking CRUD** from your models — light/dark + a one-line custom theme.
- **One world-class DataTable** (§7) reused everywhere: sort, search, faceted filters, column
  control, density, selection with a **floating bulk-action toolbar**, an **extensible icon
  action column**, and **full pagination** (page-size, prev/next, page list).
- **Right-side sheets** (§8) for preview/edit — fast, keeps list context, no page reloads.
- **Real field editors** (§9): async searchable relation pickers that don't load the whole table,
  and **inbuilt file/media previews** (§10) for images, PDFs, video/audio, CSV, and code/text.
- **Backend-defined extensible actions** (§11) — row and bulk — written in Rust, rendered with a
  consistent icon set.
- **Power-user speed**: command palette, keyboard nav, audit history.
- **Permissions-aware** everywhere; **responsive**; **accessible**.

### Design principles
1. **Data-first, chrome-second** — content gets the pixels; nav/toolbars stay quiet.
2. **Dense but breathable** — high density, generous line-height, restrained borders.
3. **Customizable, not chaotic** — curated catalogs and good defaults; the empty state already looks good.
4. **One consistent shell** — sidebar + topbar + content never move; overlays (sheets, dialogs) handle the rest.
5. **Theme-native** — every color/radius is a token with light & dark values; nothing hard-coded.
6. **Reuse one component** — the DataTable, the Sheet, the field editors, and the dialogs are shared primitives, not per-screen rebuilds.

---

## 2. Icon library

Standardize on **Lucide** (MIT, ~1,400 icons, consistent 24px grid, tree-shakeable, pairs
cleanly with Tailwind). All actions, nav, file-type glyphs, and controls reference Lucide icons
by name (e.g. `pencil`, `trash-2`, `eye`, `download`, `more-horizontal`). Backend-defined actions
specify a Lucide icon name (§11). Alternatives if needed: Tabler Icons or Heroicons — pick one
and use it everywhere; never mix sets.

---

## 3. Global layout (the shell)

```
┌───────────────────────────────────────────────────────────────┐
│ TOPBAR: logo · breadcrumbs ······ ⌘K search · theme · user     │
├───────────┬───────────────────────────────────────────────────┤
│  SIDEBAR  │                 CONTENT REGION                      │
│  (scoped, │   (dashboard, or a DataTable for a model)           │
│  search)  │                                  ┌──────────────┐   │
│           │                                  │ right SHEET  │   │
│           │                                  │ (preview/    │   │
│           │                                  │  edit)       │   │
└───────────┴──────────────────────────────────┴──────────────┘──┘
```

- **Sidebar:** ~260px, collapsible to a ~64px icon rail; scrolls independently.
- **Topbar:** ~56px, sticky. Breadcrumbs (left), command-palette search trigger (center/right),
  theme toggle + user menu (right).
- **Content:** the only region that swaps. Overlays (right sheet, dialogs) float above it.

---

## 4. Theming — light & dark + custom themes

Everything is a **semantic token** exposed as a CSS custom property, defined under `:root`
(light) and `.dark` (dark). Components never hard-code color/radius.

| Token role | Light | Dark |
|---|---|---|
| `--umbra-bg-canvas` | `#F7F8FA` | `#0E1014` |
| `--umbra-bg-surface` (cards, sheet) | `#FFFFFF` | `#171A21` |
| `--umbra-bg-surface-2` (insets, hover) | `#F0F2F5` | `#1E222B` |
| `--umbra-border-subtle` | `#E5E8EC` | `#262B35` |
| `--umbra-text-primary` | `#10131A` | `#ECEFF4` |
| `--umbra-text-secondary` | `#5B6472` | `#9AA4B2` |
| `--umbra-accent` | `#5B5BD6` | `#7C7CF0` |
| `--umbra-accent-quiet` (tints, selected) | `#EEEEFF` | `#22243A` |
| `--umbra-danger` / `success` / `warning` | semantic | semantic |
| chart series 1–6 | a categorical ramp legible on both canvases | |

- **Type:** humanist sans (Inter). Sizes ~12/13/14(body)/16/20/24/30; tabular numerals in tables/KPIs.
- **Radius:** 6px controls, 8px cards, 10px chips, **14px sheet & dialogs**.
- **Elevation:** shadows in light; borders + subtle surface lift in dark.
- **Theme toggle:** light / dark / system, persisted per user; no flash on load.

### 4.1 Custom themes (`admin.css`)
The admin loads a developer-supplied **`admin.css`** *last*; it redefines any subset of the
variables to rebrand without forking:

```css
:root { --umbra-accent: #0F766E; --umbra-radius-card: 12px; }
.dark { --umbra-accent: #2DD4BF; --umbra-bg-canvas: #08100E; }
```

Tailwind's theme is backed by these variables (`colors.accent = "var(--umbra-accent)"`), so
utilities and components pick up overrides automatically; the Tailwind-config-extension path
works too. Overriding only `--umbra-accent` is the common one-line rebrand.

---

## 5. Dashboard (the landing screen)

Customizable 12-column widget grid; widgets are backend-registered, users compose per-user
layouts. KPI cards, line/bar/donut charts, **DataTable widgets** (§7), activity feed, quick
actions, custom. Full spec unchanged from v0.1 — see backend PRD §4 for the widget system.
(The recent-records and top-N widgets reuse the DataTable component in a compact mode.)

---

## 6. Left sidebar (the spine)

- **Search box** at top live-filters the model tree; matches highlight, non-matching groups collapse.
- **Groups, one per app/plugin** (Blog, Auth, Newsletter), collapsible, each listing its models with a count badge.
- **Selected item**: `accent-quiet` background + `accent` text. Pinned **Dashboard** entry on top.
- **Collapsed rail**: icons only; hover flyout lists the group's models.
- Permission-filtered (only what the user may see). Remembers expand/collapse state.

---

## 7. DataTable — the reusable workhorse

**One component**, used by the model changelist, dashboard table widgets, CSV preview, and inline
relation lists. Everything below is configurable per use; sensible defaults come from model
metadata.

### 7.1 Anatomy (top → bottom)
1. **Header bar:** title + live record count · primary **"Add <Model>"** (Lucide `plus`).
2. **Toolbar:** search input (Lucide `search`) · **filter** button → faceted filter panel
   (by field, FK, date range, choices) shown as removable chips · **column** menu (show/hide,
   reorder) · **density** toggle (comfortable/compact) · **export** (Lucide `download`, CSV) ·
   optional **saved views** (v2).
3. **Table body:** see §7.2.
4. **Pagination footer:** see §7.4.
5. **Floating bulk-action toolbar:** see §7.3 (appears on selection).

### 7.2 Columns, rows, selection
- **Selection column** (left): header checkbox = select-all-on-page + a "select all N matching"
  affordance; per-row checkboxes.
- **Data columns:** sortable (click header → asc/desc/none, multi-sort with shift), resizable,
  reorderable; types render appropriately — text (truncate + tooltip), numbers (right-aligned,
  tabular), booleans/choices (status **pills**), dates (relative + absolute tooltip), FK (link
  chip → opens that record's sheet), **file/image (thumbnail or type glyph, §10)**.
- **Row interactions:** click anywhere (outside controls) → **opens preview sheet (§8)**; hover
  reveals the action column.
- **Action column** (right, sticky): **fully extensible**, icon-based (Lucide). Shows up to ~3
  inline quick actions (e.g. `eye` preview, `pencil` edit, `trash-2` delete) + a `more-horizontal`
  overflow menu for the rest. **Actions are backend-defined** (§11); each renders its icon,
  tooltip/label, and danger styling, and is permission-filtered per row.
- **Inline edit** (optional per column): double-click a cell → inline editor (text/number/
  select/toggle) with save on blur/Enter, escape to cancel; for quick edits without opening the sheet.
- **States:** skeleton rows (loading), distinct **empty** / **no-results (filtered)** / **error
  (retry)** states.

### 7.3 Floating bulk-action toolbar
On any selection, a **floating toolbar** rises from the bottom-center (pill-shaped, elevated,
14px radius, theme surface):
- Left: selection count + "Clear".
- Center/right: **bulk actions as icon buttons** (Lucide) — Delete (`trash-2`, danger) plus any
  backend-defined bulk actions; overflow into a menu past ~4.
- Destructive bulk actions route through the **confirm dialog (§12)**; long-running ones show
  progress and a result toast.
- Dismisses when selection clears; never covers the pagination permanently (sits above it).

### 7.4 Pagination footer (full)
A complete pager, always at the bottom:
- **Range text:** "1–25 of 1,204".
- **Page-size select:** 10 / 25 / 50 / 100 (configurable default), persisted per user.
- **Prev / Next** buttons (Lucide `chevron-left` / `chevron-right`), disabled at bounds.
- **Page-number list:** first … current-window … last (e.g. `1 … 4 5 [6] 7 8 … 41`), with
  jump-to-first/last; a "Go to page" input for large sets.
- Server-driven (offset/limit or cursor under the hood — see backend PRD); reflects active
  search/filters in the count.

### 7.5 Responsive
≥1024px: full table. 768–1023px: hide low-priority columns (column menu still exposes them),
keep selection + actions. <768px: table becomes a **stacked card list** (key fields + action
overflow); bulk toolbar stays; pagination simplifies to range + prev/next + size.

---

## 8. Right-side Sheet (preview & edit)

Preview and edit happen in a **floating right-side sheet**, not a centered dialog and not a page
nav — so the list stays visible and the flow feels instant.

### 8.1 Form & behavior
- **Position:** anchored to the right, **inset/offset** from the viewport edges (margin top/right/
  bottom ~16–24px) so it reads as a floating panel with **14px radius** and a soft shadow; the
  underlying list dims (scrim) but stays visible to the left.
- **Size:** "good sized" default ~640px wide (≈ min(720px, 90vw)); **user-resizable** via a left
  drag handle; remembers width per user. Full height of the inset.
- **Structure:** sticky **header** (title + record identity, mode toggle, close ✕) · scrollable
  **body** · sticky **footer** action bar.
- **Modes:** **Preview** (read-only: fields as labelled values, related objects as links/lists,
  file/media previews inline (§10), created/updated + audit/history link) and **Edit** (the form).
  A primary **Edit** button in preview swaps to edit **in place**; Cancel returns to preview.
- **Edit form:** sectioned field groups; all field editors from §9; inline related records.
  Inline validation + a summary banner on submit failure.
- **Footer:** Save · Save and add another · Save and continue · Delete (→ confirm dialog §12).
  Success → toast, sheet stays (continue) or closes, and the **DataTable row updates in place**.
- **Add new:** same sheet in create mode (opened from the table's Add button).
- **Stacking:** opening a related record (FK "＋ Add new", or clicking a related link) **stacks a
  second sheet** offset further left with a back affordance; closing returns to the parent.
- **Guards:** unsaved-changes confirm on close/Esc/scrim; focus trap; focus returns to the source
  row. `Esc` closes top sheet only.

---

## 9. Field editors

Rendered from model metadata; all Tailwind components, theme-native:
- **Text / textarea / rich-text**, **number**, **slug** (auto from a source field), **choice**
  (segmented or select), **boolean** (toggle), **date / datetime / time** (picker), **JSON**
  (collapsible editor), **color**, **duration**.
- **Foreign key (single)** & **Many-to-many (multiple)** — async searchable comboboxes that
  **never preload the whole table** (§9.1).
- **File / image / media** — upload + **inline preview** (§10), drag-drop, progress, replace/remove.

### 9.1 Async relation pickers (FK & M2M)
- **Search-on by default**; ~250ms debounce queries the backend options endpoint; matching runs
  in SQL (`search_fields`), not the browser.
- **Lazy + paginated**: first ~20 on open, infinite-load on scroll; the DOM never holds thousands.
- **FK:** single combobox, clearable, "＋ Add new" opens a nested create **sheet** (§8) and
  selects the result.
- **M2M:** removable **chips**, selected items excluded from later results, "+N more" overflow.
- **Preselected values** resolve labels via a single by-id lookup, not a full fetch.

---

## 10. File & media previews

A key differentiator: clicking a file field (in a row, the preview sheet, or a dedicated file
model) shows an **inbuilt preview** keyed off a backend-resolved `preview_kind` (backend PRD §6),
so the front-end just switches on a kind. Thumbnails appear in DataTable cells; full previews
open in the sheet (or a lightbox for images).

| `preview_kind` | Renders as |
|---|---|
| `image` (png/jpg/webp/gif/svg) | thumbnail in table; full image in a **lightbox** with zoom/fit; EXIF/dimensions meta |
| `pdf` | embedded paginated **PDF viewer** in the sheet + page nav + download |
| `video` (mp4/webm/mov) | HTML5 **player** with controls, poster thumbnail |
| `audio` (mp3/wav/ogg) | compact **audio player** + waveform/duration |
| `csv` / `tsv` | rendered with the **DataTable** (first N rows, headers, horizontal scroll) |
| `spreadsheet` (xlsx) | first sheet as a table where server can convert; else download card |
| `text` (.txt, .md, .log) | text viewer; **markdown** renders formatted with a "view raw" toggle |
| `code` (.py, .rs, .js, .json, .toml, .yaml, …) | **syntax-highlighted** viewer, line numbers, language badge, copy |
| `document` (.doc/.docx) | server-converted HTML/PDF preview where available; else download card |
| `download` (.zip, .tar, binaries, unknown) | **download-only card**: type glyph, filename, size, Download button — no inline render |

- **Common chrome:** every preview shows filename, size, mime, and a **Download** button; large
  files lazy-load and respect range requests (media seeking).
- **Security:** user-uploaded files are served with `Content-Disposition`/sandboxed and never
  executed inline as HTML (backend PRD §6, §9); previews fail safe to the download card.
- **Table cells:** images → thumbnail; others → a Lucide file-type glyph + filename; click → preview.

---

## 11. Extensible actions (backend-defined)

Row and bulk actions are **declared in the admin plugin (Rust)** and rendered consistently here.
The UI never hard-codes the action list beyond the built-ins (preview/edit/delete).

Each action surfaces (per backend PRD §5.3): **Lucide icon**, label/tooltip, **variant**
(default / danger), **scope** (row / bulk / both), optional **confirm** text, and a
**permission** (auto-hidden if unmet). After running, an action can: toast, refresh the table,
open a sheet, trigger a **download**, or redirect — per its backend result.

- **Row actions** populate the table action column (inline + overflow, §7.2).
- **Bulk actions** populate the floating toolbar (§7.3), operating on the selection (incl.
  "all N matching").
- **Built-ins** (preview `eye`, edit `pencil`, delete `trash-2`) are themselves expressed in this
  system, so a developer's custom actions sit beside them seamlessly.

---

## 12. Confirm / alert dialog (destructive actions)

A centered **alert dialog** (14px radius, **danger** primary) for single delete, bulk delete, and
any destructive action: title ("Delete Post?"), plain-language body naming object(s) + **count**
and any cascade warning, **Cancel** (ghost, default-focused) + **Delete** (danger, never
auto-focused), busy state, success toast. `Esc`/scrim = cancel.

---

## 13. Command palette & global search

`⌘K` / `Ctrl-K` opens a command palette: jump to any model, run a global record search
(backend-scoped, permission-filtered), trigger top-level actions, toggle theme. This is the
power-user speed that makes the admin feel professional rather than generated.

---

## 14. Component inventory (for Stitch)

App shell · sidebar (+ icon rail, model search) · topbar · command palette · breadcrumb · user
menu · theme toggle · **DataTable** (sortable/resizable/reorderable columns, selection, inline
edit, density, skeleton/empty/error) · **filter panel + chips** · **column menu** · **floating
bulk-action toolbar** · **full pagination footer (size select, prev/next, page list, jump)** ·
**extensible icon action column + overflow menu** · **right-side floating sheet (preview + edit,
resizable, stackable)** · field editors (§9) · **async FK combobox** · **async M2M multi-select
chips** · **file/media previewers** (image lightbox, PDF viewer, video/audio player, CSV table,
code/text viewer, download card) · KPI/chart cards · activity feed · **confirm/alert dialog
(danger)** · buttons (primary/secondary/ghost/danger) · pills/badges · toasts · Lucide icon set.

---

## 15. Responsive & accessibility

- **Breakpoints:** ≥1280px full; 768–1279px collapsed rail + reduced columns + sheet narrows;
  <768px sidebar drawer, table → stacked cards, sheet → near-full-width.
- **A11y:** WCAG AA contrast in *both* themes; full keyboard nav (sidebar, table rows/cells,
  sheet, palette, pagination, bulk toolbar); visible focus rings; ARIA roles for table/dialog/
  menu; charts and file previews have accessible summaries/alternatives; focus trapping in sheet/
  dialog; respect `prefers-reduced-motion` and `prefers-color-scheme`.

---

## 16. Stitch prompt seeds

Keep the §4 token language consistent across prompts; request light **and** dark each time.

- **Shell + dashboard:** "Modern admin dashboard: fixed 260px left sidebar grouped by app with a
  model search box, 56px top bar with breadcrumbs and a ⌘K search and theme toggle, main area a
  12-column grid of widget cards (KPI cards with sparklines, a line chart, a donut, a recent-
  records table). Inter font, 8px card radius, subtle borders. Light and dark."
- **DataTable:** "An admin data table: header with record count and an Add button; toolbar with
  search, a Filter button with removable filter chips, a column-visibility menu, and a density
  toggle; a sortable, selectable table with a left checkbox column, status pills, a thumbnail
  column, and a sticky right action column of icon buttons (eye, pencil, trash) plus an overflow
  menu; a full pagination footer with a page-size select, prev/next, and a numbered page list.
  When rows are selected, a floating pill toolbar appears bottom-center with a selection count and
  bulk action icons. Lucide icons. Light and dark."
- **Right sheet (preview/edit):** "A floating right-side panel inset from the viewport edges with
  14px radius and a soft shadow, the list dimmed behind it; sticky header with title and a
  Preview/Edit toggle and close; scrollable body showing a record as a form with sectioned
  groups, an async searchable foreign-key select, a many-to-many chip multi-select, a date
  picker, toggles, and an inline file preview; sticky footer with Save / Save and continue /
  Delete; a left-edge drag handle to resize. Light and dark."
- **File previews:** "A file preview panel that can show: an image in a zoomable lightbox, an
  embedded PDF viewer, a video player, a CSV rendered as a table, and a syntax-highlighted code
  viewer with line numbers; plus a download-only card for zip files showing a file-type icon,
  name, size, and a Download button. Light and dark."
- **Confirm dialog:** "A small centered alert dialog (14px radius) to confirm deleting records:
  title, a message naming the items and count, a ghost Cancel and a red Delete button. Light and dark."

---

*Backend contracts for the DataTable, relation options, extensible actions, and file previews
live in `umbra-admin-backend-prd.md`.*
