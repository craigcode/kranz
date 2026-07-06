// Delivered-stage affordance (roadmap M6 "Merge on both human surfaces"):
// self-contained — keyed off backing state (MissionSummary.merged), not a
// new pipeline model. Renders only for a Complete, not-yet-merged mission:
// the report + diff-stat inline, an UNMERGED badge, and the gated Merge
// button. A failed merge surfaces the server's failing-gate text verbatim.

import { useCallback, useEffect, useState } from 'react';
import { api, ApiError } from '../lib/api';
import { renderMarkdown } from '../lib/markdown';

interface Props {
  missionId: string;
  status: string;
}

export function DeliveredPanel({ missionId, status }: Props) {
  const [merged, setMerged] = useState<boolean | null>(null);
  const [reportMarkdown, setReportMarkdown] = useState<string | null>(null);
  const [diffStat, setDiffStat] = useState<string | null>(null);
  const [merging, setMerging] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setMerged(null);
    setReportMarkdown(null);
    setDiffStat(null);
    setError(null);
    if (status !== 'complete') return;

    let cancelled = false;
    void api.missions().then((missions) => {
      if (cancelled) return;
      const row = missions.find((m) => m.id === missionId);
      setMerged(row?.merged ?? null);
    });
    void api
      .reportMd(missionId)
      .then((r) => !cancelled && setReportMarkdown(r.markdown))
      .catch(() => {
        /* report not available yet; panel still shows the diff + merge */
      });
    void api
      .diffStat(missionId)
      .then((r) => !cancelled && setDiffStat(r.diffStat))
      .catch(() => {
        /* diff-stat not available yet */
      });
    return () => {
      cancelled = true;
    };
  }, [missionId, status]);

  const onMerge = useCallback(() => {
    setError(null);
    setMerging(true);
    api
      .merge(missionId)
      .then(() => {
        setMerged(true);
      })
      .catch((err: unknown) => {
        setError(err instanceof ApiError ? err.message : String(err));
      })
      .finally(() => setMerging(false));
  }, [missionId]);

  if (status !== 'complete' || merged !== false) return null;

  return (
    <div className="delivered-panel">
      <div className="delivered-header">
        <span className="status-pill pill-unmerged">UNMERGED</span>
        <button type="button" className="btn-small delivered-merge-btn" disabled={merging} onClick={onMerge}>
          {merging ? 'Merging…' : 'Merge'}
        </button>
      </div>
      {diffStat !== null && (
        <pre className="delivered-diff-stat">{diffStat}</pre>
      )}
      {reportMarkdown !== null && <div className="delivered-report">{renderMarkdown(reportMarkdown)}</div>}
      {error !== null && (
        <pre className="picker-error" role="alert">
          {error}
        </pre>
      )}
    </div>
  );
}
