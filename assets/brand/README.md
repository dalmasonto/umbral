# umbral brand assets

The mark is an **umbra crescent** — the dark shadow body of a total eclipse with a glowing corona crescent and a "diamond-ring" highlight. It's the visual of the name: *umbral*, "of the shadow" (Latin *umbra*, the darkest part of a shadow).

## Files

| File | Use |
|---|---|
| `umbral-mark.svg` | Logomark only (square). App icons, avatars, spot use. Works on dark and light. |
| `umbral-logo.svg` | Horizontal lockup (mark + wordmark) for **dark** backgrounds. |
| `umbral-logo-light.svg` | Horizontal lockup for **light** backgrounds. |
| `umbral-favicon.svg` | Favicon — mark on a rounded dark tile, simplified (no blur) to stay crisp at 16px. |
| `preview.html` | Renders every asset on dark + light for review. |

All are SVG (scalable, crisp at any size). The wordmark is set in **Inter** via `<text>`; in a context that doesn't load Inter it falls back to `system-ui`. For fixed, font-independent rendering (print, third-party sites), outline the text.

## Palette

| Token | Hex | Role |
|---|---|---|
| Ground | `#07070f` | deepest background |
| Umbra | `#0c0d1c` → `#1b1840` | the shadow body (radial) |
| Corona warm | `#ffe6a6` | diamond-ring / sun edge |
| Corona violet | `#b79cff` | mid crescent |
| Corona indigo | `#6d5cf0` | crescent base |
| Primary violet | `#8b7bf5` / `#a78bfa` | glow, wordmark accent |
| Ink (dark bg) | `#ecebf5` | wordmark on dark |
| Ink (light bg) | `#141126` | wordmark on light |

Type: **Inter** (weights 500–800). Same face as the docs site and the architecture diagram.

## Do / don't

- **Do** keep clear space around the mark equal to the crescent's width.
- **Do** use the light lockup on light backgrounds (the umbra body still reads).
- **Don't** recolor the corona gradient, stretch the mark, or add a second effect — the glow is the one accent.
- **Don't** hand-redraw the eclipse; scale the SVG.

## Wiring it in (not done automatically)

The site/docs still ship the Specra placeholder favicon. To adopt this mark, replace:
- `documentation/static/favicon.svg`
- `umbral_website/` favicon and header logo
- `plugins/umbral-playground/frontend/public/favicon.svg`

with `umbral-favicon.svg` / `umbral-mark.svg`. Left as a deliberate follow-up so it doesn't silently change the live sites.
