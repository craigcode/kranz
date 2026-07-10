import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { MissionSocket } from './ws';
import { cancelTokenPrompt, clearToken, provideToken } from './token';

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

function makeSocket(): MissionSocket {
  return new MissionSocket({
    origin: 'http://127.0.0.1:4560',
    missionId: 'm-1',
    getSince: () => null,
    onFrame: () => {},
    onStatus: () => {},
  });
}

beforeEach(() => {
  vi.useFakeTimers();
  FakeWebSocket.instances = [];
  vi.stubGlobal('WebSocket', FakeWebSocket);
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

  it('close() unsubscribes: a later provideToken opens no new socket', () => {
    const socket = makeSocket();
    socket.connect();
    FakeWebSocket.instances[0].onclose?.(); // reconnect pending

    socket.close();
    provideToken('late-token');

    expect(FakeWebSocket.instances).toHaveLength(1);
  });
});
