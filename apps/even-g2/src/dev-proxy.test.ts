import { createServer as createHttpServer } from 'node:http'
import type { AddressInfo } from 'node:net'
import { createServer } from 'vite'
import { expect, it } from 'vitest'
import { readAuthGuard, requireReadAuth } from '../dev-proxy'

it('fails closed at startup and on requests after an unauthenticated upstream restart', async () => {
  const fixtureAuthority = 'fixture authority'
  const validHeaders = new Headers()
  validHeaders.set('x-kranz-token', fixtureAuthority)
  let requireAuth = true
  let leakedReads = 0
  const upstream = createHttpServer((request, response) => {
    if (requireAuth && request.headers['x-kranz-token'] !== fixtureAuthority) {
      response.writeHead(401).end()
    } else {
      if (request.url === '/api/missions') leakedReads++
      response.writeHead(200, { 'content-type': 'application/json' }).end('[]')
    }
  })
  await new Promise<void>((resolve) => upstream.listen(0, '127.0.0.1', resolve))
  const target = `http://127.0.0.1:${(upstream.address() as AddressInfo).port}`
  const vite = await createServer({
    configFile: false,
    plugins: [readAuthGuard(target)],
    server: { host: '127.0.0.1', port: 0, proxy: { '/api': { target, changeOrigin: true } } },
  })
  try {
    await requireReadAuth(target)
    await vite.listen()
    const url = `http://127.0.0.1:${(vite.httpServer!.address() as AddressInfo).port}/api/missions`
    expect((await fetch(url)).status).toBe(401)
    expect((await fetch(url, { headers: { 'x-kranz-token': 'wrong' } })).status).toBe(401)
    expect(leakedReads).toBe(0)
    expect((await fetch(url, { headers: validHeaders })).status).toBe(200)
    requireAuth = false
    await expect(requireReadAuth(target)).rejects.toThrow('must require read authentication')
    expect((await fetch(url, { headers: { 'x-kranz-token': 'wrong' } })).status).toBe(503)
    expect(leakedReads).toBe(1)
  } finally {
    await vite.close()
    upstream.closeAllConnections()
    await new Promise<void>((resolve, reject) => upstream.close((error) => error ? reject(error) : resolve()))
  }
})
