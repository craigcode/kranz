// WebSocket client for /api/missions/:id/ws with auto-reconnect.
//
// Reconnect contract (docs/protocol.md): the client tracks the last seq seen
// and passes it as ?since=<seq> on reconnect. If the gap is small the server
// replays `event` frames (no snapshot) and we keep our state; otherwise it
// sends a fresh `snapshot`. Backoff is exponential: 0.5s doubling to 8s.
//
// The one terminal state: a mission deleted out-of-band (`kranz clean` in a
// terminal, another tab) 404s on every upgrade, which would reconnect-loop
// at the backoff cap forever. After repeated failures a cheap REST probe
// checks whether the mission still exists; a 404 stops the socket for good
// and reports 'gone' through onStatus.
//
// Authenticated sockets first exchange the header credential for read-only
// authority. Only that read token enters the WebSocket URL.

import { ApiError, getJson, scopedApiPath } from './api';
import { repoIdFromHash } from './routes';
import { resolveToken, subscribeTokenGate } from './token';
import type { WsFrame } from './types';

const BACKOFF_MIN_MS = 500;
const BACKOFF_MAX_MS = 8000;
/** Closes-without-open tolerated before the existence probe kicks in. */
const GONE_PROBE_AFTER_FAILURES = 2;

export interface MissionSocketOptions {
  /** http(s) origin of the server (no trailing slash). */
  origin: string;
  missionId: string;
  /** Repository captured when the socket is created. */
  repoId?: string | null;
  /** Last seq seen, or null to request a fresh snapshot. */
  getSince: () => number | null;
  onFrame: (frame: WsFrame) => void;
  /** 'gone' is terminal: the mission 404s server-side and the socket has
   *  stopped reconnecting for good. */
  onStatus: (status: 'connecting' | 'live' | 'lost' | 'gone') => void;
}

export class MissionSocket {
  private ws: WebSocket | null = null;
  private closed = false;
  private authRequest: AbortController | null = null;
  private backoffMs = BACKOFF_MIN_MS;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  /** Consecutive closes without a successful open (resets on 'live'). */
  private failures = 0;
  /** True while an existence probe is in flight (at most one at a time). */
  private probing = false;
  private readonly opts: MissionSocketOptions;
  private readonly unsubscribeToken: () => void;

  constructor(opts: MissionSocketOptions) {
    this.opts = opts;
    // A freshly pasted token must not wait out the backoff (up to 8s of
    // "lost"): when the token gate resolves with a token in hand, reset the
    // backoff and — if a reconnect is pending — retry immediately.
    this.unsubscribeToken = subscribeTokenGate(() => {
      if (this.closed || resolveToken() === null) return;
      this.backoffMs = BACKOFF_MIN_MS;
      if (this.reconnectTimer !== null || this.authRequest !== null) {
        if (this.reconnectTimer !== null) clearTimeout(this.reconnectTimer);
        this.reconnectTimer = null;
        this.connect();
      }
    });
  }

  connect(): void {
    if (this.closed) return;
    this.opts.onStatus('connecting');
    this.authRequest?.abort();
    this.authRequest = null;
    // Server middleware decides whether anonymous reads are allowed. This
    // branch only chooses whether the connection needs a credential exchange.
    const token = resolveToken();
    if (token === null) {
      this.openSocket(null);
    } else {
      void this.exchangeAndConnect(token);
    }
  }

  private async exchangeAndConnect(token: string): Promise<void> {
    const request = new AbortController();
    this.authRequest = request;
    const timeout = setTimeout(() => request.abort(), 10_000);
    try {
      const response = await fetch(`${this.opts.origin}/api/read-token`, {
        headers: { 'x-kranz-token': token },
        cache: 'no-store',
        redirect: 'error',
        credentials: 'omit',
        signal: request.signal,
      });
      if (!response.ok) throw new Error('read-token exchange failed');
      const body: unknown = await response.json();
      if (typeof body !== 'object' || body === null || !('token' in body) ||
          typeof body.token !== 'string' || body.token === '') {
        throw new Error('invalid read-token response');
      }
      if (this.closed || this.authRequest !== request) return;
      if (resolveToken() !== token) {
        this.connect();
        return;
      }
      this.openSocket(body.token);
    } catch {
      if (!this.closed && this.authRequest === request) {
        this.opts.onStatus('lost');
        this.scheduleReconnect();
      }
    } finally {
      clearTimeout(timeout);
      if (this.authRequest === request) this.authRequest = null;
    }
  }

