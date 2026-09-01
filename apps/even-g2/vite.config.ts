import { defineConfig, loadEnv } from 'vite'

export default defineConfig(async ({ command, mode }) => {
  const env = loadEnv(mode, process.cwd(), '')
  const target = env.KRANZ_API_TARGET || 'http://127.0.0.1:4560'
  const demoOnly = mode === 'demo'
  if (command === 'serve' && mode !== 'test' && !demoOnly) {
    let status: number
    try {
      status = (
        await fetch(new URL('/api/repos', target), { signal: AbortSignal.timeout(2_000) })
      ).status
    } catch {
      throw new Error(`Kranz is not reachable at ${target}; start kranz serve --read-auth first`)
    }
    if (status !== 401) {
      throw new Error(
        `Kranz at ${target} does not require read authentication; restart it with --read-auth before exposing the G2 proxy`,
      )
    }
  }
  return {
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
