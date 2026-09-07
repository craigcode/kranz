import { defineConfig, loadEnv } from 'vite'
import { readAuthGuard, requireReadAuth } from './dev-proxy.ts'

export default defineConfig(async ({ command, mode }) => {
  const env = loadEnv(mode, process.cwd(), '')
  const target = env.KRANZ_API_TARGET || 'http://127.0.0.1:4560'
  const demoOnly = mode === 'demo'
  if (command === 'serve' && mode !== 'test' && !demoOnly) {
    await requireReadAuth(target)
  }
  return {
    plugins: demoOnly ? [] : [readAuthGuard(target)],
    server: {
      host: true,
      port: 5174,
      strictPort: true,
      // The phone loads this Vite origin over the LAN. Proxying /api keeps
      // Kranz same-origin from the WebView's perspective and preserves the
      // server's strict CORS/Host policy.
      proxy: demoOnly
        ? undefined
        : {
            '/api': {
              target,
              changeOrigin: true,
            },
          },
    },
    build: { target: 'esnext' },
    test: { environment: 'node' },
  }
})
