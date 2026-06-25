---
name: Umbral Admin
colors:
  surface: '#10131a'
  surface-dim: '#10131a'
  surface-bright: '#363941'
  surface-container-lowest: '#0b0e15'
  surface-container-low: '#191c23'
  surface-container: '#1d2027'
  surface-container-high: '#272a31'
  surface-container-highest: '#32353c'
  on-surface: '#e0e2ec'
  on-surface-variant: '#c7c5d5'
  inverse-surface: '#e0e2ec'
  inverse-on-surface: '#2d3038'
  outline: '#918f9e'
  outline-variant: '#464553'
  surface-tint: '#c2c1ff'
  primary: '#c2c1ff'
  on-primary: '#1f1792'
  primary-container: '#8484f8'
  on-primary-container: '#16098d'
  inverse-primary: '#504fc1'
  secondary: '#bec7d7'
  on-secondary: '#28313d'
  secondary-container: '#3e4755'
  on-secondary-container: '#adb6c5'
  tertiary: '#ffb3ad'
  on-tertiary: '#68000a'
  tertiary-container: '#ff5451'
  on-tertiary-container: '#5c0008'
  error: '#ffb4ab'
  on-error: '#690005'
  error-container: '#93000a'
  on-error-container: '#ffdad6'
  primary-fixed: '#e2dfff'
  primary-fixed-dim: '#c2c1ff'
  on-primary-fixed: '#0b006b'
  on-primary-fixed-variant: '#3835a8'
  secondary-fixed: '#dae3f4'
  secondary-fixed-dim: '#bec7d7'
  on-secondary-fixed: '#131c28'
  on-secondary-fixed-variant: '#3e4755'
  tertiary-fixed: '#ffdad7'
  tertiary-fixed-dim: '#ffb3ad'
  on-tertiary-fixed: '#410004'
  on-tertiary-fixed-variant: '#930013'
  background: '#10131a'
  on-background: '#e0e2ec'
  surface-variant: '#32353c'
typography:
  display:
    fontFamily: Inter
    fontSize: 30px
    fontWeight: '700'
    lineHeight: 38px
    letterSpacing: -0.02em
  h1:
    fontFamily: Inter
    fontSize: 24px
    fontWeight: '600'
    lineHeight: 32px
    letterSpacing: -0.015em
  h2:
    fontFamily: Inter
    fontSize: 20px
    fontWeight: '600'
    lineHeight: 28px
    letterSpacing: -0.01em
  h3:
    fontFamily: Inter
    fontSize: 16px
    fontWeight: '600'
    lineHeight: 24px
  body-lg:
    fontFamily: Inter
    fontSize: 16px
    fontWeight: '400'
    lineHeight: 24px
  body-md:
    fontFamily: Inter
    fontSize: 14px
    fontWeight: '400'
    lineHeight: 20px
  body-sm:
    fontFamily: Inter
    fontSize: 13px
    fontWeight: '400'
    lineHeight: 18px
  label-md:
    fontFamily: Inter
    fontSize: 12px
    fontWeight: '500'
    lineHeight: 16px
    letterSpacing: 0.01em
  label-sm:
    fontFamily: Inter
    fontSize: 11px
    fontWeight: '600'
    lineHeight: 14px
    letterSpacing: 0.03em
  data-mono:
    fontFamily: Inter
    fontSize: 14px
    fontWeight: '400'
    lineHeight: 20px
rounded:
  sm: 0.125rem
  DEFAULT: 0.25rem
  md: 0.375rem
  lg: 0.5rem
  xl: 0.75rem
  full: 9999px
spacing:
  base: 4px
  xs: 4px
  sm: 8px
  md: 16px
  lg: 24px
  xl: 32px
  sidebar-width: 260px
  sidebar-collapsed: 68px
  topbar-height: 64px
  gutter: 20px
---

## Brand & Style

The design system is engineered for high-performance administrative environments where speed of thought and clarity of data are paramount. The brand personality is **technical, precise, and unobtrusive**, acting as a sophisticated "dark mode first" ecosystem that recedes to let user data take center stage.

The style leverages **Modern Minimalist** principles with a heavy focus on **Systematic Density**. By utilizing minimal chrome and structural borders rather than heavy shadows, the interface maintains a lightweight feel even when displaying complex datasets. It avoids decorative flourishes in favor of utility, using the primary accent color as a surgical tool for focus and action.

## Colors

