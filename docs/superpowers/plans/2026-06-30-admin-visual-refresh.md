# Admin Visual Refresh Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the umbral admin a professional, neutral (pure-white / near-black) look with a consistent card system, fix the dev-vs-prod styling drift that makes sidebar labels and table headers render huge, keep table pagination in long numbered form after HTMX swaps, make the create/edit sheet responsive, and refresh the changelist after save-and-continue.

**Architecture:** The umbral admin is a server-rendered MiniJinja + HTMX + Tailwind UI in `plugins/umbral-admin`. Theme tokens are CSS custom properties in `wrapper.html`; Tailwind maps utility classes to those vars. Today there are **two** Tailwind theme definitions (a CDN inline config used in dev, a compiled `tailwind.config.js` used in prod) that have drifted — the root cause of the "huge text" bugs. This plan collapses the theme definition to one JSON source read by both, neutralizes the palette, extracts a shared pagination macro, and makes targeted template/JS/Rust fixes.

**Tech Stack:** Rust (axum, minijinja), HTMX, Tailwind CSS v3 (CDN in dev, compiled via `build.rs` + npm in prod), Lucide icons, Inter font, ApexCharts. oklch color space.

## Global Constraints

- No new dependencies. Charts → ApexCharts (incl. sparklines); icons → Lucide; type → Inter. Never hand-roll SVG/icons.
- Plugins use the ORM, never raw `sqlx::query` (not relevant here — no new DB access).
- Dark mode is **near-black neutral** (`oklch(0.16 0 0)`), NOT true black. Light mode background is **pure white** (`oklch(1 0 0)`).
- Primary accent is **monochrome**: near-black on white (light) / near-white on near-black (dark). Status colors (`error`/`success`/`warning`) keep chroma.
- All neutral tokens are **zero-chroma** (`oklch(L 0 0)`) — no warm hue.
- One theme definition only. The CDN inline config and the compiled `tailwind.config.js` MUST both read `css/theme.json`. A hand-maintained second copy is a plan failure.
- Before each commit: `cargo fmt && cargo clippy --all-targets && cargo build && cargo test` must pass. Never `--no-verify`. Never delete the DB or migration files.
- Do not stash or discard the user's working tree (there is an unrelated dirty `planning/gaps3.md`). Only `git add` the specific files each task names.
- Line-wrapping: prose in `.md` is not hard-wrapped.

## File Structure

| File | Responsibility | Tasks |
|---|---|---|
| `plugins/umbral-admin/css/theme.json` (new) | Single source of truth for the Tailwind `theme.extend` (colors, spacing, borderRadius, fontFamily, fontSize, boxShadow). | 1, 2, 3 |
| `plugins/umbral-admin/css/tailwind.config.js` | Production config; `require('./theme.json')`. | 1 |
| `plugins/umbral-admin/src/engine.rs` | Register the new pagination macro template; expose `admin_theme_json` global. | 1, 4 |
| `plugins/umbral-admin/templates/wrapper.html` | Light/dark CSS-var token blocks (palette); CDN config reads injected `admin_theme_json`. | 1, 2 |
| `plugins/umbral-admin/templates/_macros/pagination.html` (new) | Shared numbered pagination footer `<tr>`. | 4 |
| `plugins/umbral-admin/templates/_macros/data_table.html` | Use the shared pagination macro (delete inline footer nav). | 4 |
| `plugins/umbral-admin/templates/rows_fragment.html` | Use the shared pagination macro (delete compact footer). | 4 |
| `plugins/umbral-admin/templates/_macros/sheet.html` | Responsive panel width clamp + small-screen drawer + JS localStorage clamp. | 5 |
| `plugins/umbral-admin/templates/dashboard.html`, `_macros/widgets/*.html` | Apply the shared card recipe. | 3 |
| `plugins/umbral-admin/src/handlers/crud.rs` | Emit `refreshTable` on save-and-continue. | 6 |
| `plugins/umbral-admin/tests/*.rs` | New behavioral tests. | 1, 4, 6 |

