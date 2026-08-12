// Right column: human-readable feed of the whole event stream, newest first.
// Every event type has a one-line renderer; worker.message tool-results are
// skipped to reduce noise. Left border color encodes the event category.

import { useMemo } from 'react';
import { useKranzStore } from '../lib/store';
import { relTime, roleLabel, truncate } from '../lib/format';
import { useNow } from '../lib/useNow';
import type { MissionEvent, MissionState } from '../lib/types';

type Tone = 'ok' | 'bad' | 'warn' | 'info' | 'accent' | 'plain';

function describe(e: MissionEvent, state: MissionState | null): { text: string; tone: Tone } {
  switch (e.type) {
    case 'mission.created':
      return { text: `Mission created — ${truncate(e.payload.goal, 80)}`, tone: 'accent' };
    case 'plan.approved':
      return {
        text: `Plan approved (${e.payload.plan.milestones.length} milestones)`,
        tone: 'ok',
      };
    case 'plan.revision.proposed':
      return {
        text: `Revision ${e.payload.revision} proposed (${e.payload.plan.milestones.length} milestones)`,
        tone: 'warn',
      };
    case 'plan.revised':
      return { text: `Revision ${e.payload.revision} approved`, tone: 'ok' };
    case 'plan.revision.rejected':
      return { text: `Revision ${e.payload.revision} rejected`, tone: 'warn' };
    case 'grant.requested':
      return {
        text: `Grant requested: ${truncate(e.payload.command, 80)} — awaiting approval`,
        tone: 'warn',
      };
    case 'grant.approved':
      return { text: `Grant approved: ${truncate(e.payload.command, 80)}`, tone: 'ok' };
    case 'grant.denied':
      return { text: `Grant denied: ${truncate(e.payload.command, 80)}`, tone: 'bad' };
    case 'question.opened':
      return {
        text: `Question ${e.payload.questionId}: ${truncate(e.payload.text, 80)} — awaiting an answer`,
        tone: 'warn',
      };
    case 'question.answered':
      return {
        text: `Question ${e.payload.questionId} answered: ${truncate(e.payload.answer, 80)}`,
        tone: 'ok',
      };
    case 'question.cleared':
      return {
        text: `Question ${e.payload.questionId} cleared (${e.payload.why})`,
        tone: 'plain',
      };
    case 'milestone.started':
      return { text: `Milestone ${e.payload.milestoneId} started`, tone: 'info' };
    case 'feature.started':
      return { text: `Feature ${e.payload.featureId} started`, tone: 'info' };
    case 'worker.spawned': {
      const target = e.payload.featureId ?? e.payload.milestoneId;
      return {
        text: `${roleLabel(e.payload.role)} ${e.payload.runId} spawned${target !== undefined ? ` on ${target}` : ''}`,
        tone: 'info',
      };
    }
    case 'worker.message': {
      const p = e.payload;
      if (p.tag === 'denied') return { text: `DENIED: ${truncate(p.content, 90)}`, tone: 'bad' };
      if (p.tag === 'tool-use') return { text: `${p.runId} → ${truncate(p.content, 90)}`, tone: 'plain' };
      return { text: `${p.runId}: ${truncate(p.content, 90)}`, tone: 'plain' };
    }
    case 'worker.completed': {
      const run = state?.runs[e.payload.runId];
      const who = run ? roleLabel(run.role) : 'Run';
      const target = run?.featureId ?? run?.milestoneId;
      return {
        text: `${who} ${e.payload.runId} completed${target !== undefined ? ` ${target}` : ''} (${e.payload.result})`,
        tone: e.payload.result === 'pass' ? 'ok' : e.payload.result === 'fail' ? 'bad' : 'warn',
      };
    }
    case 'feature.completed':
      return { text: `Feature ${e.payload.featureId} completed`, tone: 'ok' };
    case 'feature.failed':
      return {
        text: `Feature ${e.payload.featureId} failed: ${truncate(e.payload.reason, 70)}`,
        tone: 'bad',
      };
    case 'feature.skipped':
      return {
        text: `Feature ${e.payload.featureId} skipped: ${truncate(e.payload.reason, 70)}`,
        tone: 'warn',
      };
    case 'milestone.validating':
      return { text: `Milestone ${e.payload.milestoneId} validating`, tone: 'info' };
    case 'validation.finding':
      return {
        text: `Finding [${e.payload.finding.severity}]: ${truncate(e.payload.finding.evidence, 80)}`,
        tone: 'warn',
      };
    case 'gate.result':
      return {
        text: `Gate ${e.payload.gate}: ${e.payload.verdict}${(e.payload.ruleIds ?? []).length > 0 ? ` (${e.payload.ruleIds?.join(', ')})` : ''}`,
        tone: e.payload.verdict === 'pass' ? 'ok' : 'bad',
      };
    case 'standards.resolved':
      return {
        text: `Flight Rules resolved: ${e.payload.rules.length} rule(s), ${e.payload.digest.slice(0, 10)}…`,
        tone: 'info',
      };
    case 'standards.drifted':
      return {
        text: `Flight Rules drift refused merge: ${truncate(e.payload.changedRules.join('; '), 76)}`,
        tone: 'bad',
      };
    case 'standards.waiver.approved':
      return {
        text: `Standards waiver approved: ${e.payload.ruleId} r${e.payload.ruleRevision}`,
        tone: 'warn',
      };
    case 'standards.attestation.approved':
      return {
        text: `Standards attestation approved: ${e.payload.ruleId} r${e.payload.ruleRevision}`,
        tone: 'ok',
      };
    case 'validator.tamper': {
      const p = e.payload;
      const what =
        p.headBefore !== p.headAfter
          ? `HEAD moved, ${p.appeared.length + p.resolved.length} path(s) changed`
          : truncate(p.appeared.join(', ') || 'checkout altered', 70);
      return { text: `VALIDATOR TAMPER on ${p.milestoneId}: ${what} — round failed`, tone: 'bad' };
    }
    case 'fixfeature.created':
      return {
        text: `Fix feature ${e.payload.feature.id}: ${truncate(e.payload.feature.title, 70)}`,
        tone: 'warn',
      };
    case 'milestone.blocked':
      return {
        text: `Milestone ${e.payload.milestoneId} BLOCKED: ${truncate(e.payload.reason, 80)}`,
        tone: 'bad',
      };
    case 'milestone.unblocked':
      return {
        text: `Milestone ${e.payload.milestoneId} unblocked (${truncate(e.payload.reason, 60)})`,
        tone: 'ok',
      };
    case 'milestone.completed':
      return { text: `Milestone ${e.payload.milestoneId} completed`, tone: 'ok' };
    case 'mission.validating':
      return { text: 'Mission validation started (contract gate)', tone: 'info' };
    case 'mission.paused':
      return { text: 'Mission paused', tone: 'warn' };
    case 'mission.resumed':
      return { text: 'Mission resumed', tone: 'ok' };
    case 'user.message':
      return { text: `You: ${truncate(e.payload.text, 90)}`, tone: 'accent' };
    case 'orchestrator.decision':
      return { text: `Decision: ${truncate(e.payload.summary, 90)}`, tone: 'accent' };
    case 'config.changed':
      return { text: 'Config changed (applies to next spawn)', tone: 'info' };
    case 'mission.completed':
      return { text: 'Mission completed', tone: 'ok' };
    case 'mission.failed':
      return { text: `Mission failed: ${truncate(e.payload.reason, 80)}`, tone: 'bad' };
  }
}

export function ProgressLog() {
  const events = useKranzStore((s) => s.events);
  const state = useKranzStore((s) => s.state);
  const now = useNow(15_000);

  const rows = useMemo(() => {
    const out: { seq: number; ts: string; text: string; tone: Tone }[] = [];
    for (let i = events.length - 1; i >= 0; i--) {
      const e = events[i];
      if (e.type === 'worker.message' && e.payload.tag === 'tool-result') continue;
      const { text, tone } = describe(e, state);
      out.push({ seq: e.seq, ts: e.ts, text, tone });
    }
    return out;
  }, [events, state]);

  return (
    <section className="panel panel-log">
      <div className="section-label">Progress log</div>
      <div className="log-scroll">
        {rows.length === 0 && <div className="dim panel-empty">No events yet</div>}
        <ul className="log-list">
          {rows.map((row) => (
            <li key={row.seq} className={`log-row tone-${row.tone}`}>
              <span className="log-text">{row.text}</span>
              <span className="log-time dim mono">{relTime(row.ts, now)}</span>
            </li>
          ))}
        </ul>
      </div>
    </section>
  );
}
