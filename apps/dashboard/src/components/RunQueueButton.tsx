// The "Run queue" affordance (docs/scoping/pipeline-view.md "simple lists,
// easy buttons — one obvious action"): drains the per-repo queue over the
// hosted REST endpoints. Deliberately not named "Approve" (plan consent) or
// "Queue" (ticket-queueing, per D-A) — this triggers the drain/claim/skip
// loop itself.

import { useCallback, useEffect, useRef, useState } from 'react';
import { api, ApiError } from '../lib/api';
import type { QueueState } from '../lib/types';

const POLL_MS = 3000;

export function RunQueueButton() {
  const [queue, setQueue] = useState<QueueState | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const state = await api.queue();
      setQueue(state);
    } catch {
      // keep the last known state; the next poll tick retries
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
    api
      .drainQueue()
      .then((drain) => setQueue((q) => (q === null ? q : { ...q, drain })))
      .catch((err: unknown) => {
        setError(err instanceof ApiError ? err.message : String(err));
      });
  }, []);

  const count = queue?.entries.length ?? 0;
  const live = queue?.drain.live ?? false;
  const empty = queue !== null && count === 0;
  const disabled = live || empty;

  let status: string;
  if (live) {
    const id = queue?.drain.currentMissionId;
    status = id !== null && id !== undefined ? `Draining… ${id}` : 'Draining…';
  } else if (empty) {
    status = 'queue empty';
  } else {
    status = `${count} queued`;
  }

  return (
    <div className="run-queue">
      <button type="button" className="btn-small run-queue-btn" disabled={disabled} onClick={onClick}>
        Run queue
      </button>
      <span className="dim run-queue-status">{status}</span>
      {error !== null && (
        <div className="picker-error" role="alert">
          {error}
        </div>
      )}
    </div>
  );
}
