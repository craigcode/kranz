// Right column: hook-derived lifecycle signals (ticket
// `agent-hooks-status-signals`) — the OPTIONAL, non-authoritative
// observability lane for hook-capable CLI backends (cursor).
//
// When the lane is enabled, a session's lifecycle hooks report coarse
// signals ("running" / "needs input" / "interrupted" / "turn finished")
// into the ephemeral `.kranz/hook-status/` projection, served by
// `GET /api/missions/:id/hook-status`. This panel renders them in the SAME
// "your move" right column as the parked grant and open questions (sharing
// the chrome classes) — but always labelled hook-derived and
// non-authoritative: a signal is what the backend's hooks last said, never
// folded mission state, and a terminal mission's chrome is unchanged by it.
// The projection is out-of-band (signals do not arrive as events), so the
// panel polls on a short interval rather than keying off the event stream.

import { useEffect, useState } from 'react';
import { api } from '../lib/api';
import { useKranzStore } from '../lib/store';
import type { HookStatusSignalKind, MissionHookStatus } from '../lib/types';

const SIGNAL_LABEL: Record<HookStatusSignalKind, string> = {
  running: 'running',
  'needs-input': 'needs input',
  interrupted: 'interrupted',
  'turn-finished': 'turn finished',
};

const POLL_MS = 5000;

export function HookStatusPanel() {
  const missionId = useKranzStore((s) => s.missionId);
  const [projection, setProjection] = useState<MissionHookStatus | null>(null);

  useEffect(() => {
    setProjection(null);
    if (missionId === null) return;
    let cancelled = false;
    const refresh = () => {
      void api
        .hookStatus(missionId)
        .then((value) => !cancelled && setProjection(value))
        .catch(() => {
          // The lane is optional and advisory: a missing/again-later
          // endpoint degrades to no panel, never to an error chrome.
        });
    };
    refresh();
    const timer = setInterval(refresh, POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [missionId]);

  const signaled = (projection?.runs ?? []).filter((run) => run.signal !== undefined);
  if (missionId === null || signaled.length === 0) return null;

  return (
    <section className="panel panel-hook-status">
      <div className="section-label">
        Hook signals
        <span className="section-count mono">{signaled.length}</span>
      </div>
      {signaled.map((run) => {
        const record = run.signal!;
        return (
          <div className="revision-body" key={run.runId}>
            <div className="dim">
              {SIGNAL_LABEL[record.signal]} · run {run.runId.slice(0, 8)}… · hook-derived, not
              mission state
            </div>
            {record.detail !== undefined && <pre className="revision-diff">{record.detail}</pre>}
          </div>
        );
      })}
    </section>
  );
}
