// Read-auth mode (docs/deploy.md "security notes"): GETs — not just POSTs —
// must park on the token gate on a 401 and retry with the fresh token. This
// file exercises the REAL token module (unlike api.test.ts, which mocks it)
// so tokenGateSnapshot()/provideToken() reflect genuine gate state.

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { getJson, postJson } from './api';
import { cancelTokenPrompt, clearToken, provideToken, tokenGateSnapshot } from './token';

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });
}

function unauthorizedResponse(): Response {
  return new Response(JSON.stringify({ error: 'token required' }), {
    status: 401,
    headers: { 'content-type': 'application/json' },
  });
}

beforeEach(() => {
  vi.stubGlobal('fetch', vi.fn());
  window.location.hash = '';
  cancelTokenPrompt();
  clearToken();
});

afterEach(() => {
  window.location.hash = '';
  vi.unstubAllGlobals();
  cancelTokenPrompt();
  clearToken();
});

describe('read-auth: GET opens the token gate on 401 and retries with the header', () => {
  it('drives tokenGateSnapshot().needed and resolves with the retried body', async () => {
    vi.mocked(fetch)
      .mockResolvedValueOnce(unauthorizedResponse())
      .mockResolvedValueOnce(jsonResponse({ repos: ['alpha'] }));

    expect(tokenGateSnapshot().needed).toBe(false);

    const pending = getJson('/api/repos');

    // Let the first fetch (401) resolve and awaitToken() park the gate open.
    await vi.waitFor(() => {
      expect(tokenGateSnapshot().needed).toBe(true);
    });

    provideToken('read-token');

    await expect(pending).resolves.toEqual({ repos: ['alpha'] });
    expect(tokenGateSnapshot().needed).toBe(false);

    expect(fetch).toHaveBeenCalledTimes(2);
    const secondInit = vi.mocked(fetch).mock.calls[1][1];
    const headers = secondInit?.headers as Record<string, string> | undefined;
    expect(headers?.['x-kranz-token']).toBe('read-token');
  });
});

describe('read-auth: POST never leaks the token into the URL', () => {
  it('sends the token only via the header, not as a query parameter', async () => {
    provideToken('post-token');
    vi.mocked(fetch).mockResolvedValueOnce(jsonResponse({ ok: true }));

    await expect(postJson('/api/missions', { goal: 'x' })).resolves.toEqual({ ok: true });

    const [url, init] = vi.mocked(fetch).mock.calls[0];
    expect(String(url)).not.toContain('token=');
    const headers = init?.headers as Record<string, string> | undefined;
    expect(headers?.['x-kranz-token']).toBe('post-token');
  });
});
