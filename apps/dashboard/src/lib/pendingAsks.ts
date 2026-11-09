// Canonical definitions of the six in-flight operator asks on a MissionState.
// Shared between the mission-page panels (GrantRequestPanel, PermissionRequestPanel,
// GateReviewPanel, RevisionPanel, QuestionRequestPanel, StatusStrip) and the
// Trajectory Board fleet model so the two surfaces never disagree.

import type {
  GateEvaluation,
  LivePermission,
  MissionState,
  MissionStatus,
  PendingGrantRequest,
  PendingQuestion,
  PendingRevision,
} from './types';

export type InFlightAskKind =
  | 'permission'
  | 'grant'
  | 'blocked'
  | 'gate'
  | 'revision'
  | 'question';

export type InFlightAsk =
  | { kind: 'permission'; id: string; record: LivePermission; detail: string }
  | { kind: 'grant'; id: string; grant: PendingGrantRequest; detail: string }
  | { kind: 'blocked'; id: string; detail: string }
  | { kind: 'gate'; id: string; record: GateEvaluation; detail: string }
  | { kind: 'revision'; id: string; revision: PendingRevision; detail: string }
  | { kind: 'question'; id: string; question: PendingQuestion; detail: string };

const REVISABLE_STATUSES: ReadonlySet<MissionStatus> = new Set<MissionStatus>([
  'approved',
  'running',
  'paused',
  'blocked',
  'validating',
]);

export function isRevisableStatus(status: MissionStatus | null | undefined): boolean {
  return status != null && REVISABLE_STATUSES.has(status);
}

export function pendingPermissions(
  state: MissionState | null | undefined,
  nowMs: number,
): LivePermission[] {
  const records = state?.permissions;
  if (!records) return [];
  return Object.values(records).filter(
    (r) => !r.resolution && !r.closed && Date.parse(r.request.proposal.deadline) > nowMs,
  );
}

export function pendingGrant(state: MissionState | null | undefined): PendingGrantRequest | null {
  if (!state?.mission || state.pendingGrantRequest === undefined) return null;
  return state.pendingGrantRequest;
}

export function isMissionBlocked(state: MissionState | null | undefined): boolean {
  return state?.mission?.status === 'blocked';
}

export function pendingGateReviews(state: MissionState | null | undefined): GateEvaluation[] {
  const records = state?.gateEvaluations;
  if (!records) return [];
  return Object.values(records)
    .filter((r) => !r.closed && !r.consumed && Boolean(r.resolution && r.resolution.disposition !== 'proceed'))
    .sort((a, b) => a.requestedSeq - b.requestedSeq);
}

export function pendingRevision(state: MissionState | null | undefined): PendingRevision | null {
  if (!state?.mission || !isRevisableStatus(state.mission.status) || state.pendingRevision === undefined) {
    return null;
  }
  return state.pendingRevision;
}

export function pendingQuestions(state: MissionState | null | undefined): PendingQuestion[] {
  if (!state?.mission || !state.pendingQuestions || state.pendingQuestions.length === 0) {
    return [];
  }
  return state.pendingQuestions;
}

/**
 * Returns every pending in-flight ask on `state` in urgency order:
 * permission -> grant -> blocked -> gate -> revision -> question.
 */
export function pendingAsks(state: MissionState | null | undefined, nowMs: number): InFlightAsk[] {
  if (!state) return [];
  const out: InFlightAsk[] = [];

  for (const record of pendingPermissions(state, nowMs)) {
    const { proposal, binding } = record.request;
    out.push({
      kind: 'permission',
      id: `permission:${proposal.id}`,
      record,
      detail: `${binding.runId} · ${JSON.stringify(proposal.action)}`,
    });
  }

  const grant = pendingGrant(state);
  if (grant !== null) {
    out.push({
      kind: 'grant',
      id: 'grant',
      grant,
      detail: `${grant.kind ?? 'command'}: ${grant.command}`,
    });
  }

  if (isMissionBlocked(state)) {
    const blockedMs = state.mission.milestones.find((m) => m.status === 'blocked');
    out.push({
      kind: 'blocked',
      id: 'blocked',
      detail: blockedMs ? `${blockedMs.id} (${blockedMs.title})` : 'milestone blocked',
    });
  }

  for (const record of pendingGateReviews(state)) {
    const params = record.requested.request.params;
    out.push({
      kind: 'gate',
      id: `gate:${params.attemptId}`,
      record,
      detail: `${params.gateId} · ${params.stage}`,
    });
  }

  const rev = pendingRevision(state);
  if (rev !== null) {
    out.push({
      kind: 'revision',
      id: `revision:${rev.revision}`,
      revision: rev,
      detail: `rev ${rev.revision}: ${rev.instructions}`,
    });
  }

  for (const q of pendingQuestions(state)) {
    out.push({
      kind: 'question',
      id: `question:${q.questionId}`,
      question: q,
      detail: q.text,
    });
  }

  return out;
}
