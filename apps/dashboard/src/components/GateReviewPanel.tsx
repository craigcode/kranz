import { useKranzStore } from '../lib/store';
import { useNow } from '../lib/useNow';

export function GateReviewPanel() {
  const records = useKranzStore((s) => s.state?.gateEvaluations);
  const now = useNow();
  const pending = Object.values(records ?? {})
    .filter((r) => !r.closed && !r.consumed && r.resolution && r.resolution.disposition !== 'proceed')
    .sort((a, b) => a.requestedSeq - b.requestedSeq);
  return <>{pending.map((record) => {
    const request = record.requested.request.params;
    const outcome = record.finished?.outcome;
    const detail = outcome?.status === 'evaluated' ? outcome.result.rationale : outcome?.message;
    const expired = Date.parse(request.deadline) <= now;
    return <section className="panel panel-grant" key={request.attemptId}>
      <div className="section-label">Gate review needed</div>
      <div className="revision-body">
        <p><strong>{request.gateId}</strong> · {request.stage}</p>
        <p>{record.resolution?.disposition === 'require-human' ? 'The evaluator requested human review.' : 'This check blocks progress.'}</p>
        {detail && <pre className="revision-diff">{detail}</pre>}
        <p>Inspect the evidence and correct the cause before retrying this stage. Required checks and consent still apply.</p>
        {expired && <p role="status">This attempt expired. Retry to collect fresh evidence.</p>}
        <details><summary>Evidence binding</summary>
          <pre className="revision-diff">{JSON.stringify({ attempt: request.attemptId, subject: request.subject, binding: request.binding }, null, 2)}</pre>
        </details>
      </div>
    </section>;
  })}</>;
}
