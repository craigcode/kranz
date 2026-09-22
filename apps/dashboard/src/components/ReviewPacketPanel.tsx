import { useEffect, useRef, useState } from 'react';
import { api } from '../lib/api';
import { renderMarkdown } from '../lib/markdown';
import { useKranzStore } from '../lib/store';

// Explicit refresh captures working-tree edits too, which need not emit a
// mission event. Never submit consent from a cached read-only packet.
export function ReviewPacketPanel() {
  const missionId = useKranzStore((s) => s.missionId);
  const repoId = useKranzStore((s) => s.repoId);
  const seq = useKranzStore((s) => s.state?.lastSeq);
  if (!missionId) return null;
  return <Packet key={JSON.stringify([repoId, missionId])} missionId={missionId} seq={seq} />;
}

function Packet({ missionId, seq }: { missionId: string; seq: number | undefined }) {
  const request = useRef(0);
  const [result, setResult] = useState<Awaited<ReturnType<typeof api.reviewPacket>> | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  useEffect(() => () => { request.current += 1; }, []);
  const read = () => {
    const pending = ++request.current;
    setResult(null);
    setError(null);
    setLoading(true);
    void api.reviewPacket(missionId).then((value) => {
      if (pending === request.current) setResult(value);
    }).catch((err: unknown) => {
      if (pending === request.current) setError(err instanceof Error ? err.message : String(err));
    }).finally(() => {
      if (pending === request.current) setLoading(false);
    });
  };

  const stale = result !== null && (result.packet.missionId !== missionId || result.packet.throughSeq !== seq);
  return <section className="panel panel-review-packet">
    <div className="section-label">Human review packet</div>
    <div className="revision-body">
      <p>Scope, changes, evidence and the decision awaiting you. Refresh before acting to include edits outside the event log.</p>
      <button type="button" className="btn-small" disabled={loading} onClick={read}>
        {loading ? 'Reading evidence…' : result ? 'Refresh review packet' : 'Read review packet'}
      </button>
      {error && <p role="alert">Review packet unavailable: {error}</p>}
      {stale && <p role="status">Mission events changed. Refresh the packet before reviewing this decision.</p>}
      {result && !stale && <details open><summary>Observed {result.packet.observedAt} · event #{result.packet.throughSeq}</summary>
        {renderMarkdown(result.markdown)}
      </details>}
    </div>
  </section>;
}
