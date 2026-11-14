import { useEffect, useState } from 'react';
import { hasAmbiguous, sizeNote, visible } from '../lib/presentation';
import { api } from '../lib/api';
import { pendingPermissions } from '../lib/pendingAsks';
import { useKranzStore } from '../lib/store';
import { useNow } from '../lib/useNow';
import type { LivePermission } from '../lib/types';

function PermissionCard({ record, expired, onDismiss, onActed }: {
  record: LivePermission;
  expired: boolean;
  onDismiss: () => void;
  onActed?: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [queued, setQueued] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const { proposal, binding, bindingDigest } = record.request;

  const ambiguous = hasAmbiguous(record.request);

  const answer = async (allow: boolean) => {
    if (expired) return;
    if (allow && ambiguous) return;
    setBusy(true);
    setError(null);
    try {
      await api.answerPermission(binding.missionId, proposal.id, bindingDigest, allow);
      setQueued(true);
      onActed?.();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="panel panel-grant">
      <div className="section-label">One-call permission</div>
      <div className="revision-body">
        <p>Allow this exact invocation once. Future calls require their own authorization.</p>
        <div className="dim">Run {visible(binding.runId)} · expires {new Date(proposal.deadline).toLocaleTimeString()}</div>
        <div className="mono">{visible(binding.workspace)}</div>
        <pre className="revision-diff">{visible(JSON.stringify(proposal.action, null, 2))}</pre>
        <div className="dim">{sizeNote(JSON.stringify(proposal.action, null, 2))}</div>
        <details>
          <summary>Approval binding</summary>
          <pre className="revision-diff">{visible(JSON.stringify({ requestId: proposal.id, bindingDigest, plan: binding.planDigest, policy: binding.policyDigest }, null, 2))}</pre>
        </details>
        {proposal.prohibition && <p role="status">Blocked by policy: {visible(proposal.prohibition)}</p>}
        {ambiguous && <p role="status">Blocked: invisible or control characters make this request unsafe to approve.</p>}
        <div className="revision-actions">
          <button type="button" className="strip-btn" disabled={busy || queued || expired || !!proposal.prohibition || ambiguous}
            onClick={() => void answer(true)}>Allow once</button>
          <button type="button" className="strip-btn strip-btn-danger" disabled={busy || queued || expired}
            onClick={() => void answer(false)}>Deny once</button>
        </div>
        {queued && <p role="status">Answer queued. The engine will record delivery separately.</p>}
        {error && <div className="picker-error revision-error" role="alert">{visible(error)}</div>}
        <p role="status" className="permission-status">
          {expired ? <>Expired — no answer was sent{' '}
            <button type="button" className="strip-btn" onClick={onDismiss}>Dismiss</button></> : ' '}
        </p>
      </div>
    </section>
  );
}

export function PermissionRequestPanel({ onActed }: { onActed?: () => void } = {}) {
  const state = useKranzStore((s) => s.state);
  const missionId = useKranzStore((s) => s.missionId);
  const now = useNow();
  const [retainedIds, setRetainedIds] = useState<string[]>([]);
  const [dismissed, setDismissed] = useState<Set<string>>(new Set());

  useEffect(() => {
    setRetainedIds([]);
    setDismissed(new Set());
  }, [missionId]);

  const live = pendingPermissions(state, now);
  const liveIds = new Set(live.map((r) => r.request.proposal.id));

  useEffect(() => {
    setRetainedIds((prev) => {
      const missing = live
        .map((r) => r.request.proposal.id)
        .filter((id) => !prev.includes(id));
      return missing.length ? [...prev, ...missing] : prev;
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [live]);

  const permissions = state?.permissions;
  const dismiss = (id: string) => setDismissed((prev) => {
    const next = new Set(prev);
    next.add(id);
    return next;
  });

  return <>{retainedIds
    .filter((id) => !dismissed.has(id))
    .map((id) => permissions?.[id])
    .filter((record): record is LivePermission => record != null)
    .map((record) => {
      const id = record.request.proposal.id;
      return <PermissionCard key={id} record={record} expired={!liveIds.has(id)}
        onDismiss={() => dismiss(id)} onActed={onActed} />;
    })}</>;
}