  private openSocket(readToken: string | null): void {
    let ws: WebSocket;
    try {
      ws = new WebSocket(this.url(readToken));
    } catch {
      this.scheduleReconnect();
      return;
    }
    this.ws = ws;

    ws.onopen = () => {
      if (this.closed) return;
      this.backoffMs = BACKOFF_MIN_MS;
      this.failures = 0;
      this.opts.onStatus('live');
    };

    ws.onmessage = (msg: MessageEvent) => {
      if (this.closed || typeof msg.data !== 'string') return;
      let frame: WsFrame;
      try {
        frame = JSON.parse(msg.data) as WsFrame;
      } catch {
        return; // tolerate malformed frames
      }
      this.opts.onFrame(frame);
    };

    ws.onclose = () => {
      if (this.closed) return;
      this.failures += 1;
      this.opts.onStatus('lost');
      this.scheduleReconnect();
      // Repeated closes without ever opening look like the mission is gone
      // (its dir deleted out-of-band — the upgrade 404s before onopen). The
      // probe runs ALONGSIDE the scheduled retry, not instead of it, so a
      // token nudge keeps its immediate reconnect; a terminal 404 cancels
      // the pending retry via close().
      if (this.failures >= GONE_PROBE_AFTER_FAILURES) void this.probeMissionGone();
    };

    ws.onerror = () => {
      // onclose follows; nothing to do here.
    };
  }

  close(): void {
    this.closed = true;
    this.authRequest?.abort();
    this.authRequest = null;
    this.unsubscribeToken();
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    if (this.ws) {
      this.ws.onclose = null;
      this.ws.close();
      this.ws = null;
    }
  }

  private url(readToken: string | null): string {
    const wsOrigin = this.opts.origin.replace(/^http/i, 'ws');
    const params = new URLSearchParams();
    const since = this.opts.getSince();
    if (since !== null) params.set('since', String(since));
    if (readToken !== null) params.set('token', readToken);
    const query = params.toString();
    const q = query === '' ? '' : `?${query}`;
    const repoId = this.opts.repoId === undefined ? repoIdFromHash() : this.opts.repoId;
    const path = scopedApiPath(
      `/api/missions/${encodeURIComponent(this.opts.missionId)}/ws`,
      repoId,
    );
    return `${wsOrigin}${path}${q}`;
  }

  private scheduleReconnect(): void {
    if (this.closed || this.reconnectTimer !== null) return;
    const delay = this.backoffMs;
    this.backoffMs = Math.min(this.backoffMs * 2, BACKOFF_MAX_MS);
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this.connect();
    }, delay);
  }

  /** Fail-fast existence check for the mission behind this socket. An HTTP
   *  404 is terminal: the socket closes for good and reports 'gone' (the
   *  store tears the dead connection down). Anything else — success, a 401
   *  off-loopback, a network error — is inconclusive and leaves the normal
   *  reconnect schedule alone. */
  private async probeMissionGone(): Promise<void> {
    if (this.probing) return;
    this.probing = true;
    let gone = false;
    try {
      const repoId = this.opts.repoId === undefined ? repoIdFromHash() : this.opts.repoId;
      await getJson(scopedApiPath(`/api/missions/${encodeURIComponent(this.opts.missionId)}/state`, repoId), {
        tokenGate: false,
      });
    } catch (err) {
      gone = err instanceof ApiError && err.status === 404;
    } finally {
      this.probing = false;
    }
    if (this.closed || !gone) return;
    this.close();
    this.opts.onStatus('gone');
  }
}