This design system utilizes a semantic color token strategy to ensure seamless switching between Light and Dark modes. 

- **Primary Accent:** Used for active states, primary buttons, and critical focus indicators. The shift from `#5B5BD6` (Light) to `#7C7CF0` (Dark) ensures AA contrast accessibility on their respective surfaces.
- **Canvas vs. Surface:** The `canvas` token is reserved for the background of the application (the lowest layer), while `surface` is used for cards, sidebars, and modals to create subtle depth.
- **Status:** Danger (`#EF4444`) is used sparingly for destructive actions and error states. Success and Warning states should be derived from the secondary palette or neutral scales to maintain the minimal aesthetic.

## Typography

The design system relies exclusively on **Inter**, a humanist sans-serif optimized for screen legibility. 

A critical requirement for this admin interface is the use of **Tabular Numerals** (`tnum`) for all data-heavy contexts, such as DataTables and KPI Cards. This ensures that columns of numbers align vertically, facilitating rapid scanning and comparison.

- **Scale:** The scale is compact to support high information density. 
- **Hierarchy:** Use `label-sm` (uppercase) for table headers and section overlines.
- **Mobile:** For screens below 768px, `display` scales down to 24px and `h1` scales to 20px to maintain balance.

## Layout & Spacing

The layout follows a **Hybrid Fluid** model. While the main content area expands to fill the viewport, it is anchored by a fixed-width collapsible sidebar.

- **Grid:** A 12-column grid is used for dashboard layouts, typically reflowing to a single column on mobile.
- **Density:** We utilize a 4px baseline shift. Most internal component padding is set to `sm` (8px) or `md` (16px) to maintain a "tight but breathable" feel.
- **Safe Areas:** Standard page margins are `lg` (24px) on desktop, reducing to `md` (16px) on mobile devices.
- **Breakpoints:**
  - Mobile: < 640px
  - Tablet: 640px - 1024px
  - Desktop: > 1024px

## Elevation & Depth

This design system minimizes the use of heavy shadows to keep the interface fast and clean. Depth is primarily communicated through **Tonal Layering** and **Low-Contrast Outlines**.

- **Level 0 (Canvas):** The base background layer.
- **Level 1 (Surface):** Cards and Sidebars. These use a 1px solid border (`border` token).
- **Level 2 (Floating):** Popovers and Command Palettes. These use a slightly more pronounced border and a subtle, large-radius ambient shadow (10% opacity) to distinguish them from the surface beneath.
- **Level 3 (Overlays):** Sheets and Dialogs. These use a backdrop blur (8px) on the `canvas` to focus user attention.

## Shapes

The shape language is varied to create a clear visual distinction between different UI roles.

- **Controls:** Inputs and Buttons use a sharp `6px` radius to feel precise and tool-like.
- **Containers:** Dashboard cards use an `8px` radius to soften the layout slightly.
- **Indicators:** Chips and Tags use a `10px` radius, bordering on pill-shaped but maintaining a modern geometric profile.
- **Layout Extensions:** Large Sheets and Dialogs use a generous `14px` radius on leading corners to signify they are temporary "wrappers" over the main UI.

## Components

### Sidebars & Topbars
- **Sidebar:** Collapsible state should only show Lucide icons. Expanded state includes `body-sm` text. Active links use a subtle background tint of the `accent` color at 10% opacity and a 2px vertical "pill" indicator on the leading edge.
- **Topbar:** Contains a persistent Breadcrumb and a "Search" trigger that opens the Command Palette (`Cmd+K`).

### DataTables
- **Header:** `label-sm` font style, muted text. Sticky on scroll.
- **Rows:** 48px height. Subtle hover state change to `canvas` color.
- **Cells:** Use `data-mono` for all numeric values.

### KPI Cards
- **Structure:** Title (`label-md`), Value (`h1` with tabular numbers), and Trend (Chip with Lucide arrow icon).
- **Style:** Bordered, no shadow, `surface` background.

### Right-side Sheets
- **Animation:** Slide-in from right. 
- **Width:** Fixed 400px on desktop; 100% width on mobile.
- **Close:** Esc key or backdrop click.

### Buttons & Inputs
- **Primary:** Solid `accent` color with white (L) or dark (D) text.
- **Ghost/Tertiary:** No border, primary text, background appears only on hover.
- **Inputs:** 1px border. On focus, the border changes to `accent` with a 2px outer glow of the same color at 20% opacity.