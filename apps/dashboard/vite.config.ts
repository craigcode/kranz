import { defineConfig } from 'vitest/config'
import react from '@vitejs/plugin-react'

// https://vite.dev/config/
export default defineConfig({
  plugins: [react()],
  server: {
    // Dev-only: point the API at a locally running `kranz serve` so
    // `npm run dev` shows real missions (ws covers the live event feed).
    proxy: {
      '/api': { target: 'http://127.0.0.1:4560', ws: true },
    },
  },
  test: {
    environment: 'jsdom',
    globals: false,
  },
})
