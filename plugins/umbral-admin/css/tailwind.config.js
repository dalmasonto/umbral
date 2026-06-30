/** @type {import('tailwindcss').Config} */
// Single source of truth for the theme lives in ./theme.json — the same
// object is injected into the dev CDN config in wrapper.html via the
// `admin_theme_json` MiniJinja global. Keep all token edits in theme.json
// so the compiled build and the CDN can never drift (that drift is what
// made `text-label-sm` resolve to nothing in prod → huge sidebar labels).
//
// Note on the `divider` / `divider-soft` color tokens in theme.json:
// Tailwind v3 silently drops opacity modifiers on raw CSS-variable colors,
// so those tokens bake their alpha into the value itself. Use the named
// utilities (`border-divider`, `divide-divider-soft`) for soft dividers
// rather than an opacity modifier like `border-outline-variant/20`, which
// would be dropped. (This rationale used to live in the inline `extend`
// block that theme.json replaced; JSON can't carry comments, so it lives
// here.)
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
