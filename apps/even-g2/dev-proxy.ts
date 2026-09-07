import type { Plugin } from 'vite'

export async function requireReadAuth(target: string): Promise<void> {
  let status: number
  try {
    status = (await fetch(new URL('/api/repos', target), {
      signal: AbortSignal.timeout(2_000), redirect: 'error',
    })).status
  } catch {
    throw new Error('Kranz is unreachable; start kranz serve --read-auth first')
  }
  if (status !== 401) {
    throw new Error('Kranz must require read authentication; restart with --read-auth')
  }
}

export function readAuthGuard(target: string): Plugin {
  return {
    name: 'kranz-read-auth-guard',
    configureServer(server) {
      server.middlewares.use('/api', async (request, response, next) => {
        const authority = request.headers['x-kranz-token']
        if (typeof authority !== 'string' || !authority.trim()) {
          response.writeHead(401, { 'content-type': 'application/json' })
          response.end(JSON.stringify({ error: 'Missing Kranz token' }))
          return
        }
        try {
          // Recheck after upstream restarts; startup alone is not a durable
          // read-auth guarantee for a LAN-facing development proxy.
          await requireReadAuth(target)
          next()
        } catch (error) {
          response.writeHead(503, { 'content-type': 'application/json' })
          response.end(JSON.stringify({ error: (error as Error).message }))
        }
      })
    },
  }
}
