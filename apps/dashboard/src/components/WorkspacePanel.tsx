// Effective local workspace and sandbox posture. The server derives this
// projection from folded mission config plus the existing preflight decision
// event; this component never guesses an absolute worktree path.

import { useEffect, useMemo, useState } from 'react';
import { api } from '../lib/api';
import { useKranzStore } from '../lib/store';
import type { WorkspaceSummary } from '../lib/types';

const LIFECYCLE_LABEL: Record<WorkspaceSummary['lifecycle'], string> = {
  active: 'active',
  pending: 'pending setup',
  removed: 'ephemeral · removed',
  'primary-checkout': 'primary checkout',
};

function sandboxLabel(row: WorkspaceSummary['sandboxes'][number]): string {
  const grants = [];
  if (row.extraWriteCount > 0) grants.push(`+${row.extraWriteCount} write`);
  if (row.egressCount > 0) grants.push(`+${row.egressCount} egress`);
  return `${row.role} ${row.enforce}${grants.length > 0 ? ` · ${grants.join(' · ')}` : ''}`;
}

export function WorkspacePanel() {
  const missionId = useKranzStore((s) => s.missionId);
  const state = useKranzStore((s) => s.state);
  const events = useKranzStore((s) => s.events);
  const [summary, setSummary] = useState<WorkspaceSummary | null>(null);
  const [error, setError] = useState<string | null>(null);

  const preflightSeq = useMemo(() => {
    for (let i = events.length - 1; i >= 0; i--) {
      const event = events[i];
      if (
        event.type === 'orchestrator.decision' &&
        event.payload.summary.startsWith('preflight:')
      ) {
        return event.seq;
      }
    }
    return 0;
  }, [events]);

  const workspaceKey = state
    ? [
        state.mission.status,
        state.config.workerIsolation ?? 'worktree',
        state.config.worker.sandbox?.enforce ?? 'off',
        state.config.worker.sandbox?.extraWrite.length ?? 0,
        state.config.worker.sandbox?.egress.length ?? 0,
        state.config.validatorScrutiny.sandbox?.enforce ?? 'off',
        state.config.validatorScrutiny.sandbox?.extraWrite.length ?? 0,
        state.config.validatorScrutiny.sandbox?.egress.length ?? 0,
        state.config.validatorFunctional.sandbox?.enforce ?? 'off',
        state.config.validatorFunctional.sandbox?.extraWrite.length ?? 0,
        state.config.validatorFunctional.sandbox?.egress.length ?? 0,
      ].join(':')
    : '';

  useEffect(() => {
    setSummary(null);
    setError(null);
    if (missionId === null) return;
    let cancelled = false;
    void api
      .workspace(missionId)
      .then((value) => !cancelled && setSummary(value))
      .catch((err: unknown) => {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err));
      });
    return () => {
      cancelled = true;
    };
  }, [missionId, workspaceKey, preflightSeq]);

  if (state === null) {
    return (
      <section className="panel">
        <div className="section-label">Workspace</div>
        <div className="dim panel-empty">—</div>
      </section>
    );
  }

  return (
    <section className="panel panel-workspace">
      <div className="section-label">Workspace</div>
      {summary !== null ? (
        <div className="workspace-body">
          <div className="workspace-row">
            <span className="dim">mode</span>
            <span className="workspace-value mono">
              {summary.isolation} · {LIFECYCLE_LABEL[summary.lifecycle]}
            </span>
          </div>
          <div className="workspace-row">
            <span className="dim">cwd</span>
            <code className="workspace-value workspace-path" title={summary.cwd}>
              {summary.cwd}
            </code>
          </div>
          <div className="workspace-row">
            <span className="dim">sandbox</span>
            <span className="workspace-value workspace-sandboxes">
              {summary.sandboxes.map((row) => (
                <span key={row.role} className="workspace-chip">
                  {sandboxLabel(row)}
                </span>
              ))}
            </span>
          </div>
          <div className="workspace-row">
            <span className="dim">preflight</span>
            <span
              className={`workspace-value${
                summary.preflight.status === 'issues' ? ' workspace-preflight-issues' : ''
              }`}
            >
              {summary.preflight.summary}
            </span>
          </div>
        </div>
      ) : error !== null ? (
        <div className="panel-error" role="alert">
          workspace details unavailable: {error}
        </div>
      ) : (
        <div className="dim panel-empty">loading…</div>
      )}
    </section>
  );
}
