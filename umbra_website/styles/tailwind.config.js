/** @type {import('tailwindcss').Config} */
module.exports = {
  content: [
    '../templates/**/*.html',
    '../plugins/**/templates/**/*.html',
    '../src/**/*.rs',
    '../plugins/**/src/**/*.rs',
  ],
  theme: {
    extend: {
      fontFamily: {
        sans: [
          'Aptos',
          'ui-sans-serif',
          'system-ui',
          '-apple-system',
          'BlinkMacSystemFont',
          '"Segoe UI"',
          'sans-serif',
        ],
        mono: [
          '"Cascadia Mono"',
          '"SFMono-Regular"',
          'Consolas',
          '"Liberation Mono"',
          'monospace',
        ],
      },
      boxShadow: {
        crisp: '0 1px 0 oklch(0.84 0.02 250), 0 18px 50px -36px oklch(0.24 0.05 260)',
      },
    },
  },
  plugins: [],
};
