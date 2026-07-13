// Right column: parked capability-grant decision (approve/deny).
//
// A validator stopped by a command outside its allow-set parks the milestone
// and sets `pendingGrantRequest`. This panel appears exactly then, naming the
// command, and lets the operator approve (extend command_grants + re-validate)
// or deny (block, fail closed) in one click — the dashboard half of the
// grant-request decision flow.

import { useState } from 'react';
import { api } from '../lib/api';
import { useKranzStore } from '../lib/store';

export function GrantRequestPanel() {
  const state = useKranzStore((s) => s.state);
  const [busy, setBusy] = useState<'approve' | 'deny' | null>(null);
  const [error, setError] = useState<string | null>(null);

  const mission = state?.mission ?? null;
  const pending = state?.pendingGrantRequest;

  if (mission === null || pending === undefined) return null;

  const blurb =
    pending.kind === 'touch-path'
      ? 'A worker wrote a path outside the mission’s touch-set:'
      : 'A validator is blocked on a command outside its allow-set:';

  const decide = async (kind: 'approve' | 'deny') => {
    setBusy(kind);
    setError(null);
    try {
      if (kind === 'approve') {
        await api.approveGrant(mission.id, pending.command);
      } else {
        await api.denyGrant(mission.id, pending.command);
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(null);
    }
  };

  return (
    <section className="panel panel-grant">
      <div className="section-label">
        Grant request
        <span className="section-count mono">{pending.milestoneId}</span>
      </div>
      <div className="revision-body">
        <div className="dim">{blurb}</div>
        <pre className="revision-diff">{pending.command}</pre>
        <div className="revision-actions">
          <button
            type="button"
            className="strip-btn"
            disabled={busy !== null}
            onClick={() => void decide('approve')}
          >
            {busy === 'approve' ? 'Approving...' : 'Approve grant'}
          </button>
          <button
            type="button"
            className="strip-btn strip-btn-danger"
            disabled={busy !== null}
            onClick={() => void decide('deny')}
          >
            {busy === 'deny' ? 'Denying...' : 'Deny'}
          </button>
        </div>
      </div>
      {error !== null && (
        <div className="picker-error revision-error" role="alert">
          {error}
        </div>
      )}
    </section>
  );
}
