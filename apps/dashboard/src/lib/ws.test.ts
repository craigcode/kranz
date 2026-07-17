import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { MissionSocket } from './ws';
import { cancelTokenPrompt, clearToken, provideToken, tokenGateListenerCount } from './token';

/** Minimal WebSocket stand-in: records instances, never connects. */
class FakeWebSocket {
  static instances: FakeWebSocket[] = [];

  url: string;
  onopen: (() => void) | null = null;
  onmessage: ((msg: MessageEvent) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;

  constructor(url: string) {
    this.url = url;
    FakeWebSocket.instances.push(this);
  }

  close(): void {}
}

/** Microtask-only Response stand-in (a real Response body read can involve
 *  non-microtask async, which stalls under fake timers). */
function fakeJsonResponse(status: number, body: unknown): Response {
  return {
    status,
    ok: status >= 200 && status < 300,
    statusText: '',
    headers: { get: () => 'application/json' },
    text: () => Promise.resolve(JSON.stringify(body)),
    json: () => Promise.resolve(body),
  } as unknown as Response;
}

function makeSocket(onStatus: (status: string) => void = () => {}): MissionSocket {
  return new MissionSocket({
    origin: 'http://127.0.0.1:4560',
    missionId: 'm-1',
    getSince: () => null,
    onFrame: () => {},
    onStatus,
  });
}

beforeEach(() => {
  vi.useFakeTimers();
  FakeWebSocket.instances = [];
  vi.stubGlobal('WebSocket', FakeWebSocket);
  // The existence probe's default: mission still there, keep reconnecting.
  vi.stubGlobal('fetch', vi.fn(() => Promise.resolve(fakeJsonResponse(200, {}))));
  cancelTokenPrompt();
  clearToken();
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
  cancelTokenPrompt();
  clearToken();
});

describe('MissionSocket token nudge', () => {
  it('uses the captured repository id for the websocket path', () => {
    const socket = new MissionSocket({
      origin: 'http://127.0.0.1:4560',
      repoId: 'alpha',
      missionId: 'm-1',
      getSince: () => null,
      onFrame: () => {},
      onStatus: () => {},
    });
    socket.connect();
    expect(FakeWebSocket.instances[0].url).toBe(
      'ws://127.0.0.1:4560/api/repos/alpha/missions/m-1/ws',
    );
    socket.close();
  });

  it('reconnects immediately on provideToken instead of waiting out the backoff', () => {
    const socket = makeSocket();
    socket.connect();
    expect(FakeWebSocket.instances).toHaveLength(1);

    // Off-loopback 401 on the upgrade surfaces as a close → backoff timer.
    FakeWebSocket.instances[0].onclose?.();
    expect(FakeWebSocket.instances).toHaveLength(1);

    // A pasted token must not wait for the timer: reconnect fires now, with
    // the fresh ?token= on the URL.
    provideToken('fresh-token');
    expect(FakeWebSocket.instances).toHaveLength(2);
    expect(FakeWebSocket.instances[1].url).toContain('token=fresh-token');

    socket.close();
  });

  it('resets the backoff so the retry after the nudge starts at the minimum', async () => {
    const socket = makeSocket();
    socket.connect(); // #1

    // Grow the backoff: 500ms → 1000ms → 2000ms pending.
    FakeWebSocket.instances[0].onclose?.();
    await vi.advanceTimersByTimeAsync(500); // #2
    FakeWebSocket.instances[1].onclose?.();
    await vi.advanceTimersByTimeAsync(1000); // #3
    FakeWebSocket.instances[2].onclose?.();

    provideToken('fresh-token'); // immediate reconnect → #4
    expect(FakeWebSocket.instances).toHaveLength(4);

    // The next failure retries at the reset minimum (500ms), not 4s.
    FakeWebSocket.instances[3].onclose?.();
    await vi.advanceTimersByTimeAsync(499);
    expect(FakeWebSocket.instances).toHaveLength(4);
    await vi.advanceTimersByTimeAsync(1);
    expect(FakeWebSocket.instances).toHaveLength(5);

    socket.close();
  });

  it('close() unsubscribes from the token gate: no listener leak, no revival', () => {
    const baseline = tokenGateListenerCount();
    const socket = makeSocket();
    expect(tokenGateListenerCount()).toBe(baseline + 1);

    socket.connect();
    FakeWebSocket.instances[0].onclose?.(); // reconnect pending

    socket.close();
    // The listener must be GONE, not merely inert behind the `closed` guard:
    // token.ts's listener set would otherwise grow by one dead socket per
    // mission switch. This is the assertion that fails if close() drops its
    // this.unsubscribeToken() call.
    expect(tokenGateListenerCount()).toBe(baseline);

    provideToken('late-token');
    expect(FakeWebSocket.instances).toHaveLength(1);
  });
});

describe('MissionSocket 404 probe (mission deleted out-of-band)', () => {
  it('stops reconnecting for good once the probe sees a 404, reporting gone', async () => {
    const statuses: string[] = [];
    const socket = makeSocket((s) => statuses.push(s));
    vi.mocked(fetch).mockImplementation(() =>
      Promise.resolve(fakeJsonResponse(404, { error: "unknown mission 'm-1'" })),
    );

    socket.connect(); // #1
    FakeWebSocket.instances[0].onclose?.(); // failure 1 — below the probe threshold
    expect(fetch).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(500); // retry #2

    FakeWebSocket.instances[1].onclose?.(); // failure 2 — probe fires
    await vi.advanceTimersByTimeAsync(60_000);

    expect(fetch).toHaveBeenCalledTimes(1);
    expect(vi.mocked(fetch).mock.calls[0][0]).toBe('/api/missions/m-1/state');
    expect(statuses[statuses.length - 1]).toBe('gone');
    // Terminal: the pending backoff retry was cancelled, nothing new opens.
    expect(FakeWebSocket.instances).toHaveLength(2);

    // Even a pasted token cannot revive a gone socket.
    provideToken('late-token');
    await vi.advanceTimersByTimeAsync(60_000);
    expect(FakeWebSocket.instances).toHaveLength(2);
  });

  it('keeps the reconnect schedule when the probe is inconclusive (mission alive)', async () => {
    const statuses: string[] = [];
    const socket = makeSocket((s) => statuses.push(s)); // fetch: default 200

    socket.connect(); // #1
    FakeWebSocket.instances[0].onclose?.();
    await vi.advanceTimersByTimeAsync(500); // retry #2
    FakeWebSocket.instances[1].onclose?.(); // failure 2 — probe fires, finds the mission
    await vi.advanceTimersByTimeAsync(1000); // backoff retry #3 still happens

    expect(fetch).toHaveBeenCalledTimes(1);
    expect(FakeWebSocket.instances).toHaveLength(3);
    expect(statuses).not.toContain('gone');

    socket.close();
  });
});
