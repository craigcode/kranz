import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';

vi.mock('./token', () => ({
  resolveToken: vi.fn(() => 'test-token'),
  awaitToken: vi.fn(),
}));

import { api, getJson, postJson, ApiError, isTokenRequired } from './api';
import { awaitToken, resolveToken } from './token';

function htmlResponse(): Response {
  return new Response('<!doctype html><html><body>app</body></html>', {
    status: 200,
    headers: { 'content-type': 'text/html' },
  });
}

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });
}

function unauthorizedResponse(): Response {
  return new Response(JSON.stringify({ error: 'invalid token' }), {
    status: 401,
    headers: { 'content-type': 'application/json' },
  });
}

describe('getJson / postJson non-JSON guard', () => {
  beforeEach(() => {
    vi.stubGlobal('fetch', vi.fn());
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.clearAllMocks();
  });

  it('getJson rejects with a friendly ApiError on an HTML 200 response', async () => {
    vi.mocked(fetch).mockImplementation(() => Promise.resolve(htmlResponse()));

    let rejection: unknown;
    try {
      await getJson('/api/missions');
    } catch (err) {
      rejection = err;
    }
    expect(rejection).toBeInstanceOf(ApiError);
    expect((rejection as ApiError).message).toBe('endpoint unavailable — server restart needed?');
    expect(rejection).not.toBeInstanceOf(SyntaxError);
    expect((rejection as Error).message).not.toContain('Unexpected token');
  });

  it('getJson still resolves to the parsed value for a valid application/json response', async () => {
    vi.mocked(fetch).mockImplementation(() => Promise.resolve(jsonResponse({ ok: true })));

    await expect(getJson('/api/health')).resolves.toEqual({ ok: true });
  });

  it('postJson rejects with the same friendly ApiError on an HTML 200 response', async () => {
    vi.mocked(fetch).mockImplementation(() => Promise.resolve(htmlResponse()));

    await expect(postJson('/api/missions', { goal: 'x' })).rejects.toMatchObject({
      name: 'ApiError',
      message: 'endpoint unavailable — server restart needed?',
    });
  });

  it('postJson still resolves to the parsed value for a valid application/json response', async () => {
    vi.mocked(fetch).mockImplementation(() => Promise.resolve(jsonResponse({ id: 'm-1' })));

    await expect(postJson('/api/missions', { goal: 'x' })).resolves.toEqual({ id: 'm-1' });
  });
});

describe('401 token gate', () => {
  beforeEach(() => {
    vi.stubGlobal('fetch', vi.fn());
    vi.mocked(resolveToken).mockReturnValue('stale-token');
    vi.mocked(awaitToken).mockReset().mockImplementation(async () => {
      // Real awaitToken clears the stale token then waits for a paste.
      vi.mocked(resolveToken).mockReturnValue('fresh-token');
    });
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.clearAllMocks();
  });

  /** The x-kranz-token header sent on the nth fetch call (0-based). */
  function sentToken(call: number): string | undefined {
    const init = vi.mocked(fetch).mock.calls[call][1];
    return (init?.headers as Record<string, string> | undefined)?.['x-kranz-token'];
  }

  it('postJson awaits a fresh token after 401 then retries with the fresh header', async () => {
    vi.mocked(fetch)
      .mockResolvedValueOnce(unauthorizedResponse())
      .mockResolvedValueOnce(jsonResponse({ ok: true }));

    await expect(postJson('/api/missions', { goal: 'x' })).resolves.toEqual({ ok: true });

    expect(awaitToken).toHaveBeenCalledOnce();
    expect(fetch).toHaveBeenCalledTimes(2);
    expect(sentToken(0)).toBe('stale-token');
    // The retry must re-resolve the token — not replay the rejected one.
    expect(sentToken(1)).toBe('fresh-token');
  });

  it('getJson awaits a fresh token after 401 then retries with the fresh header', async () => {
    vi.mocked(fetch)
      .mockResolvedValueOnce(unauthorizedResponse())
      .mockResolvedValueOnce(jsonResponse({ ok: true }));

    await expect(getJson('/api/missions')).resolves.toEqual({ ok: true });

    expect(awaitToken).toHaveBeenCalledOnce();
    expect(fetch).toHaveBeenCalledTimes(2);
    expect(sentToken(0)).toBe('stale-token');
    expect(sentToken(1)).toBe('fresh-token');
  });

  it('getJson with tokenGate:false rejects the 401 without parking on the gate', async () => {
    vi.mocked(fetch).mockResolvedValueOnce(unauthorizedResponse());

    let rejection: unknown;
    try {
      await getJson('/api/queue', { tokenGate: false });
    } catch (err) {
      rejection = err;
    }

    expect(awaitToken).not.toHaveBeenCalled();
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(rejection).toBeInstanceOf(ApiError);
    expect((rejection as ApiError).status).toBe(401);
    expect(isTokenRequired(rejection)).toBe(true);
  });

  it('isTokenRequired is false for non-401 failures', () => {
    expect(isTokenRequired(new ApiError(500, 'boom'))).toBe(false);
    expect(isTokenRequired(new Error('network down'))).toBe(false);
  });

  it('api.queue forwards its opts to getJson: tokenGate:false fails fast on 401', async () => {
    vi.mocked(fetch).mockResolvedValueOnce(unauthorizedResponse());

    let rejection: unknown;
    try {
      await api.queue({ tokenGate: false });
    } catch (err) {
      rejection = err;
    }

    // Pins the queue(opts) → getJson(path, opts) forwarding: if queue()
    // dropped its opts, the 401 would park on the token gate and retry
    // (awaitToken called, a second fetch) instead of failing fast.
    expect(awaitToken).not.toHaveBeenCalled();
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(isTokenRequired(rejection)).toBe(true);
  });
});
