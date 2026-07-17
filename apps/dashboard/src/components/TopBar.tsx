// Top bar: home | mission path + goal | TIME | PROGRESS | USAGE | connection.

import { useKranzStore } from '../lib/store';
import { fmtCost, fmtElapsed, fmtTokens, pausedMs } from '../lib/format';
import { useNow } from '../lib/useNow';
import { repoHash } from '../lib/routes';

// KRANZ wordmark as the way back to the mission list from any mission view.
function HomeButton() {
  return (
    <button
      type="button"
      className="topbar-home mono"
      title="all missions"
      onClick={() => {
        window.location.hash = repoHash();
      }}
    >
      KRANZ
    </button>
  );
}

export function TopBar() {
  const state = useKranzStore((s) => s.state);
  const pauseEvents = useKranzStore((s) => s.pauseEvents);
  const connection = useKranzStore((s) => s.connection);
  const now = useNow(1000);

  if (!state) {
    return (
      <header className="topbar">
        <HomeButton />
        <div className="topbar-mission">
          <span className="dim">Connecting to mission…</span>
        </div>
      </header>
    );
  }

  const { mission, totals, totalCostUsd } = state;
  const features = mission.milestones.flatMap((m) => m.features);
  const done = features.filter((f) => f.status === 'complete').length;
  // Pause spans come from the store's uncapped pauseEvents list — the capped
  // events ring may have evicted older mission.paused/resumed pairs.
  const elapsed = now - Date.parse(mission.createdAt) - pausedMs(pauseEvents, now);

  return (
    <header className="topbar">
      <HomeButton />
      <div className="topbar-mission" title={`${mission.missionBranch} — ${mission.goal}`}>
        <span className="mono topbar-path">{mission.missionBranch}</span>
        <span className="topbar-goal">{mission.goal}</span>
      </div>
      <div className="topbar-stats">
        <div className="stat">
          <span className="stat-label">Time</span>
          <span className="stat-value mono">{fmtElapsed(elapsed)}</span>
        </div>
        <div className="stat">
          <span className="stat-label">Progress</span>
          <span className="stat-value mono">
            {done}/{features.length}
          </span>
        </div>
        <div
          className="stat"
          title={`tokens in ${fmtTokens(totals.input)} · out ${fmtTokens(totals.output)} · cache read ${fmtTokens(totals.cacheRead)}`}
        >
          <span className="stat-label">Usage</span>
          <span className="stat-value mono">{fmtCost(totalCostUsd)}</span>
        </div>
        <div className={`conn conn-${connection}`} title={`connection: ${connection}`}>
          <span className="conn-dot" aria-hidden="true" />
          {connection}
        </div>
      </div>
    </header>
  );
}
