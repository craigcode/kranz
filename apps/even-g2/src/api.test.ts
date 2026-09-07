import { afterEach, describe, expect, it, vi } from 'vitest'
import { HttpKranzApi, setMutationToken } from './api'

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('HttpKranzApi', () => {
  it('keeps mutation authority in a header and never in the URL', async () => {
    const storage = new Map<string, string>()
    vi.stubGlobal('sessionStorage', {
      getItem: (key: string) => storage.get(key) ?? null,
      setItem: (key: string, value: string) => storage.set(key, value),
    })
    setMutationToken('secret token')
    const fetchMock = vi.fn<typeof fetch>().mockImplementation((input) => {
      const url = String(input)
      const body = url === '/api/repos'
        ? '[{"id":"kranz","displayName":"Kranz","pinned":true,"isDefault":false,"status":"healthy"}]'
        : '{"queued":true}'
      return Promise.resolve(
        new Response(body, {
          status: 200,
          headers: { 'content-type': 'application/json' },
        }),
      )
    })
    const api = new HttpKranzApi(fetchMock)

    await api.approveGrant('m/a', 'api.github.com:443')

    expect(fetchMock).toHaveBeenCalledTimes(2)
    const [url, init] = fetchMock.mock.calls[1] ?? []
    expect(url).toBe('/api/repos/kranz/missions/m%2Fa/grant/approve')
    expect(String(url)).not.toContain('secret')
    expect(new Headers(init?.headers).get('x-kranz-token')).toBe('secret token')
    expect(init?.body).toBe('{"command":"api.github.com:443"}')
  })

  it('skips an unavailable default repository and scopes reads to a healthy project', async () => {
    vi.stubGlobal('sessionStorage', { getItem: () => null, setItem: () => undefined })
    const fetchMock = vi.fn<typeof fetch>().mockImplementation((input) => {
      const url = String(input)
      const body = url === '/api/repos'
        ? JSON.stringify([
            { id: 'gone', displayName: 'Gone', pinned: false, isDefault: true, status: 'unavailable' },
            { id: 'kranz', displayName: 'Kranz', pinned: true, isDefault: false, status: 'healthy' },
          ])
        : '[]'
      return Promise.resolve(
        new Response(body, { status: 200, headers: { 'content-type': 'application/json' } }),
      )
    })

    await new HttpKranzApi(fetchMock).listMissions()

    expect(fetchMock.mock.calls.map(([url]) => url)).toEqual([
      '/api/repos',
      '/api/repos/kranz/missions',
    ])
  })

  it('requires an explicit repo when several healthy projects are ambiguous', async () => {
    vi.stubGlobal('sessionStorage', { getItem: () => null, setItem: () => undefined })
    const fetchMock = vi.fn<typeof fetch>().mockResolvedValue(
      new Response(
        JSON.stringify([
          { id: 'a', displayName: 'A', pinned: false, isDefault: false, status: 'healthy' },
          { id: 'b', displayName: 'B', pinned: false, isDefault: false, status: 'healthy' },
        ]),
        { status: 200, headers: { 'content-type': 'application/json' } },
      ),
    )

    await expect(new HttpKranzApi(fetchMock).listMissions()).rejects.toThrow(
      'Multiple healthy Kranz repositories',
    )
  })

  it('surfaces the server error body', async () => {
    vi.stubGlobal('sessionStorage', { getItem: () => null, setItem: () => undefined })
    const fetchMock = vi.fn<typeof fetch>().mockResolvedValue(
      new Response('{"error":"missing or invalid token"}', {
        status: 401,
        statusText: 'Unauthorized',
        headers: { 'content-type': 'application/json' },
      }),
    )

    await expect(new HttpKranzApi(fetchMock).listMissions()).rejects.toThrow(
      'missing or invalid token',
    )
  })
})
