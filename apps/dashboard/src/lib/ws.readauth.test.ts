// WebSocket URLs carry only a read token obtained through a header-authenticated exchange.

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
  vi.stubGlobal('fetch', vi.fn(async () => ({ ok: true, json: async () => ({ token: 'read-only' }) })));
  cancelTokenPrompt();
  clearToken();
});

afterEach(() => {
  vi.unstubAllGlobals();
  cancelTokenPrompt();
  clearToken();
});

describe('read-auth: MissionSocket URL token', () => {
  it('exchanges the credential in a header and puts only read authority in the URL', async () => {
    setToken('ws-token');
    const socket = makeSocket();
    socket.connect();

    await vi.waitFor(() => expect(FakeWebSocket.instances).toHaveLength(1));
    expect(fetch).toHaveBeenCalledWith('http://127.0.0.1:4560/api/read-token', expect.objectContaining({
      headers: { 'x-kranz-token': 'ws-token' }, redirect: 'error', cache: 'no-store',
    }));
    expect(FakeWebSocket.instances[0].url).toContain('token=read-only');
    expect(FakeWebSocket.instances[0].url).not.toContain('ws-token');

    socket.close();
  });

  it('omits token= entirely when no token is resolvable', () => {
    const socket = makeSocket();
    socket.connect();

    expect(FakeWebSocket.instances[0].url).not.toContain('token=');
    expect(fetch).not.toHaveBeenCalled();

    socket.close();
  });
});


it('never falls back to putting a credential in the URL when exchange fails', async () => {
  vi.mocked(fetch).mockResolvedValue({ ok: false, status: 401 } as Response);
  setToken('mutation-secret');
  const socket = makeSocket();
  socket.connect();
  await vi.waitFor(() => expect(fetch).toHaveBeenCalledOnce());
  expect(FakeWebSocket.instances).toHaveLength(0);
  socket.close();
});

it('does not open a socket after closing during the exchange', async () => {
  let finish!: (response: Response) => void;
  vi.mocked(fetch).mockImplementation(() => new Promise(resolve => { finish = resolve; }));
  setToken('mutation-secret');
  const socket = makeSocket();
  socket.connect();
  socket.close();
  finish({ ok: true, json: async () => ({ token: 'late-read' }) } as Response);
  await Promise.resolve();
  await Promise.resolve();
  expect(FakeWebSocket.instances).toHaveLength(0);
});

it('ignores an old exchange when a newer credential starts connecting', async () => {
  let finish!: (response: Response) => void;
  vi.mocked(fetch).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  setToken('old-mutation');
  const socket = makeSocket();
  socket.connect();
  setToken('new-mutation');
  socket.connect();
  await vi.waitFor(() => expect(FakeWebSocket.instances).toHaveLength(1));
  finish({ ok: true, json: async () => ({ token: 'stale-read' }) } as Response);
  await Promise.resolve();
  await Promise.resolve();
  expect(FakeWebSocket.instances).toHaveLength(1);
  expect(FakeWebSocket.instances[0].url).toContain('token=read-only');
  socket.close();
});
