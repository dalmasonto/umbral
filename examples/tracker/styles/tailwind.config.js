/** @type {import('tailwindcss').Config} */
module.exports = {
  // A dark theme is a `.dark { ... }` block in input.css overriding the same
  // variables — the templates never change.
  darkMode: 'class',
  content: [
    '../templates/**/*.html',
    '../plugins/**/templates/**/*.html',
    '../src/**/*.rs',
    '../plugins/**/src/**/*.rs',
  ],
  theme: {
    extend: {
      colors: {
        paper:       'var(--paper)',
        surface:     'var(--surface)',
        'surface-2': 'var(--surface-2)',
        'surface-3': 'var(--surface-3)',
        ink:         'var(--ink)',
        'ink-2':     'var(--ink-2)',
        body:        'var(--body)',
        muted:       'var(--muted)',
        faint:       'var(--faint)',
        hairline:    'var(--hairline)',
        'hairline-2':'var(--hairline-2)',
        accent:        'var(--accent)',
        'accent-2':    'var(--accent-2)',
        'accent-soft': 'var(--accent-soft)',
        'accent-line': 'var(--accent-line)',
        'accent-ghost':'var(--accent-ghost)',
        ok:          'var(--ok)',
        'ok-soft':   'var(--ok-soft)',
        warn:        'var(--warn)',
        'warn-soft': 'var(--warn-soft)',
        shade:         'var(--shade)',
        'shade-2':     'var(--shade-2)',
        'shade-ink':   'var(--shade-ink)',
        'shade-muted': 'var(--shade-muted)',
        'shade-faint': 'var(--shade-faint)',
      },
      borderColor: { DEFAULT: 'var(--hairline)' },
      fontFamily: {
        sans: ['Inter', 'ui-sans-serif', 'system-ui', '-apple-system', 'sans-serif'],
        mono: ['"JetBrains Mono"', 'ui-monospace', 'SFMono-Regular', 'Consolas', 'monospace'],
      },
    },
  },
  plugins: [],
};
