// Full-width status strip: color-coded status pill, pause/resume controls,
// a confirm-gated abandon, blocked reason (impossible to miss), and a thin
// feature-progress bar.

import { useEffect, useState } from 'react';
import { useKranzStore } from '../lib/store';
import type { MissionStatus } from '../lib/types';

const STATUS_LABEL: Record<MissionStatus, string> = {
  planning: 'Planning',
  approved: 'Approved',
  running: 'Running',
  paused: 'Paused',
  blocked: 'Blocked',
  validating: 'Validating',
  complete: 'Complete',
  failed: 'Failed',
  abandoned: 'Abandoned',
};

/** Statuses whose detail view offers abandon (running included — that's the
 *  confirm-gated stop; terminal missions have nothing left to abandon). */
const ABANDONABLE: ReadonlySet<MissionStatus> = new Set([
  'planning',
  'approved',
  'running',
  'paused',
  'blocked',
  'validating',
]);

export function StatusStrip() {
  const state = useKranzStore((s) => s.state);
  const events = useKranzStore((s) => s.events);
  const sendControl = useKranzStore((s) => s.sendControl);
  const selectedRun = useKranzStore((s) => s.selectedRun);
  const selectRun = useKranzStore((s) => s.selectRun);
  const abandonMission = useKranzStore((s) => s.abandonMission);
  const [armedAbandon, setArmedAbandon] = useState(false);

  // An armed abandon disarms itself: a stray click must not lie in wait.
  useEffect(() => {
    if (!armedAbandon) return;
    const t = window.setTimeout(() => setArmedAbandon(false), 5000);
    return () => window.clearTimeout(t);
  }, [armedAbandon]);

  if (!state) return <div className="status-strip status-planning" />;

  const status = state.mission.status;
  const features = state.mission.milestones.flatMap((m) => m.features);
  const done = features.filter((f) => f.status === 'complete').length;
  const pct = features.length > 0 ? (done / features.length) * 100 : 0;

  let blockedReason: string | null = null;
  if (status === 'blocked') {
    for (let i = events.length - 1; i >= 0; i--) {
      const e = events[i];
      if (e.type === 'milestone.blocked') {
        blockedReason = e.payload.reason;
        break;
      }
    }
  }

  const failedReason =
    status === 'failed'
      ? [...events].reverse().find((e) => e.type === 'mission.failed')
      : undefined;

  return (
    <div className={`status-strip status-${status}`}>
      <span className={`status-pill pill-${status}`}>
        <span className="status-dot" aria-hidden="true" />
        {STATUS_LABEL[status]}
      </span>

      {status === 'planning' && (
        <>
          <button
            type="button"
            className="strip-btn"
            title="show the planning conversation in the centre pane"
            onClick={() => selectRun(null)}
            disabled={selectedRun === null}
          >
            open planning
          </button>
          <span className="strip-hint">conversation → plan → approve → start</span>
        </>
      )}
      {status === 'paused' && (
        <button
          type="button"
          className="strip-btn"
          onClick={() => void sendControl({ kind: 'resume' }).catch(() => {})}
        >
          ▶ resume
        </button>
      )}
      {status === 'running' && (
        <button
          type="button"
          className="strip-btn"
          onClick={() => void sendControl({ kind: 'pause' }).catch(() => {})}
        >
          ⏸ pause
        </button>
      )}

      {ABANDONABLE.has(status) &&
        (armedAbandon ? (
          <button
            type="button"
            className="strip-btn strip-btn-danger"
            title={
              status === 'running' || status === 'validating'
                ? 'stops the run, kills its agent sessions, and records the abandonment'
                : 'records the abandonment in the event log; the directory stays'
            }
            onClick={() => {
              setArmedAbandon(false);
              void abandonMission(state.mission.id);
            }}
          >
            confirm abandon
          </button>
        ) : (
          <button type="button" className="strip-btn" onClick={() => setArmedAbandon(true)}>
            abandon
          </button>
        ))}

      {status === 'blocked' && (
        <span className="strip-reason">
          {blockedReason ?? 'milestone blocked'}
          <span className="strip-hint"> — send a message to unblock</span>
        </span>
      )}
      {status === 'failed' && failedReason?.type === 'mission.failed' && (
        <span className="strip-reason">{failedReason.payload.reason}</span>
      )}

      <div
        className="strip-progress"
        role="progressbar"
        aria-valuenow={done}
        aria-valuemin={0}
        aria-valuemax={features.length}
        aria-label="feature progress"
      >
        <div className="strip-progress-fill" style={{ width: `${pct}%` }} />
      </div>
      <span className="strip-count mono">
        {done}/{features.length}
      </span>
    </div>
  );
}
