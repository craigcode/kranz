// Plan review panel (M2.5): shown when request-plan returns ready:true.
// Renders the validation contract, milestones/features with specs and
// done-when criteria, and the cost estimate (wording mirrors the CLI's
// render_cost_estimate verbatim). Approval mirrors the TUI's two consents:
//   1. [Approve & commit plan]  → POST approve (plan.json committed; branch shown)
//   2. [Start execution]        → POST start (spending money is its own consent)
// with a "back to conversation" link on both steps.

import { useKranzStore } from '../lib/store';
import type { CostEstimate } from '../lib/types';

/** Verbatim mirror of crates/cli/src/output.rs render_cost_estimate. */
function renderCostEstimate(e: CostEstimate): string {
  return (
    `estimated $${e.lowUsd.toFixed(2)}-$${e.highUsd.toFixed(2)} ` +
    `(expected ~$${e.expectedUsd.toFixed(2)}; rough estimate — live usage is authoritative)`
  );
}

export function PlanReview() {
  const review = useKranzStore((s) => s.planning.review);
  const approvedBranch = useKranzStore((s) => s.planning.approvedBranch);
  const approving = useKranzStore((s) => s.planning.approving);
  const starting = useKranzStore((s) => s.planning.starting);
  const error = useKranzStore((s) => s.planning.error);
  const approvePlan = useKranzStore((s) => s.approvePlan);
  const startMission = useKranzStore((s) => s.startMission);
  const planningBack = useKranzStore((s) => s.planningBack);

  if (review === null) return null;
  const { plan, estimate } = review;
  const featureCount = plan.milestones.reduce((n, m) => n + m.features.length, 0);

  return (
    <div className="plan-review">
      <div className="plan-review-scroll">
        <header className="plan-review-head">
          <h2 className="plan-review-title">Plan review</h2>
          <span className="dim">
            {plan.milestones.length} milestone(s) · {featureCount} feature(s)
          </span>
        </header>

        <p className="plan-review-goal">{plan.goal}</p>

        {plan.consideredAlternatives !== undefined && (
          <section aria-label="Considered alternatives">
            <div className="section-label">Considered alternatives</div>
            <div className="plan-alternatives">
              <p>
                <span className="dim">Chosen:</span> {plan.consideredAlternatives.chosen}
              </p>
              {plan.consideredAlternatives.rejected.length > 0 && (
                <ul>
                  {plan.consideredAlternatives.rejected.map((rejected, i) => (
                    <li key={i}>
                      <strong>{rejected.approach}</strong>
                      {' — '}
                      {rejected.tradeOff}
                    </li>
                  ))}
                </ul>
              )}
            </div>
          </section>
        )}

        <section aria-label="Validation contract">
          <div className="section-label">Validation contract</div>
          <ul className="contract-list">
            {plan.validationContract.map((a) => (
              <li key={a.id} className="contract-row">
                <span className="mono contract-id">[{a.id}]</span>
                <span className="contract-check dim">({a.check})</span>
                <span className="contract-statement">
                  {a.statement}
                  {a.command !== undefined && a.command !== '' && (
                    <>
                      {' — '}
                      <code className="mono">{a.command}</code>
                    </>
                  )}
                </span>
              </li>
            ))}
          </ul>
        </section>

        <section aria-label="Milestones">
          <div className="section-label">Milestones</div>
          <ol className="plan-milestones">
            {plan.milestones.map((ms, mi) => (
              <li key={mi} className="plan-milestone">
                <div className="plan-milestone-title">{ms.title}</div>
                <ol className="plan-features">
                  {ms.features.map((f, fi) => (
                    <li key={fi} className="plan-feature">
                      <div className="plan-feature-title">
                        <span className="mono dim">
                          {mi + 1}.{fi + 1}
                        </span>{' '}
                        {f.title}
                      </div>
                      <div className="plan-feature-spec">{f.spec}</div>
                      {f.validationCriteria.length > 0 && (
                        <ul className="plan-donewhen">
                          {f.validationCriteria.map((c, ci) => (
                            <li key={ci}>{c}</li>
                          ))}
                        </ul>
                      )}
                    </li>
                  ))}
                </ol>
              </li>
            ))}
          </ol>
        </section>

        <section aria-label="Cost estimate">
          <div className="section-label">Cost</div>
          <p className="plan-estimate mono">{renderCostEstimate(estimate)}</p>
        </section>
      </div>

      <footer className="plan-review-consent">
        {error !== null && (
          <div className="composer-error" role="alert">
            {error}
          </div>
        )}
        {approvedBranch === null ? (
          <div className="consent-row">
            <button
              type="button"
              className="composer-send"
              disabled={approving}
              onClick={approvePlan}
            >
              {approving ? 'Approving…' : 'Approve & commit plan'}
            </button>
            <button type="button" className="link-btn" onClick={planningBack}>
              back to conversation
            </button>
          </div>
        ) : (
          <div className="consent-row">
            <span className="consent-committed">
              plan approved and committed on <code className="mono">{approvedBranch}</code>
            </span>
            <button
              type="button"
              className="composer-send"
              disabled={starting}
              onClick={startMission}
            >
              {starting ? 'Starting…' : 'Start execution'}
            </button>
            <button type="button" className="link-btn" onClick={planningBack}>
              back to conversation
            </button>
          </div>
        )}
      </footer>
    </div>
  );
}
