// WebSocket client for /api/missions/:id/ws with auto-reconnect.
//
// Reconnect contract (docs/protocol.md): the client tracks the last seq seen
// and passes it as ?since=<seq> on reconnect. If the gap is small the server
// replays `event` frames (no snapshot) and we keep our state; otherwise it
// sends a fresh `snapshot`. Backoff is exponential: 0.5s doubling to 8s.
//
// Off-loopback serves also require ?token= (browsers cannot set the
// x-kranz-token header on WebSocket upgrades).

import { resolveToken, subscribeTokenGate } from './token';
import type { WsFrame } from './types';

const BACKOFF_MIN_MS = 500;
const BACKOFF_MAX_MS = 8000;

export interface MissionSocketOptions {
  /** http(s) origin of the server (no trailing slash). */
  origin: string;
  missionId: string;
  /** Last seq seen, or null to request a fresh snapshot. */
  getSince: () => number | null;
  onFrame: (frame: WsFrame) => void;
  onStatus: (status: 'connecting' | 'live' | 'lost') => void;
}

export class MissionSocket {
  private ws: WebSocket | null = null;
  private closed = false;
  private backoffMs = BACKOFF_MIN_MS;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
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
      if (this.reconnectTimer !== null) {
        clearTimeout(this.reconnectTimer);
        this.reconnectTimer = null;
        this.connect();
      }
    });
  }

  connect(): void {
    if (this.closed) return;
    this.opts.onStatus('connecting');

    let ws: WebSocket;
    try {
      ws = new WebSocket(this.url());
    } catch {
      this.scheduleReconnect();
      return;
    }
    this.ws = ws;

    ws.onopen = () => {
      if (this.closed) return;
      this.backoffMs = BACKOFF_MIN_MS;
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
      this.opts.onStatus('lost');
      this.scheduleReconnect();
    };

    ws.onerror = () => {
      // onclose follows; nothing to do here.
    };
  }

  close(): void {
    this.closed = true;
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

  private url(): string {
    const wsOrigin = this.opts.origin.replace(/^http/i, 'ws');
    const params = new URLSearchParams();
    const since = this.opts.getSince();
    if (since !== null) params.set('since', String(since));
    const token = resolveToken();
    if (token !== null) params.set('token', token);
    const query = params.toString();
    const q = query === '' ? '' : `?${query}`;
    return `${wsOrigin}/api/missions/${encodeURIComponent(this.opts.missionId)}/ws${q}`;
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
}
