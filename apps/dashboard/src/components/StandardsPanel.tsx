import { useCallback, useEffect, useState } from 'react';
import { api, ApiError } from '../lib/api';
import { useKranzStore } from '../lib/store';
import type {
  MissionStandardsView,
  StandardsWaiverCandidate,
} from '../lib/types';

function defaultExpiry(): string {
  const date = new Date(Date.now() + 7 * 24 * 60 * 60 * 1000);
  const local = new Date(date.getTime() - date.getTimezoneOffset() * 60_000);
  return local.toISOString().slice(0, 16);
}

export function StandardsPanel() {
  const missionId = useKranzStore((state) => state.missionId);
  const lastSeq = useKranzStore((state) => state.state?.lastSeq ?? 0);
  const [view, setView] = useState<MissionStandardsView | null>(null);
  const [selected, setSelected] = useState<StandardsWaiverCandidate | null>(null);
  const [reason, setReason] = useState('');
  const [expires, setExpires] = useState(defaultExpiry);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [recorded, setRecorded] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    if (missionId === null) return;
    const next = await api.standards(missionId);
    setView(next);
  }, [missionId]);

  useEffect(() => {
    setView(null);
    setSelected(null);
    setError(null);
    setRecorded(null);
    void refresh().catch(() => setView({}));
  }, [refresh]);

  useEffect(() => {
    if (lastSeq === 0) return;
    void refresh().catch(() => undefined);
  }, [lastSeq, refresh]);

  const rows = view?.coverage?.rules ?? [];
  const counts = new Map<string, number>();
  for (const row of rows) counts.set(row.disposition, (counts.get(row.disposition) ?? 0) + 1);

  const approveWaiver = useCallback(async () => {
    if (missionId === null || selected === null) return;
    if (reason.trim() === '' || expires === '') {
      setError('Reason and expiry are required.');
      return;
    }
    setSubmitting(true);
    setError(null);
    setRecorded(null);
    try {
      const result = await api.approveStandardsWaiver(missionId, {
        ruleId: selected.rule.id,
        revision: selected.rule.revision,
        findingSubject: selected.findingSubject,
        reason: reason.trim(),
        expiresAt: new Date(expires).toISOString(),
      });
      setRecorded(
        `Waiver seq ${result.seq} recorded for ${result.rule.id} r${result.rule.revision}; diff sha256:${result.diffDigest}.`,
      );
      setSelected(null);
      setReason('');
      await refresh();
    } catch (cause: unknown) {
      setError(cause instanceof ApiError ? cause.message : String(cause));
    } finally {
      setSubmitting(false);
    }
  }, [expires, missionId, reason, refresh, selected]);

  if (view === null || (view.manifest === undefined && view.coverage === undefined)) return null;

  return (
    <section className="panel standards-panel" aria-label="Flight Rules standards">
      <div className="panel-heading-row">
        <h2>Flight Rules</h2>
        {view.manifest !== undefined && (
          <code className="standards-digest" title={view.manifest.digest}>
            {view.manifest.digest.slice(0, 10)}…
          </code>
        )}
      </div>

      {rows.length > 0 && (
        <div className="standards-counts" aria-label="Standards outcome counts">
          {['passed', 'failed', 'advisory', 'waived', 'not-evaluated', 'not-applicable'].map(
            (status) => (
              <span key={status} className={`standards-disposition disposition-${status}`}>
                {status}: {counts.get(status) ?? 0}
              </span>
            ),
          )}
        </div>
      )}

      <ul className="standards-coverage-list">
        {rows.map((row) => (
          <li key={`${row.id}-r${row.revision}`}>
            <div className="standards-rule-head">
              <code>{row.id} r{row.revision}</code>
              <strong className={`standards-disposition disposition-${row.disposition}`}>
                {row.disposition}
              </strong>
            </div>
            <div>{row.statement}</div>
            <div className="standards-rule-meta">
              {row.lifecycle} · {row.level.toUpperCase()} · {row.checker ?? 'no checker'}
            </div>
            {(row.evidence ?? []).map((evidence) => (
              <div className="standards-evidence" key={`${evidence.seq}-${evidence.reference}`}>
                seq {evidence.seq} · {evidence.mechanism} · {evidence.bearing} ·{' '}
                <code>{evidence.reference}</code>
                {evidence.waiver !== undefined && (
                  <span>
                    {' '}— waiver by {evidence.waiver.approver}, expires {evidence.waiver.expiresAt}
                  </span>
                )}
              </div>
            ))}
          </li>
        ))}
      </ul>

      {(view.coverage?.drift ?? []).map((drift) => (
        <div className="standards-drift" role="alert" key={drift.seq}>
          <strong>Policy drift refused merge.</strong>
          <div>approved sha256:{drift.approvedDigest}</div>
          <div>current {drift.currentDigest === undefined ? 'unreadable' : `sha256:${drift.currentDigest}`}</div>
          <ul>{drift.changedRules.map((change) => <li key={change}>{change}</li>)}</ul>
          <p>Revise and re-approve the mission against current policy; there is no bypass.</p>
        </div>
      ))}

      {(view.waiverCandidates ?? []).map((candidate) => (
        <button
          type="button"
          className="standards-waiver-open"
          key={`${candidate.rule.id}-${candidate.findingSubject}`}
          onClick={() => {
            setSelected(candidate);
            setRecorded(null);
            setError(null);
          }}
        >
          Review exact waiver for {candidate.rule.id} r{candidate.rule.revision}
        </button>
      ))}

      {selected !== null && (
        <div className="standards-waiver-form">
          <strong>Approve exact human waiver</strong>
          <p><code>{selected.rule.id} r{selected.rule.revision}</code> — {selected.rule.statement}</p>
          <p className="standards-evidence">Evidence: {selected.findingEvidence}</p>
          <label>
            Reason
            <textarea value={reason} onChange={(event) => setReason(event.target.value)} />
          </label>
          <label>
            Expires
            <input
              type="datetime-local"
              value={expires}
              onChange={(event) => setExpires(event.target.value)}
            />
          </label>
          <button type="button" disabled={submitting} onClick={() => void approveWaiver()}>
            {submitting ? 'Recording…' : 'Approve this exact exception'}
          </button>
          <button type="button" className="link-btn" onClick={() => setSelected(null)}>
            cancel
          </button>
        </div>
      )}
      {recorded !== null && <p className="standards-recorded">{recorded}</p>}
      {error !== null && <div className="composer-error" role="alert">{error}</div>}
    </section>
  );
}
