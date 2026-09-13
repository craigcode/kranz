import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';

vi.mock('./token', () => ({
  resolveToken: vi.fn(() => 'test-token'),
  awaitToken: vi.fn(),
}));

import { api, getJson, postJson, ApiError, isNotHosted, isStalePlan, isTokenRequired } from './api';
import { awaitToken, resolveToken } from './token';
import wireFixtures from './fixtures/api-errors.json?raw';

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

describe('stable HTTP error codes', () => {
  const fixtures: Array<{ name: string; status: number; body: { error: string; code?: string } }> =
    JSON.parse(wireFixtures);

  beforeEach(() => {
    vi.stubGlobal('fetch', vi.fn());
    window.location.hash = '';
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.clearAllMocks();
  });

  // Rust's http_api_error_codes_match_dashboard_wire_fixtures exercises the
  // host refusal paths and pins these exact HTTP bodies through IntoResponse.
  it.each(fixtures)('consumes the server’s $name wire response', async (fixture) => {
    vi.mocked(fetch).mockResolvedValueOnce(new Response(JSON.stringify(fixture.body), {
      status: fixture.status, headers: { 'content-type': 'application/json' },
    }));
    const error = await postJson('/api/missions/A/start', {}).catch(err => err);
    expect(error).toBeInstanceOf(ApiError);
    expect(error).toMatchObject({ status: fixture.status, message: fixture.body.error, code: fixture.body.code });
    expect(isNotHosted(error)).toBe(fixture.name === 'mission_not_hosted' || fixture.name === 'legacy');
    expect(isStalePlan(error)).toBe(fixture.name === 'stale_plan' || fixture.name === 'legacy');
  });

  it('uses the code even when the human wording changes', () => {
    expect(isNotHosted(new ApiError(409, 'Resume elsewhere.', 'mission_not_hosted'))).toBe(true);
    expect(isStalePlan(new ApiError(409, 'Review again.', 'stale_plan'))).toBe(true);
  });

  it.each(['turn_in_flight', 'repository_busy', 'future_code', ''])('never falls back to prose for code %j', (code) => {
    const error = new ApiError(409, 'mission is not hosted; refresh the plan preview', code);
    expect(isNotHosted(error)).toBe(false);
    expect(isStalePlan(error)).toBe(false);
  });

  it('preserves unknown codes received from a newer server', async () => {
    vi.mocked(fetch).mockResolvedValueOnce(new Response(JSON.stringify({
      error: 'mission is not hosted', code: 'future_code',
    }), { status: 409 }));
    const error = await getJson('/api/missions').catch(err => err);
    expect(error).toMatchObject({ code: 'future_code' });
    expect(isNotHosted(error)).toBe(false);
  });

  it('does not interpret a malformed code as an absent legacy code', async () => {
    vi.mocked(fetch).mockResolvedValueOnce(new Response(JSON.stringify({
      error: 'mission is not hosted', code: null,
    }), { status: 409 }));
    const error = await getJson('/api/missions').catch(err => err);
    expect(isNotHosted(error)).toBe(false);
    expect(isStalePlan(error)).toBe(false);
  });
});

describe('getJson / postJson non-JSON guard', () => {
  beforeEach(() => {
    vi.stubGlobal('fetch', vi.fn());
    window.location.hash = '';
  });

  afterEach(() => {
    window.location.hash = '';
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

  it('binds approve-pending to the reviewed identity while leaving start separate', async () => {
    vi.mocked(fetch).mockResolvedValueOnce(jsonResponse({ branch: 'approved', started: false }));
    await api.approvePending('m-reviewed', 'the-reviewed-plan-sha256');
    const [url, init] = vi.mocked(fetch).mock.calls[0];
    expect(url).toBe('/api/missions/m-reviewed/approve-pending');
    expect(JSON.parse(init!.body as string)).toEqual({
      planIdentity: 'the-reviewed-plan-sha256', start: false,
    });
  });
});

describe('401 token gate', () => {
  beforeEach(() => {
    vi.stubGlobal('fetch', vi.fn());
    window.location.hash = '';
    vi.mocked(resolveToken).mockReturnValue('stale-token');
    vi.mocked(awaitToken).mockReset().mockImplementation(async () => {
      // Real awaitToken clears the stale token then waits for a paste.
      vi.mocked(resolveToken).mockReturnValue('fresh-token');
    });
  });

  afterEach(() => {
    window.location.hash = '';
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

  it('keeps the original repository scope across a token-gate retry', async () => {
    window.location.hash = '#/r/alpha';
    vi.mocked(awaitToken).mockImplementationOnce(async () => {
      window.location.hash = '#/r/beta';
      vi.mocked(resolveToken).mockReturnValue('fresh-token');
    });
    vi.mocked(fetch)
      .mockResolvedValueOnce(unauthorizedResponse())
      .mockResolvedValueOnce(jsonResponse({ ok: true }));

    await postJson('/api/missions', { goal: 'stay scoped' });

    expect(vi.mocked(fetch).mock.calls[0][0]).toBe('/api/repos/alpha/missions');
    expect(vi.mocked(fetch).mock.calls[1][0]).toBe('/api/repos/alpha/missions');
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

describe('repository scope', () => {
  beforeEach(() => {
    vi.stubGlobal('fetch', vi.fn(() => Promise.resolve(jsonResponse([]))));
    window.location.hash = '#/r/alpha';
  });

  afterEach(() => {
    window.location.hash = '';
    vi.unstubAllGlobals();
  });

  it('prefixes repo operations but leaves the catalog endpoint unscoped', async () => {
    await getJson('/api/missions');
    await api.repos();

    expect(vi.mocked(fetch).mock.calls[0][0]).toBe('/api/repos/alpha/missions');
    expect(vi.mocked(fetch).mock.calls[1][0]).toBe('/api/repos');
  });
});
