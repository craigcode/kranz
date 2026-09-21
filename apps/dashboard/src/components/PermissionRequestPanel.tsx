import { useState } from 'react';
import { api } from '../lib/api';
import { useKranzStore } from '../lib/store';
import { useNow } from '../lib/useNow';
import type { LivePermission } from '../lib/types';

function PermissionCard({ record }: { record: LivePermission }) {
  const [busy, setBusy] = useState(false);
  const [queued, setQueued] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const { proposal, binding, bindingDigest } = record.request;

  const answer = async (allow: boolean) => {
    setBusy(true);
    setError(null);
    try {
      await api.answerPermission(binding.missionId, proposal.id, bindingDigest, allow);
      setQueued(true);
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
        <div className="dim">Run {binding.runId} · expires {new Date(proposal.deadline).toLocaleTimeString()}</div>
        <div className="mono">{binding.workspace}</div>
        <pre className="revision-diff">{JSON.stringify(proposal.action, null, 2)}</pre>
        <details>
          <summary>Approval binding</summary>
          <pre className="revision-diff">{JSON.stringify({ requestId: proposal.id, bindingDigest, plan: binding.planDigest, policy: binding.policyDigest }, null, 2)}</pre>
        </details>
        {proposal.prohibition && <p role="status">Blocked by policy: {proposal.prohibition}</p>}
        <div className="revision-actions">
          <button type="button" className="strip-btn" disabled={busy || queued || !!proposal.prohibition}
            onClick={() => void answer(true)}>Allow once</button>
          <button type="button" className="strip-btn strip-btn-danger" disabled={busy || queued}
            onClick={() => void answer(false)}>Deny once</button>
        </div>
        {queued && <p role="status">Answer queued. The engine will record delivery separately.</p>}
        {error && <div className="picker-error revision-error" role="alert">{error}</div>}
      </div>
    </section>
  );
}

export function PermissionRequestPanel() {
  const records = useKranzStore((s) => s.state?.permissions);
  const now = useNow();
  return <>{Object.values(records ?? {})
    .filter((r) => !r.resolution && !r.closed && Date.parse(r.request.proposal.deadline) > now)
    .map((record) => <PermissionCard key={record.request.proposal.id} record={record} />)}</>;
}
