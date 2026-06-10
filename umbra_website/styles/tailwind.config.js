/** @type {import('tailwindcss').Config} */
module.exports = {
  // Theme toggling is class-based: the light values live on :root in
  // input.css and a future `.dark` block overrides the same variables.
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
        // Surfaces
        paper:       'var(--paper)',
        surface:     'var(--surface)',
        'surface-2': 'var(--surface-2)',
        'surface-3': 'var(--surface-3)',
        // Text
        ink:    'var(--ink)',
        'ink-2': 'var(--ink-2)',
        body:   'var(--body)',
        muted:  'var(--muted)',
        faint:  'var(--faint)',
        ghost:  'var(--ghost)',
        // Lines
        hairline:     'var(--hairline)',
        'hairline-2': 'var(--hairline-2)',
        // Accent
        accent:         'var(--accent)',
        'accent-2':     'var(--accent-2)',
        'accent-soft':  'var(--accent-soft)',
        'accent-line':  'var(--accent-line)',
        'accent-ghost': 'var(--accent-ghost)',
        // Status
        ok:          'var(--ok)',
        'ok-soft':   'var(--ok-soft)',
        warn:        'var(--warn)',
        'warn-soft': 'var(--warn-soft)',
        tan:         'var(--tan)',
        'tan-soft':  'var(--tan-soft)',
        'tan-line':  'var(--tan-line)',
        // Dark panels
        shade:         'var(--shade)',
        'shade-2':     'var(--shade-2)',
        'shade-ink':   'var(--shade-ink)',
        'shade-muted': 'var(--shade-muted)',
        'shade-faint': 'var(--shade-faint)',
        // Terminal syntax
        'code-violet': 'var(--code-violet)',
        'code-green':  'var(--code-green)',
        'code-amber':  'var(--code-amber)',
        'code-blue':   'var(--code-blue)',
      },
      borderColor: {
        DEFAULT: 'var(--hairline)',
      },
      fontFamily: {
        sans: ['Inter', 'ui-sans-serif', 'system-ui', '-apple-system', 'sans-serif'],
        mono: ['"JetBrains Mono"', 'ui-monospace', '"SFMono-Regular"', 'Consolas', 'monospace'],
      },
      boxShadow: {
        // Hero terminal + directory panel shadows from the mock.
        terminal: '0 24px 60px -28px rgba(27, 23, 20, 0.45)',
        panel: '0 30px 60px -40px rgba(27, 23, 20, 0.25)',
      },
    },
  },
  plugins: [],
};
