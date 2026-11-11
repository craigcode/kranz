// Plan review panel (M2.5): shown when request-plan returns ready:true.
// Renders the validation contract, milestones/features with specs and
// done-when criteria, and the cost estimate (wording mirrors the CLI's
// render_cost_estimate verbatim). Approval mirrors the TUI's two consents:
//   1. [Approve & commit plan]  → POST approve (plan.json committed; branch shown)
//   2. [Start execution]        → POST start (spending money is its own consent)
// with a "back to conversation" link on both steps.

import { useKranzStore } from '../lib/store';
import { hasAmbiguous, visible } from '../lib/presentation';
import type { CostEstimate, NegativeControl } from '../lib/types';

/** Verbatim mirror of crates/cli/src/output.rs render_cost_estimate. */
function renderCostEstimate(e: CostEstimate): string {
  return (
    `estimated $${e.lowUsd.toFixed(2)}-$${e.highUsd.toFixed(2)} ` +
    `(expected ~$${e.expectedUsd.toFixed(2)}; rough estimate — live usage is authoritative)`
  );
}

function NegativeControlReview({ assertionId, control }: { assertionId: string; control: NegativeControl }) {
  const groups = [
    { label: 'Checker files', files: control.checkerFiles },
    { label: 'Valid fixture files', files: control.validFiles },
    { label: 'Defective fixture files', files: control.defectiveFiles },
  ];
  return (
    <section className="contract-control" aria-label={`Negative control for ${assertionId}`}>
      <strong>Negative control</strong>
      <p className="dim">Advisory control configuration; results are recorded with gate evidence.</p>
      <p>
        The command above runs against both disposable fixtures: the valid case must pass,
        and the defective case must report the expected failure. Checker files must match their pinned source.
      </p>
      <p>Expected failure: <code>{visible(control.expectedFailure)}</code></p>
      <p className="dim">Timeout: {control.timeoutSeconds ?? 60}s per fixture (maximum 180s).</p>
      {control.baselinePair && (
        <section aria-label={`Baseline and candidate expectations for ${assertionId}`}>
          <strong>Baseline and candidate evidence (advisory)</strong>
          <p>Baseline commit: <code>{visible(control.baselinePair.baselineRevision)}</code></p>
          <p>Expected baseline: <code>{visible(JSON.stringify(control.baselinePair.expectedBaseline))}</code></p>
          <p>Expected candidate: <code>{visible(JSON.stringify(control.baselinePair.expectedCandidate))}</code></p>
          <p>Environment label: {visible(control.baselinePair.environmentLabel)}</p>
          <p>{control.baselinePair.overlayCheckerOnBaseline
            ? 'The exact checker files below may be overlaid onto the baseline snapshot. The candidate must deliver them.'
            : 'Both revisions must already contain the exact checker files below.'}</p>
          <p className="dim">The valid/defective control proof must succeed before the actual revision pair runs. All cases share a 300s budget. Environment configuration is recorded; host tools and external services remain unqualified.</p>
        </section>
      )}
      {groups.map(({ label, files }) => (
        <div className="contract-control-files" key={label}>
          <div>{label}</div>
          {files.map((file) => (
            <details key={file.path}>
              <summary><code>{visible(file.path)}</code> — review contents</summary>
              {file.content === '' ? <p className="dim">Empty file</p> : <pre>{visible(file.content)}</pre>}
            </details>
          ))}
        </div>
      ))}
    </section>
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
  const standards = plan.standardsManifest;
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

        {hasAmbiguous(plan) && (
          <p role="status" className="plan-review-hidden-chars">
            This plan contains hidden or control characters. Each is shown below as a{' '}
            <code>\uXXXX</code> escape where it occurs; check those places before approving.
          </p>
        )}

        <p className="plan-review-goal">{visible(plan.goal)}</p>

        {/* Approval copies these into the mission with no further prompt, so
            they are shown in full, first, and never truncated (audit H2). */}
        {((plan.commandGrants?.length ?? 0) > 0 || (plan.touchSet?.length ?? 0) > 0) && (
          <section aria-label="Authority granted on approval" className="plan-authority">
            <div className="section-label">Authority granted on approval</div>
            <p className="dim">
              Approving this plan grants the following to the mission&apos;s sessions with no
              further prompt.
            </p>
            {(plan.commandGrants?.length ?? 0) > 0 && (
              <>
                <p>
                  Commands ({plan.commandGrants!.length}) — each is a prefix, so <code>x</code>{' '}
                  also allows <code>x &lt;anything&gt;</code>:
                </p>
                <ul className="plan-authority-list">
                  {plan.commandGrants!.map((grant, i) => (
                    <li key={i}>
                      <code className="mono">{visible(grant)}</code>
                    </li>
                  ))}
                </ul>
              </>
            )}
            {(plan.touchSet?.length ?? 0) > 0 && (
              <>
                <p>Write paths ({plan.touchSet!.length}) — glob patterns the mission may change:</p>
                <ul className="plan-authority-list">
                  {plan.touchSet!.map((pattern, i) => (
                    <li key={i}>
                      <code className="mono">{visible(pattern)}</code>
                    </li>
                  ))}
                </ul>
              </>
            )}
          </section>
        )}

        {(plan.touchSet?.length ?? 0) === 0 && (
          <p role="note" className="plan-no-write-paths">
            <strong>Write paths:</strong> none declared. The plan does not limit which files the
            mission may change.
          </p>
        )}

        <section aria-label="Reviewer independence">
          <div className="section-label">Reviewer independence</div>
          <p>
            Scrutiny: {plan.reviewerIndependence?.scrutiny ? 'required' : 'not required'}
            {' · '}
            Functional: {plan.reviewerIndependence?.functional ? 'required' : 'not required'}
          </p>
          {(plan.reviewerIndependence?.scrutiny || plan.reviewerIndependence?.functional) && (
            <p className="dim">
              Required reviewers must use a known model family different from every worker attempt.
              This requirement is pinned by approval.
            </p>
          )}
        </section>

        {plan.consideredAlternatives !== undefined && (
          <section aria-label="Considered alternatives">
            <div className="section-label">Considered alternatives</div>
            <div className="plan-alternatives">
              <p>
                <span className="dim">Chosen:</span> {visible(plan.consideredAlternatives.chosen)}
              </p>
              {plan.consideredAlternatives.rejected.length > 0 && (
                <ul>
                  {plan.consideredAlternatives.rejected.map((rejected, i) => (
                    <li key={i}>
                      <strong>{visible(rejected.approach)}</strong>
                      {' — '}
                      {visible(rejected.tradeOff)}
                    </li>
                  ))}
                </ul>
              )}
            </div>
          </section>
        )}

        {standards !== undefined && (
          <section aria-label="Flight Rules consent" className="standards-consent">
            <div className="section-label">Flight Rules consent</div>
            <div className="standards-pack-line">
              <strong>{visible(standards.packName)}</strong>
              <span>
                {visible(`${standards.source} · ${standards.packDir}/${standards.standardsRoot}`)}
              </span>
              <code className="mono" title={standards.digest}>
                sha256:{standards.digest}
              </code>
            </div>
            {[...new Set(standards.rules.map((rule) => rule.rfc))].map((rfc) => (
              <div className="standards-rfc" key={rfc}>
                <h3>{visible(rfc)}</h3>
                <ul className="standards-rule-list">
                  {standards.rules
                    .filter((rule) => rule.rfc === rfc)
                    .map((rule) => (
                      <li key={`${rule.id}-r${rule.revision}`} className="standards-rule-card">
                        <div className="standards-rule-head">
                          <code className="mono">{visible(rule.id)} r{rule.revision}</code>
                          <span className="standards-state-text">
                            {visible(rule.effectiveStatus)} · {visible(rule.level.toUpperCase())}
                          </span>
                        </div>
                        <p>{visible(rule.statement)}</p>
                        <div className="standards-rule-meta">
                          <span>checker: <code>{visible(rule.checker ?? 'unavailable')}</code></span>
                          <span>waiver: {rule.waivable ? 'permitted by exact human approval' : 'prohibited'}</span>
                          <span>
                            source: {visible(`${standards.packName}/${standards.standardsRoot}`)}
                          </span>
                        </div>
                      </li>
                    ))}
                </ul>
              </div>
            ))}
          </section>
        )}

        <section aria-label="Validation contract">
          <div className="section-label">Validation contract</div>
          <ul className="contract-list">
            {plan.validationContract.map((a) => (
              <li key={a.id} className="contract-row">
                <span className="mono contract-id">[{visible(a.id)}]</span>
                <span className="contract-check dim">({visible(a.check)})</span>
                <div className="contract-statement">
                  {visible(a.statement)}
                  {a.command !== undefined && a.command !== '' && (
                    <>
                      {' — '}
                      <code className="mono">{visible(a.command)}</code>
                    </>
                  )}
                  {a.negativeControl !== undefined && (
                    <NegativeControlReview assertionId={a.id} control={a.negativeControl} />
                  )}
                </div>
              </li>
            ))}
          </ul>
        </section>

        <section aria-label="Milestones">
          <div className="section-label">Milestones</div>
          <ol className="plan-milestones">
            {plan.milestones.map((ms, mi) => (
              <li key={mi} className="plan-milestone">
                <div className="plan-milestone-title">{visible(ms.title)}</div>
                <ol className="plan-features">
                  {ms.features.map((f, fi) => (
                    <li key={fi} className="plan-feature">
                      <div className="plan-feature-title">
                        <span className="mono dim">
                          {mi + 1}.{fi + 1}
                        </span>{' '}
                        {visible(f.title)}
                      </div>
                      <div className="plan-feature-spec">{visible(f.spec)}</div>
                      {f.validationCriteria.length > 0 && (
                        <ul className="plan-donewhen">
                          {f.validationCriteria.map((c, ci) => (
                            <li key={ci}>{visible(c)}</li>
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
            {visible(error)}
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
              plan approved and committed on <code className="mono">{visible(approvedBranch)}</code>
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