Task order: **1 → 2 → 3** are sequential (2 and 3 add keys to the `theme.json` created in 1). **4, 5, 6** are independent of each other and of 1–3 (can be done in any order after 1 for task 4's macro registration). **7** is the final build + verify.

---

### Task 1: Single Tailwind theme source (fixes huge sidebar labels + table headers)

Collapse the two theme definitions into one `css/theme.json` read by both the compiled build and the dev CDN. This restores `text-label-sm` (11px) etc. in production, where the compiled `tailwind.config.js` currently defines only colors + spacing.

**Files:**
- Create: `plugins/umbral-admin/css/theme.json`
- Modify: `plugins/umbral-admin/css/tailwind.config.js`
- Modify: `plugins/umbral-admin/src/engine.rs:315` (add global after `restore_last_path`)
- Modify: `plugins/umbral-admin/templates/wrapper.html:468-569` (replace inline `theme.extend` with injected JSON)
- Test: `plugins/umbral-admin/tests/theme_single_source.rs` (new)

**Interfaces:**
- Produces: `css/theme.json` — a JSON object that is exactly the value of Tailwind's `theme.extend`. Both `tailwind.config.js` and `wrapper.html` consume it.
- Produces: MiniJinja global `admin_theme_json` (a safe string containing the raw JSON) available to all admin templates.

- [ ] **Step 1: Write the failing test**

Create `plugins/umbral-admin/tests/theme_single_source.rs`:

```rust
//! The Tailwind theme is defined once in css/theme.json and consumed by
//! both the compiled build (tailwind.config.js) and the dev CDN config
//! (wrapper.html via the `admin_theme_json` global). These tests pin that
//! single-source contract so the dev/prod drift that made sidebar labels
//! and table headers render at the default 16px can't silently return.

#[test]
fn theme_json_is_valid_and_defines_label_sm_fontsize() {
    let raw = include_str!("../css/theme.json");
    let v: serde_json::Value = serde_json::from_str(raw).expect("theme.json is valid JSON");
    // The class that broke in prod: text-label-sm must resolve to 11px.
    let label_sm = &v["fontSize"]["label-sm"];
    assert!(
        label_sm.is_array(),
        "fontSize.label-sm must be defined (array form): {label_sm}"
    );
    assert_eq!(
        label_sm[0].as_str(),
        Some("11px"),
        "label-sm must be 11px so sidebar labels + table headers stay slender"
    );
    // fontFamily and borderRadius must also be present (prod was missing them).
    assert!(v["fontFamily"]["label-sm"].is_array(), "fontFamily.label-sm present");
    assert!(v["borderRadius"]["xl"].is_string(), "borderRadius.xl present");
    assert!(v["colors"]["primary"].is_string(), "colors.primary present");
}

#[test]
fn tailwind_config_requires_theme_json() {
    let cfg = include_str!("../css/tailwind.config.js");
    assert!(
        cfg.contains("require('./theme.json')") || cfg.contains("require(\"./theme.json\")"),
        "tailwind.config.js must read the shared theme.json, not redefine the theme inline"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-admin --test theme_single_source`
Expected: FAIL — `theme.json` does not exist (compile error: `include_str!` missing file).

- [ ] **Step 3: Create `css/theme.json`**

Create `plugins/umbral-admin/css/theme.json` with the full `theme.extend` (copied from the current CDN inline config at `wrapper.html:472-566`, plus `success`/`warning` colors and a `boxShadow.card` token for later tasks):

```json
{
  "colors": {
    "surface-container-lowest": "var(--surface-container-lowest)",
    "surface-container-low": "var(--surface-container-low)",
    "surface-container": "var(--surface-container)",
    "surface-container-high": "var(--surface-container-high)",
    "surface-container-highest": "var(--surface-container-highest)",
    "surface-bright": "var(--surface-bright)",
    "surface-dim": "var(--surface-dim)",
    "surface": "var(--surface)",
    "surface-variant": "var(--surface-variant)",
    "surface-tint": "var(--surface-tint)",
    "on-surface": "var(--on-surface)",
    "on-surface-variant": "var(--on-surface-variant)",
    "inverse-surface": "var(--inverse-surface)",
    "inverse-on-surface": "var(--inverse-on-surface)",
    "outline": "var(--outline)",
    "outline-variant": "var(--outline-variant)",
    "divider": "var(--divider)",
    "divider-soft": "var(--divider-soft)",
    "background": "var(--background)",
    "on-background": "var(--on-background)",
    "primary": "var(--primary)",
    "on-primary": "var(--on-primary)",
    "primary-container": "var(--primary-container)",
    "on-primary-container": "var(--on-primary-container)",
    "primary-fixed": "var(--primary-fixed)",
    "primary-fixed-dim": "var(--primary-fixed-dim)",
    "on-primary-fixed": "var(--on-primary-fixed)",
    "on-primary-fixed-variant": "var(--on-primary-fixed-variant)",
    "inverse-primary": "var(--inverse-primary)",
    "secondary": "var(--secondary)",
    "on-secondary": "var(--on-secondary)",
    "secondary-container": "var(--secondary-container)",
    "on-secondary-container": "var(--on-secondary-container)",
    "secondary-fixed": "var(--secondary-fixed)",
    "secondary-fixed-dim": "var(--secondary-fixed-dim)",
    "on-secondary-fixed": "var(--on-secondary-fixed)",
    "on-secondary-fixed-variant": "var(--on-secondary-fixed-variant)",
    "tertiary": "var(--tertiary)",
    "on-tertiary": "var(--on-tertiary)",
    "tertiary-container": "var(--tertiary-container)",
    "on-tertiary-container": "var(--on-tertiary-container)",
    "tertiary-fixed": "var(--tertiary-fixed)",
    "tertiary-fixed-dim": "var(--tertiary-fixed-dim)",
    "on-tertiary-fixed": "var(--on-tertiary-fixed)",
    "on-tertiary-fixed-variant": "var(--on-tertiary-fixed-variant)",
    "success": "var(--success)",
    "on-success": "var(--on-success)",
    "success-container": "var(--success-container)",
    "on-success-container": "var(--on-success-container)",
    "warning": "var(--warning)",
    "on-warning": "var(--on-warning)",
    "warning-container": "var(--warning-container)",
    "on-warning-container": "var(--on-warning-container)",
    "error": "var(--error)",
    "on-error": "var(--on-error)",
    "error-container": "var(--error-container)",
    "on-error-container": "var(--on-error-container)"
  },
  "spacing": {
    "sidebar-width": "260px",
    "sidebar-collapsed": "68px",
    "topbar-height": "56px",
    "gutter": "20px",
    "base": "4px",
    "xs": "4px",
    "sm": "8px",
    "md": "16px",
    "lg": "24px",
    "xl": "32px"
  },
  "borderRadius": {
    "DEFAULT": "0.125rem",
    "sm": "0.125rem",
    "lg": "0.25rem",
    "xl": "0.5rem",
    "2xl": "0.75rem",
    "auth-card": "14px",
    "full": "9999px"
  },
  "boxShadow": {
    "card": "0 1px 2px rgba(0,0,0,0.04), 0 1px 3px rgba(0,0,0,0.06)",
    "card-hover": "0 2px 4px rgba(0,0,0,0.05), 0 4px 12px rgba(0,0,0,0.08)"
  },
  "fontFamily": {
    "display": ["Inter"],
    "h1": ["Inter"],
    "h2": ["Inter"],
    "h3": ["Inter"],
    "body-lg": ["Inter"],
    "body-md": ["Inter"],
    "body-sm": ["Inter"],
    "label-md": ["Inter"],
    "label-sm": ["Inter"],
    "data-mono": ["Inter"]
  },
  "fontSize": {
    "display": ["30px", { "lineHeight": "38px", "letterSpacing": "-0.02em", "fontWeight": "700" }],
    "h1": ["24px", { "lineHeight": "32px", "letterSpacing": "-0.015em", "fontWeight": "600" }],
    "h2": ["20px", { "lineHeight": "28px", "letterSpacing": "-0.01em", "fontWeight": "600" }],
    "h3": ["16px", { "lineHeight": "24px", "fontWeight": "600" }],
    "body-lg": ["16px", { "lineHeight": "24px", "fontWeight": "400" }],
    "body-md": ["14px", { "lineHeight": "20px", "fontWeight": "400" }],
    "body-sm": ["13px", { "lineHeight": "18px", "fontWeight": "400" }],
    "label-md": ["12px", { "lineHeight": "16px", "letterSpacing": "0.01em", "fontWeight": "500" }],
    "label-sm": ["11px", { "lineHeight": "14px", "letterSpacing": "0.03em", "fontWeight": "600" }],
    "data-mono": ["14px", { "lineHeight": "20px", "fontWeight": "400" }]
  }
}
```

- [ ] **Step 4: Rewrite `css/tailwind.config.js` to read the shared JSON**

Replace the entire file with:

```javascript
/** @type {import('tailwindcss').Config} */
// Single source of truth for the theme lives in ./theme.json — the same
// object is injected into the dev CDN config in wrapper.html via the
// `admin_theme_json` MiniJinja global. Keep all token edits in theme.json
// so the compiled build and the CDN can never drift (that drift is what
// made `text-label-sm` resolve to nothing in prod → huge sidebar labels).
module.exports = {
  darkMode: 'class',
  content: [
    '../templates/**/*.html',
    '../src/**/*.rs',
  ],
  theme: {
    extend: require('./theme.json'),
  },
  plugins: [],
};
```

- [ ] **Step 5: Expose `admin_theme_json` as a MiniJinja global**

In `plugins/umbral-admin/src/engine.rs`, immediately after the `restore_last_path` global block (ends at line 315), add:

```rust
        // Single-source Tailwind theme. The dev CDN config in wrapper.html
        // renders `theme: { extend: {{ admin_theme_json }} }` from this, the
        // same JSON the compiled build reads via tailwind.config.js →
        // require('./theme.json'). from_safe_string so the JSON's quotes and
        // braces aren't HTML-entity-escaped inside the <script> tag.
        env.add_global(
            "admin_theme_json",
            minijinja::Value::from_safe_string(include_str!("../css/theme.json").to_string()),
        );
```

- [ ] **Step 6: Make the CDN config in `wrapper.html` read the injected JSON**

In `plugins/umbral-admin/templates/wrapper.html`, replace the whole `<script id="tailwind-config">` block (lines 468-570, from `<script id="tailwind-config">` through its closing `</script>`) with:

```html
  <script id="tailwind-config">
    // Dev-only Tailwind CDN config. The theme is injected from css/theme.json
    // (the SAME file the compiled build reads), so dev and prod never drift.
    tailwind.config = {
      darkMode: "class",
      theme: {
        extend: {{ admin_theme_json }},
      },
    };
  </script>
```

- [ ] **Step 7: Run the test to verify it passes**

Run: `cargo test -p umbral-admin --test theme_single_source`
Expected: PASS (both tests).

- [ ] **Step 8: Verify the template still parses and renders**

Run: `cargo test -p umbral-admin --test phase1_shell`
Expected: PASS — wrapper.html still renders (the `{{ admin_theme_json }}` global resolves; no MiniJinja parse error).

- [ ] **Step 9: Commit**

```bash
git add plugins/umbral-admin/css/theme.json plugins/umbral-admin/css/tailwind.config.js plugins/umbral-admin/src/engine.rs plugins/umbral-admin/templates/wrapper.html plugins/umbral-admin/tests/theme_single_source.rs
git commit -m "fix(admin): single Tailwind theme source (theme.json)

The compiled tailwind.config.js defined only colors+spacing while the dev
CDN config defined fontSize/fontFamily/borderRadius too, so text-label-sm
generated nothing in production and sidebar labels + table headers fell
back to 16px. Move the whole theme.extend into css/theme.json, read by
both the build (require) and the CDN config (admin_theme_json global).

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: Neutral palette (pure white / near-black, monochrome primary)

Rewrite the CSS-var token blocks in `wrapper.html` to a zero-chroma grey ramp with monochrome primary and colored status tokens.

**Files:**
- Modify: `plugins/umbral-admin/templates/wrapper.html:155-260` (the `:root` and `.dark` blocks)
- Test: `plugins/umbral-admin/tests/theme_neutral_palette.rs` (new)

**Interfaces:**
- Consumes: the color tokens are referenced by `theme.json` (Task 1) as `var(--token)`. This task sets their values and adds `--success*` / `--warning*`.
- Produces: neutral palette; `--success`, `--warning` (+ `-container` / `on-` variants) in both `:root` and `.dark`.

- [ ] **Step 1: Write the failing test**

Create `plugins/umbral-admin/tests/theme_neutral_palette.rs`:

```rust
//! Pin the neutral palette so the warm "brownish" tint can't creep back.
//! We assert against the raw wrapper.html source (the token block is static
//! CSS, not template-rendered) for the load-bearing values.

const WRAPPER: &str = include_str!("../templates/wrapper.html");

#[test]
fn light_background_is_pure_white() {
    // :root (light) --background must be pure white, zero chroma.
    assert!(
        WRAPPER.contains("--background:               oklch(1 0 0);"),
        "light --background must be pure white oklch(1 0 0)"
    );
}

#[test]
fn dark_background_is_near_black_neutral() {
    // .dark --background near-black neutral, NOT true black, zero chroma.
    assert!(
        WRAPPER.contains("--background:               oklch(0.16 0 0);"),
        "dark --background must be near-black neutral oklch(0.16 0 0)"
    );
}

#[test]
fn status_tokens_exist() {
    for tok in ["--success:", "--warning:", "--success-container:", "--warning-container:"] {
        assert!(
            WRAPPER.matches(tok).count() >= 2,
            "{tok} must be defined in both :root and .dark"
        );
    }
}

#[test]
fn no_warm_hue_left_on_core_surfaces() {
    // The old palette used hue ~75/90/95 with chroma on surfaces. After the
    // refresh, the surface/background/on-surface neutrals are zero-chroma
    // (`0 0`). Guard the specific lines that were brown before.
    assert!(
        !WRAPPER.contains("oklch(0.16 0.009 75)"),
        "old warm dark --surface must be gone"
    );
    assert!(
        !WRAPPER.contains("oklch(0.985 0.005 95)"),
        "old warm light --surface must be gone"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-admin --test theme_neutral_palette`
Expected: FAIL (the pure-white/near-black lines and status tokens don't exist yet; the old warm values still present).

- [ ] **Step 3: Rewrite the `:root` (light) token block**

In `plugins/umbral-admin/templates/wrapper.html`, replace the `:root { ... }` block (lines 155-209) with this neutral version. Keep the alignment style (token name, then value) so the test's exact-substring matches hold:

```css
    :root {
      --surface-container-lowest: oklch(1 0 0);
      --surface-container-low:    oklch(0.985 0 0);
      --surface-container:        oklch(0.97 0 0);
      --surface-container-high:   oklch(0.95 0 0);
      --surface-container-highest:oklch(0.93 0 0);
      --surface-bright:           oklch(1 0 0);
      --surface-dim:              oklch(0.96 0 0);
      --surface:                  oklch(1 0 0);
      --surface-variant:          oklch(0.95 0 0);
      --surface-tint:             oklch(0.20 0 0);
      --on-surface:               oklch(0.20 0 0);
      --on-surface-variant:       oklch(0.45 0 0);
      --inverse-surface:          oklch(0.20 0 0);
      --inverse-on-surface:       oklch(0.97 0 0);
      --outline:                  oklch(0.70 0 0);
      --outline-variant:          oklch(0.90 0 0);
      --divider:                  oklch(0 0 0 / 0.08);
      --divider-soft:             oklch(0 0 0 / 0.05);
      --background:               oklch(1 0 0);
      --on-background:            oklch(0.20 0 0);
      --primary:                  oklch(0.20 0 0);
      --on-primary:               oklch(1 0 0);
      --primary-container:        oklch(0.95 0 0);
      --on-primary-container:     oklch(0.20 0 0);
      --primary-fixed:            oklch(0.95 0 0);
      --primary-fixed-dim:        oklch(0.88 0 0);
      --on-primary-fixed:         oklch(0.20 0 0);
      --on-primary-fixed-variant: oklch(0.35 0 0);
      --inverse-primary:          oklch(0.80 0 0);
      --secondary:                oklch(0.45 0 0);
      --on-secondary:             oklch(1 0 0);
      --secondary-container:      oklch(0.95 0 0);
      --on-secondary-container:   oklch(0.25 0 0);
      --secondary-fixed:          oklch(0.95 0 0);
      --secondary-fixed-dim:      oklch(0.88 0 0);
      --on-secondary-fixed:       oklch(0.25 0 0);
      --on-secondary-fixed-variant:oklch(0.42 0 0);
      --tertiary:                 oklch(0.45 0 0);
      --on-tertiary:              oklch(1 0 0);
      --tertiary-container:       oklch(0.95 0 0);
      --on-tertiary-container:    oklch(0.25 0 0);
      --tertiary-fixed:           oklch(0.95 0 0);
      --tertiary-fixed-dim:       oklch(0.88 0 0);
      --on-tertiary-fixed:        oklch(0.25 0 0);
      --on-tertiary-fixed-variant:oklch(0.42 0 0);
      --success:                  oklch(0.62 0.17 145);
      --on-success:               oklch(1 0 0);
      --success-container:        oklch(0.93 0.06 145);
      --on-success-container:     oklch(0.30 0.07 145);
      --warning:                  oklch(0.70 0.15 75);
      --on-warning:               oklch(0.20 0 0);
      --warning-container:        oklch(0.93 0.07 75);
      --on-warning-container:     oklch(0.35 0.08 75);
      --error:                    oklch(0.577 0.245 27.325);
      --on-error:                 oklch(1 0 0);
      --error-container:          oklch(0.93 0.07 27.325);
      --on-error-container:       oklch(0.25 0.08 27.325);
    }
```

- [ ] **Step 4: Rewrite the `.dark` token block**

Replace the `.dark { ... }` block (lines 210-260) with:

```css
    .dark {
      --surface-container-lowest: oklch(0.14 0 0);
      --surface-container-low:    oklch(0.18 0 0);
      --surface-container:        oklch(0.21 0 0);
      --surface-container-high:   oklch(0.25 0 0);
      --surface-container-highest:oklch(0.29 0 0);
      --surface-bright:           oklch(0.32 0 0);
      --surface-dim:              oklch(0.14 0 0);
      --surface:                  oklch(0.18 0 0);
      --surface-variant:          oklch(0.27 0 0);
      --surface-tint:             oklch(0.96 0 0);
      --on-surface:               oklch(0.96 0 0);
      --on-surface-variant:       oklch(0.70 0 0);
      --inverse-surface:          oklch(0.96 0 0);
      --inverse-on-surface:       oklch(0.18 0 0);
      --outline:                  oklch(0.55 0 0);
      --outline-variant:          oklch(0.32 0 0);
      --divider:                  oklch(1 0 0 / 0.08);
      --divider-soft:             oklch(1 0 0 / 0.05);
      --background:               oklch(0.16 0 0);
      --on-background:            oklch(0.96 0 0);
      --primary:                  oklch(0.96 0 0);
      --on-primary:               oklch(0.18 0 0);
      --primary-container:        oklch(0.28 0 0);
      --on-primary-container:     oklch(0.96 0 0);
      --primary-fixed:            oklch(0.95 0 0);
      --primary-fixed-dim:        oklch(0.82 0 0);
      --on-primary-fixed:         oklch(0.20 0 0);
      --on-primary-fixed-variant: oklch(0.35 0 0);
      --inverse-primary:          oklch(0.20 0 0);
      --secondary:                oklch(0.70 0 0);
      --on-secondary:             oklch(0.18 0 0);
      --secondary-container:      oklch(0.25 0 0);
      --on-secondary-container:   oklch(0.96 0 0);
      --secondary-fixed:          oklch(0.90 0 0);
      --secondary-fixed-dim:      oklch(0.78 0 0);
      --on-secondary-fixed:       oklch(0.20 0 0);
      --on-secondary-fixed-variant:oklch(0.42 0 0);
      --tertiary:                 oklch(0.70 0 0);
      --on-tertiary:              oklch(0.18 0 0);
      --tertiary-container:       oklch(0.25 0 0);
      --on-tertiary-container:    oklch(0.96 0 0);
      --tertiary-fixed:           oklch(0.90 0 0);
      --tertiary-fixed-dim:       oklch(0.78 0 0);
      --on-tertiary-fixed:        oklch(0.20 0 0);
      --on-tertiary-fixed-variant:oklch(0.42 0 0);
      --success:                  oklch(0.72 0.16 145);
      --on-success:               oklch(0.18 0 0);
      --success-container:        oklch(0.28 0.06 145);
      --on-success-container:     oklch(0.90 0.06 145);
      --warning:                  oklch(0.78 0.14 75);
      --on-warning:               oklch(0.18 0 0);
      --warning-container:        oklch(0.30 0.06 75);
      --on-warning-container:     oklch(0.92 0.07 75);
      --error:                    oklch(0.704 0.191 22.216);
      --on-error:                 oklch(0.18 0 0);
      --error-container:          oklch(0.28 0.09 22.216);
      --on-error-container:       oklch(0.95 0.055 22.216);
    }
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p umbral-admin --test theme_neutral_palette`
Expected: PASS (all four tests).

- [ ] **Step 6: Verify nothing else broke**

Run: `cargo test -p umbral-admin --test phase1_shell --test phase4_dashboard`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add plugins/umbral-admin/templates/wrapper.html plugins/umbral-admin/tests/theme_neutral_palette.rs
git commit -m "feat(admin): neutral pure-white / near-black palette

Zero-chroma grey ramp replaces the warm brown-tinted neutrals. Light bg
is pure white, dark is near-black neutral (not true black), primary is
monochrome (ink-on-paper / paper-on-ink). Adds colored success/warning
status tokens alongside error.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: Standard card system

Add the `shadow-card` elevation (already in `theme.json` boxShadow from Task 1) and apply one consistent card recipe across the dashboard stat strip, model cards, and widget cards.

**Files:**
- Modify: `plugins/umbral-admin/templates/dashboard.html` (stat strip lines 47-60; model-card + widget grid below)
- Modify: `plugins/umbral-admin/templates/_macros/widgets/card.html`, `kpi.html` (card containers)
- Test: `plugins/umbral-admin/tests/phase4_dashboard.rs` (extend — add card-recipe assertion)

**Interfaces:**
- Consumes: `boxShadow.card` from `theme.json` (Task 1) → utility class `shadow-card`.
- Produces: every dashboard/widget card container uses `bg-surface border border-outline-variant rounded-xl shadow-card`.

- [ ] **Step 1: Write the failing test**

Append to `plugins/umbral-admin/tests/phase4_dashboard.rs` (match the existing harness in that file — reuse its `boot()`/`login_session`/`send` helpers; mirror an existing dashboard test for the request setup):

```rust
#[tokio::test]
async fn test_dashboard_cards_use_shared_card_recipe() {
    let router = boot().await.clone();
    let session = login_session(router.clone(), "dash_admin", "password123").await;
    let req = axum::http::Request::builder()
        .uri("/admin/")
        .header(axum::http::header::COOKIE, format!("umbral_session={session}"))
        .body(axum::body::Body::empty())
        .unwrap();
    let (status, _h, body) = send(router, req).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    // The quick-stats cards must carry the shared elevation token so the
    // dashboard reads as one card system, not ad-hoc borders.
    assert!(
        body.contains("shadow-card"),
        "dashboard cards must use the shared shadow-card recipe"
    );
}
```

> If `phase4_dashboard.rs` uses different helper names, adapt the request-construction lines to match its existing tests; the assertion (`body.contains("shadow-card")`) is the contract.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-admin --test phase4_dashboard test_dashboard_cards_use_shared_card_recipe`
Expected: FAIL — `shadow-card` not present in the dashboard output yet.

- [ ] **Step 3: Apply the card recipe in `dashboard.html`**

In `plugins/umbral-admin/templates/dashboard.html`, update the quick-stats cards (lines 49, 53, 57, 62) and the model-card grid containers. Change each card container class from:

```
bg-surface-container border border-outline-variant rounded-xl
```

to the shared recipe:

```
bg-surface border border-outline-variant rounded-xl shadow-card
```

Apply the same change to the model cards and the 12-col widget placeholders in the same file (every element whose class currently starts `bg-surface-container border border-outline-variant rounded-xl` and represents a card surface). Keep the existing padding/sizing utilities.

- [ ] **Step 4: Apply the recipe in the widget macros**

In `plugins/umbral-admin/templates/_macros/widgets/card.html` and `_macros/widgets/kpi.html`, update the outer card container class to `bg-surface border border-outline-variant rounded-xl shadow-card` (preserve inner layout classes). Leave the small icon chips (`w-9 h-9 rounded-lg ...`) as-is — those are not cards.

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p umbral-admin --test phase4_dashboard test_dashboard_cards_use_shared_card_recipe`
Expected: PASS.

- [ ] **Step 6: Run the full dashboard suite**

Run: `cargo test -p umbral-admin --test phase4_dashboard`
Expected: PASS (no existing dashboard assertion regressed).

- [ ] **Step 7: Commit**

```bash
git add plugins/umbral-admin/templates/dashboard.html plugins/umbral-admin/templates/_macros/widgets/card.html plugins/umbral-admin/templates/_macros/widgets/kpi.html plugins/umbral-admin/tests/phase4_dashboard.rs
git commit -m "feat(admin): unified card recipe with shadow-card elevation

One card recipe (bg-surface + hairline border + rounded-xl + shadow-card)
across the dashboard stat strip, model cards, and widgets instead of
ad-hoc per-template classes. Reads cleanly on the new white/near-black
surfaces.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: Shared long-form pagination macro (fixes short-form after HTMX swap)

Extract the numbered/windowed footer into one macro used by both the initial render (`data_table.html`) and the HTMX `/rows` swap (`rows_fragment.html`), so the long form persists after every sort/search/page.

**Files:**
- Create: `plugins/umbral-admin/templates/_macros/pagination.html`
- Modify: `plugins/umbral-admin/src/engine.rs` (register the new macro template near line 131)
- Modify: `plugins/umbral-admin/templates/_macros/data_table.html:595-720` (replace footer `<tr>` with macro call)
- Modify: `plugins/umbral-admin/templates/rows_fragment.html:245-311` (replace compact footer `<tr>` with macro call)
- Test: `plugins/umbral-admin/tests/phase2_datatable.rs` (extend — assert numbered footer in the `/rows` swap)

**Interfaces:**
- Produces: macro `pagination_footer(admin_base, table, columns, search_val, filter_qs, sort_col, sort_order, pagination)` → a full `<tr>...</tr>` footer with range text, page-size select, and numbered nav (first / prev / `1 … window … last` / next / last).
- Consumes (call sites): `data_table.html` passes `table=model.table`; `rows_fragment.html` passes `table=table`. Both already have `pagination`, `columns`, `admin_base`, `search_val`, `filter_qs`, `sort_col`, `sort_order` in scope.

- [ ] **Step 1: Write the failing test**

Append to `plugins/umbral-admin/tests/phase2_datatable.rs` (reuse its existing `boot()`/`login_session`/`send` helpers; mirror an existing `/rows` test for setup). The contract: a `/rows` swap of a multi-page table renders **numbered** buttons and the ellipsis, not the compact `page / total` form.

```rust
#[tokio::test]
async fn test_rows_swap_keeps_numbered_pagination() {
    let router = boot().await.clone();
    let session = login_session(router.clone(), "dt_admin", "password123").await;
    // Force many pages: page_size=10 over the seeded rows. Adjust the table
    // name / seeding to match this file's existing fixtures.
    let req = axum::http::Request::builder()
        .uri("/admin/note/rows?page=1&page_size=10")
        .header(axum::http::header::COOKIE, format!("umbral_session={session}"))
        .header("hx-request", "true")
        .body(axum::body::Body::empty())
        .unwrap();
    let (status, _h, body) = send(router, req).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    // Numbered nav present: an explicit page-1 button rendered as `>1<`.
    assert!(body.contains(">1</button>"), "numbered page button must render in the /rows swap");
    // The compact `page / total_pages` form must be gone.
    assert!(
        !body.contains("/ {{ pagination.total_pages }}") && !body.contains("} / {"),
        "compact 'page / total' footer must not appear in the swap"
    );
}
```

> If the seeded fixture has only one page, the numbered nav still renders page `1`; the assertion holds. If this file seeds a different model than `note`, swap the URI to that model.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-admin --test phase2_datatable test_rows_swap_keeps_numbered_pagination`
Expected: FAIL — `rows_fragment.html` currently renders the compact `{{ pagination.page }} / {{ pagination.total_pages }}` form, no `>1</button>`.

- [ ] **Step 3: Create the shared macro**

Create `plugins/umbral-admin/templates/_macros/pagination.html` with the full numbered footer (lifted verbatim from `data_table.html:596-720`, parameterized on `table`):

```html
{#
  pagination.html — shared numbered pagination footer.
  Used by BOTH data_table.html (initial render) and rows_fragment.html
  (HTMX /rows swap) so the long numbered form persists across every
  sort/search/page interaction. One definition → no drift.

  Params:
    admin_base, table, columns, search_val, filter_qs, sort_col,
    sort_order, pagination ({ page, page_size, total, total_pages })
#}
{% macro pagination_footer(admin_base, table, columns, search_val, filter_qs, sort_col, sort_order, pagination) %}
<tr class="bg-surface border-t border-outline-variant">
  <td colspan="{{ columns | length + 2 }}" class="px-lg py-md">
    <div class="flex flex-wrap items-center justify-between gap-md">
      {# Range text + page size #}
      <div class="flex items-center gap-lg">
        <span class="font-body-sm text-body-sm text-on-surface-variant">
          Showing
          <span class="text-on-surface font-medium tabular-nums">
            {{ (pagination.page - 1) * pagination.page_size + 1 }}–{{ [pagination.page * pagination.page_size, pagination.total] | min }}
          </span>
          of
          <span class="text-on-surface font-medium tabular-nums">{{ pagination.total }}</span>
        </span>
        <div class="flex items-center gap-sm">
          <span class="font-label-md text-label-md text-on-surface-variant">Rows per page:</span>
          <select id="dt-page-size" name="page_size"
            hx-get="{{ admin_base }}/{{ table }}/rows"
            hx-trigger="change"
            hx-target="#table-body"
            hx-swap="innerHTML"
            hx-include="#dt-search, #dt-active-filters input, #dt-sort-col, #dt-sort-order"
            hx-push-url="true"
            class="bg-surface-container border border-outline-variant rounded-lg px-sm py-xs pr-8 min-w-[5rem] font-label-md text-label-md text-on-surface focus:outline-none focus:ring-1 focus:ring-primary">
            {% for sz in [10, 25, 50, 100] %}
            <option value="{{ sz }}" {% if sz == pagination.page_size %}selected{% endif %}>{{ sz }}</option>
            {% endfor %}
          </select>
        </div>
      </div>

      {# Numbered navigation: first / prev / 1 … window … last / next / last #}
      <div class="flex items-center gap-xs">
        <button type="button"
          {% if pagination.page <= 1 %}disabled{% endif %}
          hx-get="{{ admin_base }}/{{ table }}/rows?search={{ search_val }}{{ filter_qs }}&sort={{ sort_col }}&order={{ sort_order }}&page=1&page_size={{ pagination.page_size }}"
          hx-target="#table-body" hx-swap="innerHTML" hx-push-url="true"
          class="p-sm text-on-surface-variant hover:bg-surface-container-high rounded-xl disabled:opacity-30 disabled:cursor-not-allowed transition-colors" title="First page">
          <i data-lucide="chevrons-left" class="w-4 h-4"></i>
        </button>
        <button type="button"
          {% if pagination.page <= 1 %}disabled{% endif %}
          hx-get="{{ admin_base }}/{{ table }}/rows?search={{ search_val }}{{ filter_qs }}&sort={{ sort_col }}&order={{ sort_order }}&page={{ pagination.page - 1 }}&page_size={{ pagination.page_size }}"
          hx-target="#table-body" hx-swap="innerHTML" hx-push-url="true"
          class="p-sm text-on-surface-variant hover:bg-surface-container-high rounded-xl disabled:opacity-30 disabled:cursor-not-allowed transition-colors" title="Previous page">
          <i data-lucide="chevron-left" class="w-4 h-4"></i>
        </button>

        <div class="flex items-center">
          {% set win_start = [pagination.page - 2, 2] | max %}
          {% set win_end   = [pagination.page + 2, pagination.total_pages - 1] | min %}

          <button type="button"
            hx-get="{{ admin_base }}/{{ table }}/rows?search={{ search_val }}{{ filter_qs }}&sort={{ sort_col }}&order={{ sort_order }}&page=1&page_size={{ pagination.page_size }}"
            hx-target="#table-body" hx-swap="innerHTML" hx-push-url="true"
            class="w-8 h-8 flex items-center justify-center {% if pagination.page == 1 %}bg-primary text-on-primary{% else %}text-on-surface-variant hover:bg-surface-container-high{% endif %} rounded-lg font-label-md text-label-md transition-colors tabular-nums"
          >1</button>

          {% if win_start > 2 %}
          <span class="px-xs text-on-surface-variant font-label-md">…</span>
          {% endif %}

          {% for p in range(win_start, win_end + 1) %}
          <button type="button"
            hx-get="{{ admin_base }}/{{ table }}/rows?search={{ search_val }}{{ filter_qs }}&sort={{ sort_col }}&order={{ sort_order }}&page={{ p }}&page_size={{ pagination.page_size }}"
            hx-target="#table-body" hx-swap="innerHTML" hx-push-url="true"
            class="w-8 h-8 flex items-center justify-center {% if pagination.page == p %}bg-primary text-on-primary{% else %}text-on-surface-variant hover:bg-surface-container-high{% endif %} rounded-lg font-label-md text-label-md transition-colors tabular-nums"
          >{{ p }}</button>
          {% endfor %}

          {% if win_end < pagination.total_pages - 1 %}
          <span class="px-xs text-on-surface-variant font-label-md">…</span>
          {% endif %}

          {% if pagination.total_pages > 1 %}
          <button type="button"
            hx-get="{{ admin_base }}/{{ table }}/rows?search={{ search_val }}{{ filter_qs }}&sort={{ sort_col }}&order={{ sort_order }}&page={{ pagination.total_pages }}&page_size={{ pagination.page_size }}"
            hx-target="#table-body" hx-swap="innerHTML" hx-push-url="true"
            class="w-8 h-8 flex items-center justify-center {% if pagination.page == pagination.total_pages %}bg-primary text-on-primary{% else %}text-on-surface-variant hover:bg-surface-container-high{% endif %} rounded-lg font-label-md text-label-md transition-colors tabular-nums"
          >{{ pagination.total_pages }}</button>
          {% endif %}
        </div>

        <button type="button"
          {% if pagination.page >= pagination.total_pages %}disabled{% endif %}
          hx-get="{{ admin_base }}/{{ table }}/rows?search={{ search_val }}{{ filter_qs }}&sort={{ sort_col }}&order={{ sort_order }}&page={{ pagination.page + 1 }}&page_size={{ pagination.page_size }}"
          hx-target="#table-body" hx-swap="innerHTML" hx-push-url="true"
          class="p-sm text-on-surface-variant hover:bg-surface-container-high rounded-xl disabled:opacity-30 disabled:cursor-not-allowed transition-colors" title="Next page">
          <i data-lucide="chevron-right" class="w-4 h-4"></i>
        </button>
        <button type="button"
          {% if pagination.page >= pagination.total_pages %}disabled{% endif %}
          hx-get="{{ admin_base }}/{{ table }}/rows?search={{ search_val }}{{ filter_qs }}&sort={{ sort_col }}&order={{ sort_order }}&page={{ pagination.total_pages }}&page_size={{ pagination.page_size }}"
          hx-target="#table-body" hx-swap="innerHTML" hx-push-url="true"
          class="p-sm text-on-surface-variant hover:bg-surface-container-high rounded-xl disabled:opacity-30 disabled:cursor-not-allowed transition-colors" title="Last page">
          <i data-lucide="chevrons-right" class="w-4 h-4"></i>
        </button>
      </div>
    </div>
  </td>
</tr>
{% endmacro %}
```

- [ ] **Step 4: Register the macro template in `engine.rs`**

In `plugins/umbral-admin/src/engine.rs`, after the `data_table.html` registration block (ends at line 134), add:

```rust
        env.add_template(
            "admin/_macros/pagination.html",
            include_str!("../templates/_macros/pagination.html"),
        )
        .expect("admin/_macros/pagination.html parses");
```

- [ ] **Step 5: Use the macro in `data_table.html`**

In `plugins/umbral-admin/templates/_macros/data_table.html`, add the import near the existing imports at the top (line 38 area):

```html
{% from "admin/_macros/pagination.html" import pagination_footer %}
```

Then replace the entire footer `<tr>` block (lines 595/596 `{# ---- Pagination footer ... ---- #}` + `<tr class="bg-surface border-t border-outline-variant">` through its closing `</tr>` around line 720) with a single call:

```html
        {# ---- Pagination footer (shared macro; long numbered form) ---- #}
        {{ pagination_footer(admin_base, model.table, columns, search_val, filter_qs, sort_col, sort_order, pagination) }}
```

- [ ] **Step 6: Use the macro in `rows_fragment.html`**

In `plugins/umbral-admin/templates/rows_fragment.html`, add the import at the top of the file (before the first rendered markup):

```html
{% from "admin/_macros/pagination.html" import pagination_footer %}
```

Then replace the compact footer `<tr>` block (lines 245/246 `{# Pagination footer row ... #}` + `<tr class="bg-surface border-t border-outline-variant">` through its closing `</tr>` around line 311) with:

```html
{# Pagination footer (shared macro; long numbered form, same as first load) #}
{{ pagination_footer(admin_base, table, columns, search_val, filter_qs, sort_col, sort_order, pagination) }}
```

- [ ] **Step 7: Run the test to verify it passes**

Run: `cargo test -p umbral-admin --test phase2_datatable test_rows_swap_keeps_numbered_pagination`
Expected: PASS.

- [ ] **Step 8: Run the full datatable suite**

Run: `cargo test -p umbral-admin --test phase2_datatable`
Expected: PASS (the initial-render footer assertions still hold — same markup, now via macro).

- [ ] **Step 9: Commit**

```bash
git add plugins/umbral-admin/templates/_macros/pagination.html plugins/umbral-admin/src/engine.rs plugins/umbral-admin/templates/_macros/data_table.html plugins/umbral-admin/templates/rows_fragment.html plugins/umbral-admin/tests/phase2_datatable.rs
git commit -m "fix(admin): keep long numbered pagination after HTMX swaps

The numbered footer lived only in data_table.html (first load); the /rows
swap rendered a compact 'page X / Y'. Extract one shared pagination_footer
macro used by both so the long form persists across sort/search/page.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: Responsive create/edit sheet (fixes small-screen overflow)

Clamp the sheet width to the viewport and make it a near-full-width drawer below the `sm` breakpoint; clamp the localStorage-restored width on load.

**Files:**
- Modify: `plugins/umbral-admin/templates/_macros/sheet.html:19-22` (panel classes/style) and `:281-307` (drag-resize JS)
- Test: `plugins/umbral-admin/tests/phase2_sheet.rs` (extend — assert the responsive markup)

**Interfaces:**
- Produces: the sheet panel uses `width: min(var(--sheet-width, 640px), 100vw - 1rem)` and small-screen inset classes; the restore-width JS clamps to `window.innerWidth - 16`.

- [ ] **Step 1: Write the failing test**

Append to `plugins/umbral-admin/tests/phase2_sheet.rs` (reuse its harness; mirror an existing sheet test for setup). Assert against the rendered new-sheet / create-sheet fragment:

```rust
#[tokio::test]
async fn test_sheet_panel_is_viewport_clamped() {
    let _g = NOTE_LOCK.lock().await;
    let router = boot().await.clone();
    let session = login_session(router.clone(), "sheet_admin", "password123").await;
    let req = Request::builder()
        .uri("/admin/note/new-sheet")
        .header(header::COOKIE, format!("umbral_session={session}"))
        .header("hx-request", "true")
        .body(Body::empty())
        .unwrap();
    let (status, _h, body) = send(router, req).await;
    assert_eq!(status, StatusCode::OK);
    // Width must be clamped to the viewport so a 640px panel can't overflow
    // a phone.
    assert!(
        body.contains("min(var(--sheet-width, 640px), 100vw - 1rem)"),
        "sheet width must be viewport-clamped: {body}"
    );
    // Small-screen drawer: pinned to both edges below the sm breakpoint.
    assert!(
        body.contains("left-2 right-2") || body.contains("inset-x-2"),
        "sheet must become a full-width drawer on small screens"
    );
}
```

> Confirm the helper/import names (`NOTE_LOCK`, `boot`, `login_session`, `send`, `Request`, `Body`, `header`, `StatusCode`) match the top of `phase2_sheet.rs`; they are used by the existing tests in that file (see the gaps2 #13 tests).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-admin --test phase2_sheet test_sheet_panel_is_viewport_clamped`
Expected: FAIL — the panel currently uses a hard `width: var(--sheet-width, 640px)` and `right-4` with no viewport clamp.

- [ ] **Step 3: Make the panel responsive**

In `plugins/umbral-admin/templates/_macros/sheet.html`, replace the panel opening `<div>` (lines 19-22) with:

```html
<div
  id="umbral-sheet-panel"
  class="fixed top-2 bottom-2 left-2 right-2 sm:top-4 sm:bottom-4 sm:left-auto sm:right-4 z-[110] flex flex-col bg-surface-container border border-outline-variant rounded-[14px] shadow-2xl overflow-hidden transition-transform duration-300"
  style="width: min(var(--sheet-width, 640px), 100vw - 1rem); transform: translateX(-{{ offset }}px);"
  role="dialog"
  aria-modal="true"
  aria-label="{{ sheet_title }}"
  data-table="{{ table }}"
```

Rationale: below `sm` the panel is pinned to all four edges (`left-2 right-2`) → full-width drawer; the `width` is ignored when both `left` and `right` are set. At `sm+`, `left-auto` releases the left edge so the `width` (clamped via `min(...)`) takes over and it floats on the right as before.

- [ ] **Step 4: Clamp the restored width and guard the drag handle in the JS**

In the same file, replace the drag-resize script's restore line (line 287) so a width saved on a desktop can't overflow a phone, and disable dragging when there's no room. Replace:

```javascript
  var saved = localStorage.getItem('umbral-admin-sheet-width');
  if (saved) { panel.style.width = saved + 'px'; document.documentElement.style.setProperty('--sheet-width', saved + 'px'); }
```

with:

```javascript
  var saved = localStorage.getItem('umbral-admin-sheet-width');
  if (saved) {
    // Clamp a width saved on a wide screen to the current viewport so the
    // panel never overflows on a narrow one.
    var clamped = Math.min(parseInt(saved, 10) || 640, window.innerWidth - 16);
    panel.style.width = clamped + 'px';
    document.documentElement.style.setProperty('--sheet-width', clamped + 'px');
  }
  // Below the sm breakpoint the panel is a full-width drawer (left+right
  // pinned); resizing has no room, so the handle does nothing there.
  if (window.matchMedia('(max-width: 639px)').matches) return;
```

(The early `return` sits after the `handle`/`panel` null-guard at the top of the IIFE and before the `mousedown` wiring, so the rest of the drag logic is skipped on small screens.)

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p umbral-admin --test phase2_sheet test_sheet_panel_is_viewport_clamped`
Expected: PASS.

- [ ] **Step 6: Run the full sheet suite**

Run: `cargo test -p umbral-admin --test phase2_sheet`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add plugins/umbral-admin/templates/_macros/sheet.html plugins/umbral-admin/tests/phase2_sheet.rs
git commit -m "fix(admin): make create/edit sheet responsive on small screens

Clamp the panel width to min(sheet-width, 100vw - 1rem) and turn it into a
full-width drawer below the sm breakpoint; clamp the localStorage-restored
width to the viewport and disable drag-resize on phones. Fixes horizontal
overflow on narrow screens.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: Refresh the changelist on save-and-continue

Make save-and-continue emit a `refreshTable` HX-Trigger (sheet stays open, underlying table updates). Create and plain-save already fire it; the existing front-end listener already preserves the URL kwargs.

**Files:**
- Modify: `plugins/umbral-admin/src/handlers/crud.rs:693-695` (the `_save_continue` branch)
- Test: `plugins/umbral-admin/tests/phase2_sheet.rs` (extend)

**Interfaces:**
- Consumes: `edit_sheet_handler(State, headers, Path((table, id)))` → `Response` (the sheet re-render). Defined in `plugins/umbral-admin/src/handlers/sheet.rs:92`.
- Produces: on save-and-continue, the response carries `HX-Trigger` containing `refreshTable` but NOT `closeSheet` (the sheet stays open).

- [ ] **Step 1: Write the failing test**

Append to `plugins/umbral-admin/tests/phase2_sheet.rs`:

```rust
#[tokio::test]
async fn test_save_and_continue_refreshes_table_but_keeps_sheet_open() {
    let _g = NOTE_LOCK.lock().await;
    let router = boot().await.clone();
    let session = login_session(router.clone(), "sheet_admin", "password123").await;

    // _save_continue=1 → re-render the sheet AND refresh the underlying table.
    let body = "title=SaveContinueNote&body=Stay&published=true&_save_continue=1";
    let req = Request::builder()
        .method("POST")
        .uri("/admin/note/1/edit")
        .header(header::COOKIE, format!("umbral_session={session}"))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header("hx-request", "true")
        .body(Body::from(body))
        .unwrap();
    let (status, headers, resp_body) = send(router, req).await;
    assert_eq!(status, StatusCode::OK, "save-and-continue returns 200");
    let trigger = headers
        .get("hx-trigger")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        trigger.contains("refreshTable"),
        "save-and-continue must refresh the underlying table: {trigger}"
    );
    assert!(
        !trigger.contains("closeSheet"),
        "save-and-continue must NOT close the sheet: {trigger}"
    );
    // The sheet fragment is re-rendered in the body (not an empty 200).
    assert!(
        !resp_body.trim().is_empty(),
        "save-and-continue re-renders the sheet body"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-admin --test phase2_sheet test_save_and_continue_refreshes_table_but_keeps_sheet_open`
Expected: FAIL — the current `_save_continue` branch returns `edit_sheet_handler(...)` with no `refreshTable` header.

- [ ] **Step 3: Add the `refreshTable` trigger to the save-and-continue branch**

In `plugins/umbral-admin/src/handlers/crud.rs`, replace the `_save_continue` branch (lines 694-695):

```rust
                if form.contains_key("_save_continue") {
                    return edit_sheet_handler(State(state), headers, Path((table, id))).await;
                }
```

with a version that attaches a `refreshTable` (+ toast) HX-Trigger to the re-rendered sheet, keeping it open:

```rust
                if form.contains_key("_save_continue") {
                    // Re-render the sheet (stays open) AND refresh the
                    // underlying changelist so the saved change shows in the
                    // list behind the sheet. No `closeSheet` — symmetric with
                    // the plain-save path but without dismissing the panel.
                    // The admin.js `refreshTable` listener re-fetches /rows
                    // using window.location.search, preserving the active
                    // search/sort/page/filter kwargs.
                    let mut resp =
                        edit_sheet_handler(State(state), headers, Path((table, id.clone()))).await;
                    let trigger = serde_json::json!({
                        "refreshTable": {},
                        "showToast": {
                            "message": format!("{} saved", model.name),
                            "level": "success"
                        },
                    });
                    if let Ok(hv) = trigger.to_string().parse() {
                        resp.headers_mut().insert("HX-Trigger", hv);
                    }
                    return resp;
                }
```

> `id` was previously moved into `Path((table, id))`; this version clones it (`id.clone()`) because `model.name` is still needed for the toast. Confirm `id` is a `String` in scope (it is — it comes from the `Path((table, id))` extractor on the handler). If `edit_sheet_handler`'s return type isn't already `axum::response::Response`, wrap with `.into_response()` before mutating headers.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p umbral-admin --test phase2_sheet test_save_and_continue_refreshes_table_but_keeps_sheet_open`
Expected: PASS.

- [ ] **Step 5: Run the full sheet suite + crud-adjacent tests**

Run: `cargo test -p umbral-admin --test phase2_sheet`
Expected: PASS (existing create/plain-save trigger tests still hold).

- [ ] **Step 6: Commit**

```bash
git add plugins/umbral-admin/src/handlers/crud.rs plugins/umbral-admin/tests/phase2_sheet.rs
git commit -m "fix(admin): refresh changelist on save-and-continue

Save-and-continue re-rendered the sheet but never fired refreshTable, so
the list behind it went stale. Attach an HX-Trigger refreshTable (+ toast)
to the re-render, keeping the sheet open. The existing listener preserves
the active search/sort/page/filter kwargs.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 7: Production CSS build + full verification

Build the compiled CSS the way production does, run the whole workspace gate, and walk the manual verification checklist from the spec.

**Files:** none modified (build + verify only).

- [ ] **Step 1: Build the production Tailwind CSS**

The compiled CSS is what production serves (the CDN is dev-only). Build it so `text-label-sm` etc. exist in `src/assets/admin.css`:

```bash
cd plugins/umbral-admin/css && npm install && npm run build && cd -
```

Expected: `npx tailwindcss ... --minify` writes `plugins/umbral-admin/src/assets/admin.css` with no error. If `npm`/`npx` is unavailable, note it — `build.rs` falls back to the CDN in dev, but production parity REQUIRES this build to succeed; do not mark the task complete without it.

- [ ] **Step 2: Confirm the compiled CSS contains the previously-missing classes**

Run: `grep -c "label-sm" plugins/umbral-admin/src/assets/admin.css`
Expected: ≥ 1 (the `text-label-sm` / `font-label-sm` rules now compile — the production fix for huge sidebar labels + table headers).

- [ ] **Step 3: Full workspace gate**

Run, from the repo root:

```bash
cargo fmt && cargo clippy --all-targets && cargo build && cargo test
```

Expected: all clean. Fix anything that fails before proceeding.

- [ ] **Step 4: Refresh the GitNexus index (stale since this branch's edits)**

Run: `npx gitnexus analyze`
Expected: completes; index no longer stale.

- [ ] **Step 5: Manual verification checklist (run the example app the user already has up — do NOT restart their dev server)**

Verify in the browser against the running example admin:
- Light mode: background is pure white; dark mode: near-black neutral; no warm/brown tint anywhere; primary button is black-on-white (light) / white-on-black (dark).
- Sidebar plugin-group labels and table `<th>` headers render slender (11px uppercase), not 16px.
- Cards (dashboard stat strip, model cards, widgets, list/detail/form panels) share one border + radius + soft-shadow recipe and read cleanly.
- A table with > 7 pages keeps the numbered `1 … window … 50` footer after sort, search, and page-change (HTMX swaps), not the compact form.
- Open create/edit sheet at ~375px width: no horizontal overflow; the panel is a full-width drawer; fields wrap.
- Create a row, plain-save an edit, and save-and-continue an edit: in all three the changelist updates in place preserving the active search/sort/page/filters; save-and-continue leaves the sheet open.

- [ ] **Step 6: Update the changelog / version notes if the repo tracks them**

If `planning/features.md` or a changelog tracks user-visible changes, add a one-line entry: "Admin: neutral pro theme, unified cards, consistent long-form pagination, responsive sheets." (Skip if no such tracker is in scope — do not invent one.)

---

## Self-Review

**Spec coverage:**
- Neutral pure-white/near-black palette → Task 2. ✓
- Single Tailwind config / huge sidebar labels → Task 1. ✓
- Huge table headers → Task 1 (same root cause: `<th>` uses `font-label-sm text-label-sm`). ✓
- Standard card system → Task 3. ✓
- Long-form pagination maintained after swaps → Task 4. ✓
- Responsive sheet overflow → Task 5. ✓
- Refresh on create/save/save-and-continue → Task 6 (create + plain-save already work; save-and-continue was the gap). ✓
- Professional, not Django-admin → emergent from Tasks 1–3 + Task 7 manual polish pass. ✓
- Status colors success/warning → Task 1 (theme.json color keys) + Task 2 (token values). ✓

**Placeholder scan:** No "TBD"/"handle edge cases"/"similar to Task N" — code blocks are concrete. The two "adapt helper names to this test file" notes are explicit (the helper set is known to exist from the read of `phase2_sheet.rs` / `phase2_datatable.rs`); they guard against helper-name drift, not missing content.

**Type consistency:** `pagination_footer(admin_base, table, columns, search_val, filter_qs, sort_col, sort_order, pagination)` — same 8-arg signature defined in Task 4 Step 3 and called in Steps 5 & 6. `admin_theme_json` global (Task 1 Step 5) matches `{{ admin_theme_json }}` usage (Task 1 Step 6). `boxShadow.card` (Task 1) → `shadow-card` utility (Task 3). `--success`/`--warning` keys in theme.json colors (Task 1) match the token values (Task 2). `edit_sheet_handler(State, headers, Path((table, id)))` signature (Task 6) matches `sheet.rs:92`.
