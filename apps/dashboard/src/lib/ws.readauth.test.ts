// Read-auth mode: browsers cannot set the x-kranz-token header on a
// WebSocket upgrade, so MissionSocket must append ?token=<t> whenever a
// token is resolvable, and omit it entirely otherwise.

import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { MissionSocket } from './ws';
import { cancelTokenPrompt, clearToken, setToken } from './token';

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
  FakeWebSocket.instances = [];
  vi.stubGlobal('WebSocket', FakeWebSocket);
  cancelTokenPrompt();
  clearToken();
});

afterEach(() => {
  vi.unstubAllGlobals();
  cancelTokenPrompt();
  clearToken();
});

describe('read-auth: MissionSocket URL token', () => {
  it('includes token=<t> in the query string when a token is resolvable', () => {
    setToken('ws-token');
    const socket = makeSocket();
    socket.connect();

    expect(FakeWebSocket.instances[0].url).toContain('token=ws-token');

    socket.close();
  });

  it('omits token= entirely when no token is resolvable', () => {
    const socket = makeSocket();
    socket.connect();

    expect(FakeWebSocket.instances[0].url).not.toContain('token=');

    socket.close();
  });
});
