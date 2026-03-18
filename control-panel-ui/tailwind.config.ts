import type { Config } from 'tailwindcss'

const config: Config = {
  content: [
    './src/pages/**/*.{js,ts,jsx,tsx,mdx}',
    './src/components/**/*.{js,ts,jsx,tsx,mdx}',
    './src/app/**/*.{js,ts,jsx,tsx,mdx}',
  ],
  theme: {
    extend: {
      colors: {
        // Dark terminal palette
        bg: {
          base:    '#0a0b0d',
          surface: '#111318',
          raised:  '#181c23',
          overlay: '#1e2330',
        },
        border: {
          DEFAULT: '#1e2330',
          muted:   '#151920',
          bright:  '#2a3040',
        },
        text: {
          primary:   '#e2e8f0',
          secondary: '#8892a4',
          muted:     '#4a5568',
        },
        accent: {
          green:  '#10b981',
          red:    '#ef4444',
          blue:   '#3b82f6',
          yellow: '#f59e0b',
          purple: '#8b5cf6',
          cyan:   '#06b6d4',
        },
      },
      fontFamily: {
        mono: ['JetBrains Mono', 'Fira Code', 'monospace'],
      },
      animation: {
        'pulse-slow': 'pulse 3s cubic-bezier(0.4, 0, 0.6, 1) infinite',
        'blink':      'blink 1s step-start infinite',
      },
      keyframes: {
        blink: {
          '0%, 100%': { opacity: '1' },
          '50%':      { opacity: '0' },
        },
      },
    },
  },
  plugins: [],
}

export default config
