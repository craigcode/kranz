// WebSocket client for /api/missions/:id/ws with auto-reconnect.
//
// Reconnect contract (docs/protocol.md): the client tracks the last seq seen
// and passes it as ?since=<seq> on reconnect. If the gap is small the server
// replays `event` frames (no snapshot) and we keep our state; otherwise it
// sends a fresh `snapshot`. Backoff is exponential: 0.5s doubling to 8s.

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

  constructor(opts: MissionSocketOptions) {
    this.opts = opts;
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
    const since = this.opts.getSince();
    const query = since !== null ? `?since=${since}` : '';
    return `${wsOrigin}/api/missions/${encodeURIComponent(this.opts.missionId)}/ws${query}`;
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
