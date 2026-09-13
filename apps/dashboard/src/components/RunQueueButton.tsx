// The "Run queue" affordance (docs/scoping/pipeline-view.md "simple lists,
// easy buttons — one obvious action"): drains the per-repo queue over the
// hosted REST endpoints. Deliberately not named "Approve" (plan consent) or
// "Queue" (ticket-queueing, per D-A) — this triggers the drain/claim/skip
// loop itself.

import { useCallback, useEffect, useRef, useState } from 'react';
import { api, ApiError, isTokenRequired } from '../lib/api';
import type { QueueState } from '../lib/types';

const POLL_MS = 3000;

export function RunQueueButton() {
  const [queue, setQueue] = useState<QueueState | null>(null);
  const [error, setError] = useState<string | null>(null);
  /** Off-loopback 401 on the poll: passive status, never the TokenPrompt. */
  const [tokenRequired, setTokenRequired] = useState(false);
  /** True while the drain POST is in flight (guards double-click). */
  const [draining, setDraining] = useState(false);

  const refresh = useCallback(async () => {
    try {
      // tokenGate:false — a background poll must not park on the token gate
      // (it would reopen a dismissed TokenPrompt on the next tick).
      const state = await api.queue({ tokenGate: false });
      setQueue(state);
      setTokenRequired(false);
    } catch (err) {
      // 401 shows as a passive status; anything else keeps the last known
      // state and the next poll tick retries.
      if (isTokenRequired(err)) setTokenRequired(true);
    }
  }, []);

  const timer = useRef<number | undefined>(undefined);
  useEffect(() => {
    void refresh();
    timer.current = window.setInterval(() => void refresh(), POLL_MS);
    return () => window.clearInterval(timer.current);
  }, [refresh]);

  const onClick = useCallback(() => {
    setError(null);
    setDraining(true);
    api
      .drainQueue()
      .then(async (drain) => {
        // Prefer the host's live drain; if the immediate poll still lags and
        // reports idle/empty, keep the drain we just received so the button
        // stays disabled through the race window.
        try {
          const state = await api.queue();
          setQueue({
            ...state,
            drain: state.drain.live ? state.drain : drain,
          });
          setTokenRequired(false);
        } catch {
          setQueue((q) =>
            q === null ? { entries: [], busyWith: null, drain } : { ...q, drain },
          );
        }
      })
      .catch((err: unknown) => {
        setError(err instanceof ApiError ? err.message : String(err));
      })
      .finally(() => setDraining(false));
  }, []);

  const count = queue?.entries.length ?? 0;
  const live = queue?.drain.live ?? false;
  const empty = queue !== null && count === 0;
  const disabled = live || empty || draining;
  const front = queue?.entries[0];
  const frontReadiness = front?.readiness;
  const parkedN = queue?.drain.parked?.length ?? 0;

  let status: string;
  if (tokenRequired) {
    status = 'token required';
  } else if (live) {
    const id = queue?.drain.currentMissionId;
    status = id !== null && id !== undefined ? `Draining… ${id}` : 'Draining…';
  } else if (empty) {
    status = parkedN > 0 ? `queue empty · ${parkedN} parked` : 'queue empty';
  } else {
    const readyBit =
      frontReadiness !== undefined
        ? ` · front ${frontReadiness.overall}${
            frontReadiness.roles.some((r) => r.status !== 'ok' && r.status !== 'meterless')
              ? ` (${frontReadiness.roles.find((r) => r.status !== 'ok' && r.status !== 'meterless')?.nextAction ?? ''})`
              : ''
          }`
        : '';
    status = `${count} queued${readyBit}`;
  }

  return (
    <div className="run-queue">
      <button type="button" className="btn-small run-queue-btn" disabled={disabled} onClick={onClick}>
        Run queue
      </button>
      <span className="dim run-queue-status">{status}</span>
      {frontReadiness !== undefined &&
        frontReadiness.warnings.length > 0 &&
        !live &&
        !empty && (
          <div className="dim run-queue-readiness" title={frontReadiness.warnings.join('\n')}>
            readiness: {frontReadiness.overall}
          </div>
        )}
      {error !== null && (
        <div className="picker-error" role="alert">
          {error}
        </div>
      )}
    </div>
  );
}
