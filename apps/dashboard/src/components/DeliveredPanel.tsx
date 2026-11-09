// Delivered-stage affordance (roadmap M6 "Merge on both human surfaces"):
// self-contained — keyed off backing state (MissionSummary.merged), not a
// new pipeline model. Renders only for a Complete, not-yet-merged mission:
// the report + diff-stat inline, an UNMERGED badge, the gated Merge button,
// and optional PR handoff (copyable push / gh create — never auto-push).

import { useCallback, useEffect, useRef, useState } from 'react';
import { api, ApiError } from '../lib/api';
import { renderMarkdown } from '../lib/markdown';
import { visible } from '../lib/presentation';
import type { PrHandoff } from '../lib/types';

interface Props {
  missionId: string;
  status: string;
  onMerged?: () => void;
}

const SAFE_PR_URL_RE = /^https?:\/\//i;

export function DeliveredPanel({ missionId, status, onMerged }: Props) {
  const [merged, setMerged] = useState<boolean | null>(null);
  const [reportMarkdown, setReportMarkdown] = useState<string | null>(null);
  const [diffStat, setDiffStat] = useState<string | null>(null);
  const [prHandoff, setPrHandoff] = useState<PrHandoff | null>(null);
  const [merging, setMerging] = useState(false);
  const [creatingPr, setCreatingPr] = useState(false);
  const [prUrl, setPrUrl] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const activeRef = useRef({ mounted: true, missionId, status });
  activeRef.current.missionId = missionId;
  activeRef.current.status = status;

  useEffect(() => {
    activeRef.current.mounted = true;
    return () => {
      activeRef.current.mounted = false;
    };
  }, []);

  useEffect(() => {
    setMerged(null);
    setReportMarkdown(null);
    setDiffStat(null);
    setPrHandoff(null);
    setMerging(false);
    setCreatingPr(false);
    setPrUrl(null);
    setError(null);
    if (status !== 'complete') return;

    let cancelled = false;
    void api
      .missions()
      .then((missions) => {
        if (cancelled) return;
        const row = missions.find((m) => m.id === missionId);
        setMerged(row?.merged ?? null);
      })
      .catch(() => {
        /* mission list probe failed; avoid unhandled rejection */
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
    void api
      .prHandoff(missionId)
      .then((h) => !cancelled && setPrHandoff(h))
      .catch(() => {
        /* handoff probe failed; merge still available */
      });
    return () => {
      cancelled = true;
    };
  }, [missionId, status]);

  const onMerge = useCallback(() => {
    const reqMissionId = missionId;
    const isCurrent = () =>
      activeRef.current.mounted &&
      activeRef.current.missionId === reqMissionId &&
      activeRef.current.status === 'complete';
    setError(null);
    setMerging(true);
    api
      .merge(reqMissionId)
      .then(() => {
        if (!isCurrent()) return;
        setMerged(true);
        onMerged?.();
      })
      .catch((err: unknown) => {
        if (!isCurrent()) return;
        setError(err instanceof ApiError ? err.message : String(err));
      })
      .finally(() => {
        if (isCurrent()) setMerging(false);
      });
  }, [missionId, onMerged]);

  const onCreatePr = useCallback(() => {
    const reqMissionId = missionId;
    const isCurrent = () =>
      activeRef.current.mounted &&
      activeRef.current.missionId === reqMissionId &&
      activeRef.current.status === 'complete';
    setError(null);
    setCreatingPr(true);
    api
      .createPr(reqMissionId)
      .then((r) => {
        if (!isCurrent()) return;
        setPrUrl(r.url);
      })
      .catch((err: unknown) => {
        if (!isCurrent()) return;
        setError(err instanceof ApiError ? err.message : String(err));
      })
      .finally(() => {
        if (isCurrent()) setCreatingPr(false);
      });
  }, [missionId]);

  const copyCommand = useCallback(async (command: string) => {
    try {
      await navigator.clipboard.writeText(command);
    } catch {
      /* clipboard may be unavailable; command remains selectable in the pre */
    }
  }, []);

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
      {prHandoff !== null && (
        <div className="delivered-pr-handoff">
          {prHandoff.kind === 'needsPush' && (
            <>
              <p className="delivered-pr-label">
                Branch is local only — kranz never pushes. Copy this for your machine:
              </p>
              <pre className="delivered-pr-command">{prHandoff.command}</pre>
              <button
                type="button"
                className="btn-small"
                onClick={() => void copyCommand(prHandoff.command)}
              >
                Copy push command
              </button>
            </>
          )}
          {prHandoff.kind === 'readyToCreate' && (
            <>
              <p className="delivered-pr-label">Remote branch present — open a PR (no push):</p>
              <pre className="delivered-pr-command">{prHandoff.command}</pre>
              <button
                type="button"
                className="btn-small"
                disabled={creatingPr}
                onClick={onCreatePr}
              >
                {creatingPr ? 'Creating PR…' : 'Create PR'}
              </button>
              {prUrl !== null && (
                <p className="delivered-pr-url">
                  {SAFE_PR_URL_RE.test(prUrl) ? (
                    <a href={prUrl} target="_blank" rel="noreferrer">
                      {visible(prUrl)}
                    </a>
                  ) : (
                    <span>{visible(prUrl)}</span>
                  )}
                </p>
              )}
            </>
          )}
          {prHandoff.kind === 'unavailable' && (
            <p className="delivered-pr-unavailable">PR handoff unavailable: {prHandoff.reason}</p>
          )}
        </div>
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
