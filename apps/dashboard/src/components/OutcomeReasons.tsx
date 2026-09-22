import type { OutcomeReasonReport } from '../lib/types';

export function OutcomeReasons({ report }: { report?: OutcomeReasonReport }) {
  return (
    <div className="outcomes-section">
      <h3 className="outcomes-heading">Recorded outcome reasons</h3>
      {!report ? <p className="dim">Reason mapping unavailable in this report.</p> : <>
        <p className="dim">{report.selection}</p>
        <p>Mapping v{report.mappingVersion} · {report.from ?? 'all history'} → {report.through ?? 'latest recorded event'}</p>
        {report.missions.length === 0 && <p>No missions in this activity window.</p>}
        {report.unavailableLogs.length > 0 && <p>Logs unavailable; excluded from the window and denominators: {report.unavailableLogs.join(', ')}</p>}
        {report.taskClasses.map((group) => <div key={group.taskClass}>
          <h4>{group.taskClass} — {group.missions} missions</h4>
          <p>{group.mixedMissions} with mixed reasons · {group.unresolvedMissions} with unresolved requests or blocks</p>
          {group.counts.length === 0 ? <p>No classified reasons recorded.</p> : <table className="outcomes-escalation-table">
            <thead><tr><th>Recorded category</th><th>Missions / cohort</th><th>Observations</th></tr></thead>
            <tbody>{group.counts.map((count) => <tr key={count.category}>
              <td>{count.category.replaceAll('-', ' ')}</td>
              <td>{count.missions} / {group.missions}</td>
              <td>{count.observations}</td>
            </tr>)}</tbody>
          </table>}
        </div>)}
        {report.missions.map((mission) => <details key={mission.missionId}>
          <summary>{mission.missionId} · current state: {mission.currentStatus ?? 'unknown'} · {mission.observations.length} recorded observations</summary>
          <p className="dim">Current state and earlier attempts are separate. Resolved means a recorded workflow decision; it does not prove a repair, release or deployment.</p>
          {mission.observations.map((row, index) => <div key={`${row.seq}-${index}`}>
            <p><strong>{row.category.replaceAll('-', ' ')}</strong> · event #{row.seq} · {row.eventType} · {row.state}{row.resolutionSeq !== null ? ` at event #${row.resolutionSeq}` : ''}</p>
            <p>{row.detail}</p>
            <p className="dim">{row.ts}{row.stage ? ` · ${row.stage}` : ''}{row.attemptId ? ` · attempt ${row.attemptId}` : ''}{row.runId ? ` · run ${row.runId}` : ''}{row.permissionRequestId ? ` · permission ${row.permissionRequestId}` : ''}{row.featureId ? ` · feature ${row.featureId}` : ''}{row.milestoneId ? ` · milestone ${row.milestoneId}` : ''}</p>
            {row.actor && <p>Recorded actor: {JSON.stringify(row.actor)}</p>}
            {row.blockContext && <p>Recorded block context: {JSON.stringify(row.blockContext)}</p>}
          </div>)}
        </details>)}
      </>}
    </div>
  );
}
