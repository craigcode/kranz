// Full-width status strip: color-coded status pill, pause/resume controls,
// blocked reason (impossible to miss), and a thin feature-progress bar.

import { useKranzStore } from '../lib/store';
import type { MissionStatus } from '../lib/types';

const STATUS_LABEL: Record<MissionStatus, string> = {
  planning: 'Planning',
  running: 'Running',
  paused: 'Paused',
  blocked: 'Blocked',
  validating: 'Validating',
  complete: 'Complete',
  failed: 'Failed',
};

export function StatusStrip() {
  const state = useKranzStore((s) => s.state);
  const events = useKranzStore((s) => s.events);
  const sendControl = useKranzStore((s) => s.sendControl);

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
