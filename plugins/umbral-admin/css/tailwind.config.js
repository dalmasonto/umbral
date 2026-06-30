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
