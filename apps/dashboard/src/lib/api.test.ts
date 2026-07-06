import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';

vi.mock('./token', () => ({
  resolveToken: vi.fn(() => 'test-token'),
  awaitToken: vi.fn(),
}));

import { getJson, postJson, ApiError } from './api';

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
