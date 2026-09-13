// Right column: mid-mission plan revision request/review controls.

import { useEffect, useState } from 'react';
import { api } from '../lib/api';
import { useKranzStore } from '../lib/store';
import type { MissionStatus } from '../lib/types';

const REVISABLE: ReadonlySet<MissionStatus> = new Set([
  'approved',
  'running',
  'paused',
  'blocked',
  'validating',
]);

export function RevisionPanel() {
  const state = useKranzStore((s) => s.state);
  const [instructions, setInstructions] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const [decisionBusy, setDecisionBusy] = useState<'approve' | 'reject' | null>(null);
  const [diff, setDiff] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const mission = state?.mission ?? null;
  const pending = state?.pendingRevision;
  const missionId = mission?.id ?? null;
  const pendingRevision = pending?.revision ?? null;

  useEffect(() => {
    if (missionId === null || pendingRevision === null) {
      setDiff(null);
      setError(null);
      return;
    }
    let cancelled = false;
    setDiff(null);
    setError(null);
    api
      .revisionDiff(missionId)
      .then((res) => {
        if (!cancelled) setDiff(res.diff);
      })
      .catch((err: unknown) => {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err));
      });
    return () => {
      cancelled = true;
    };
  }, [missionId, pendingRevision]);

  if (mission === null || !REVISABLE.has(mission.status)) return null;

  const requestRevision = async () => {
    const trimmed = instructions.trim();
    if (trimmed === '') return;
    setSubmitting(true);
    setError(null);
    try {
      await api.requestRevision(mission.id, trimmed);
      setInstructions('');
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSubmitting(false);
    }
  };

  const decide = async (kind: 'approve' | 'reject') => {
    if (pending === undefined) return;
    setDecisionBusy(kind);
    setError(null);
    try {
      if (kind === 'approve') {
        await api.approveRevision(mission.id, pending.revision);
      } else {
        await api.rejectRevision(mission.id, pending.revision);
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setDecisionBusy(null);
    }
  };

  return (
    <section className="panel panel-revision">
      <div className="section-label">
        Revision
        {pending !== undefined && <span className="section-count mono">rev {pending.revision}</span>}
      </div>
      {pending !== undefined ? (
        <div className="revision-body">
          <div className="revision-instructions">{pending.instructions}</div>
          <div className="revision-actions">
            <button
              type="button"
              className="strip-btn"
              disabled={decisionBusy !== null}
              onClick={() => void decide('approve')}
            >
              {decisionBusy === 'approve' ? 'Approving...' : 'Approve'}
            </button>
            <button
              type="button"
              className="strip-btn strip-btn-danger"
              disabled={decisionBusy !== null}
              onClick={() => void decide('reject')}
            >
              {decisionBusy === 'reject' ? 'Rejecting...' : 'Reject'}
            </button>
          </div>
          {diff === null && error === null && <div className="dim panel-empty">Loading diff...</div>}
          {diff !== null && <pre className="revision-diff">{diff}</pre>}
        </div>
      ) : (
        <div className="revision-body">
          <textarea
            className="revision-input"
            rows={4}
            value={instructions}
            onChange={(e) => setInstructions(e.target.value)}
          />
          <button
            type="button"
            className="strip-btn revision-submit"
            disabled={submitting || instructions.trim() === ''}
            onClick={() => void requestRevision()}
          >
            {submitting ? 'Requesting...' : 'Request revision'}
          </button>
        </div>
      )}
      {error !== null && (
        <div className="picker-error revision-error" role="alert">
          {error}
        </div>
      )}
    </section>
  );
}
