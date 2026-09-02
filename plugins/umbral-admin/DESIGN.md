# Admin design direction (gaps4 #61 — in progress)

Goal: an admin a team ships to internal users and never wants to replace — dense-yet-calm, on the standards we already picked (**Inter** type, **Lucide** icons, **ApexCharts** charts; never hand-roll SVG/icons). Reference feel: Supabase/Linear "considered dark," but our own palette, not a copy.

## Token system (single source of truth)
- `css/theme.json` maps token NAMES → CSS vars (Tailwind build + dev CDN both read it — keep edits here so they can't drift).
- The actual color VALUES are OKLCH CSS custom properties in `templates/wrapper.html` (`:root` = light, `.dark` = dark). Because they're runtime custom properties, palette changes are live on next render — no rebuild.

## Direction: "umbra dusk" slate
The framework is *umbral* — "of the shadow." The palette leans into that: neutrals carry a whisper of cool chroma (`oklch(L 0.008 264)`, a dusk blue-violet undertone) so surfaces read as a **considered cool slate** in both themes rather than flat achromatic gray — the premium-dark-admin look — while every L value (contrast/AA) is unchanged and `--primary` stays the developer's brand color. Semantic colors (success/warning/error) keep their own hues.

## Increments (each independently reviewable, reviewed live in dev)
1. **[shipped]** Design tokens — cool-slate neutral refinement across the whole palette.
2. Shell — sidebar nav + slim topbar + roomy content canvas; consistent spacing scale; ⌘K palette polish (`templates/palette.html`).
3. List / "table editor" page (`changelist.html`, `_macros/data_table.html`) — sticky header, typed/resizable columns, inline row actions, filter/sort chips, clean pagination, first-class empty states.
4. Detail / form pages — clear sections, labels, help-text, inline validation.
5. Dashboard + custom views.

Sequence: tokens first (done), then per-page, so each lands independently and you steer it live.
