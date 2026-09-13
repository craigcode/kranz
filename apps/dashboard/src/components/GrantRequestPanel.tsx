// Right column: parked capability-grant decision (approve/deny).
//
// A run stopped by a capability boundary (validator command outside its
// allow-set, worker write outside the touch-set, worker deny rule, or an
// fs+net egress refusal) parks the milestone and sets `pendingGrantRequest`.
// This panel appears exactly then, naming the target, and lets the operator
// approve (extend the list the kind selects + re-run) or deny (the kind's
// refusal semantics) in one click — the dashboard half of the grant-request
// decision flow.

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
      : pending.kind === 'worker-deny'
        ? 'A worker command was blocked by a deny rule. Approving LIFTS that rule for this mission:'
        : pending.kind === 'egress'
          ? 'A sandboxed run was refused egress to a destination outside the mission’s egress allowlist. Approving extends the allowlist for this mission:'
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
