// Right column: milestone/feature checklist with status icons, fix-cycle
// chips and fix-origin markers. Blocked milestones render red.

import { useKranzStore } from '../lib/store';
import type { FeatureStatus, MilestoneStatus } from '../lib/types';

const MILESTONE_ICON: Record<MilestoneStatus, string> = {
  pending: '○',
  active: '●',
  validating: '◐',
  complete: '✓',
  blocked: '✗',
};

const FEATURE_ICON: Record<FeatureStatus, string> = {
  pending: '○',
  active: '●',
  complete: '✓',
  skipped: '⊘',
  failed: '✗',
};

export function FeaturesPanel() {
  const mission = useKranzStore((s) => s.state?.mission ?? null);

  if (!mission) {
    return (
      <section className="panel">
        <div className="section-label">Features</div>
        <div className="dim panel-empty">—</div>
      </section>
    );
  }

  const features = mission.milestones.flatMap((m) => m.features);
  const done = features.filter((f) => f.status === 'complete').length;

  return (
    <section className="panel panel-features">
      <div className="section-label">
        Features
        <span className="section-count mono">
          {done}/{features.length}
        </span>
      </div>
      <div className="features-scroll">
        {mission.milestones.map((ms) => (
          <div key={ms.id} className="milestone">
            <div className={`milestone-row ms-${ms.status}`}>
              <span className={`st-icon st-${ms.status}`} aria-hidden="true">
                {MILESTONE_ICON[ms.status]}
              </span>
              <span className="milestone-title" title={`${ms.id} · ${ms.status}`}>
                {ms.title}
              </span>
              {ms.fixCycles > 0 && (
                <span className="fixcycles-chip" title={`${ms.fixCycles} fix cycle(s)`}>
                  ↻{ms.fixCycles}
                </span>
              )}
            </div>
            <ul className="feature-list">
              {ms.features.map((f) => (
                <li
                  key={f.id}
                  className={`feature-row ft-${f.status}`}
                  title={`${f.id} · ${f.status}${f.respawns > 0 ? ` · ${f.respawns} respawn(s)` : ''}`}
                >
                  <span className={`st-icon st-${f.status}`} aria-hidden="true">
                    {FEATURE_ICON[f.status]}
                  </span>
                  <span className="feature-title">{f.title}</span>
                  {f.origin === 'fix' && <span className="fix-chip">fix</span>}
                </li>
              ))}
            </ul>
          </div>
        ))}
      </div>
    </section>
  );
}
