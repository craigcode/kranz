/* fold.js: events up to time T -> the fleet as the board draws it.
 *
 * A reduced mirror of Kranz's reducer (crates/engine/src/reducer.rs): only
 * the fields the board needs, unknown event types ignored. Geometry-free;
 * it also hands out the board's shared places (pads, orbit lanes, landing
 * sites) so two craft never sit in the same one.
 */
(function (KB) {
  'use strict';

  const PADS = 4, LANES = 5, SITES = 12;
  const PAD_CLEAR_MS = 2500; // a pad frees a moment after liftoff
  const FAIL_MS = 47000; // a failed craft keeps its lane until it has been round the Moon
  const LANDING_MS = 45000; // a merged craft keeps its lane until it is down

  function createFold() {
    let S = null;

    function reset() {
      S = {
        T: -Infinity,
        idx: 0,
        missions: new Map(),
        order: [],
        pads: new Array(PADS).fill(null), // { id, freeAt }
        lanes: new Array(LANES).fill(null), // { id, freeAt }
        landings: [], // mission ids in merge order; landing n uses site n % SITES
        decisions: [], // operator decisions with latency, oldest first
        version: 0,
      };
    }
    reset();

    function mission(id) {
      let m = S.missions.get(id);
      if (!m) {
        m = {
          id, slug: id, title: '', goal: '', status: 'planning',
          createdAt: null, parkedAt: null, approvedAt: null, startedAt: null, finalGateAt: null,
          completedAt: null, mergeRequestedAt: null, mergedAt: null, failedAt: null, abandonedAt: null, assembledAt: null,
          pad: null, padAt: null, lane: null, site: null, landing: null,
          milestones: [], planShape: null, estimate: null,
          runs: {}, activeRuns: [], costUsd: 0,
          pendingGrant: null, blockedAt: null, blockReason: null, questions: [],
          mergeRefused: null, failReason: null, prevStatus: null,
          lastMessage: null, lastDecision: null, findings: 0, gates: [],
        };
        S.missions.set(id, m);
        S.order.push(id);
      }
      return m;
    }

    const free = (slot, t) => slot == null || (slot.freeAt != null && t >= slot.freeAt);
    function firstFree(list, t) {
      for (let i = 0; i < list.length; i++) if (free(list[i], t)) return i;
      return -1;
    }

    // A mission stands on a pad from its first draft until liftoff.
    function settleGround(t) {
      for (const id of S.order) {
        const m = S.missions.get(id);
        if (m.pad != null || m.startedAt != null || m.createdAt == null) continue;
        if (m.status === 'failed' || m.status === 'abandoned') continue;
        const k = firstFree(S.pads, t);
        if (k < 0) break;
        S.pads[k] = { id, freeAt: null };
        m.pad = k;
        m.padAt = t;
      }
    }
    function leaveGround(m, t, linger) {
      if (m.pad != null && S.pads[m.pad] && S.pads[m.pad].id === m.id) S.pads[m.pad].freeAt = t + linger;
    }
    function leaveLane(m, t) {
      if (m.lane != null && S.lanes[m.lane] && S.lanes[m.lane].id === m.id) S.lanes[m.lane].freeAt = t;
    }

    function milestoneOf(m, id) {
      return m.milestones.find((ms) => ms.id === id);
    }
    function featureOf(m, id) {
      for (const ms of m.milestones) {
        const f = ms.features.find((x) => x.id === id);
        if (f) return f;
      }
      return null;
    }
    function decided(m, kind, askedAt, t, decision, detail) {
      S.decisions.push({ missionId: m.id, slug: m.slug, kind, askedAt, at: t, latencyMs: askedAt == null ? null : t - askedAt, decision, detail: detail || '' });
    }

    function apply(e) {
      const t = e.t, p = e.payload || {};
      const m = mission(e.missionId);
      switch (e.type) {
        case 'board.ticket':
          m.slug = p.slug;
          m.title = p.title;
          break;
        case 'mission.created':
          m.goal = p.goal;
          if (!m.title) m.title = p.goal;
          m.createdAt = t;
          m.status = 'planning';
          m.missionBranch = p.missionBranch;
          m.baseBranch = p.baseBranch;
          break;
        case 'board.plan-parked':
          m.parkedAt = t;
          if (m.assembledAt == null) m.assembledAt = t;
          m.estimate = p.estimate || null;
          m.planShape = { milestones: p.milestones, features: p.features };
          break;
        case 'board.plan-reshaping':
          // The operator sent the plan back: the ticket is drafting again.
          decided(m, 'plan', m.parkedAt, t, 'nogo', 'reshape');
          m.parkedAt = null;
          break;
        case 'plan.approved': {
          const plan = p.plan || { milestones: [] };
          decided(m, 'plan', m.parkedAt, t, 'go', 'plan approved');
          if (m.parkedAt == null) m.parkedAt = t;
          if (m.assembledAt == null) m.assembledAt = t;
          m.approvedAt = t;
          m.status = 'approved';
          m.planShape = { milestones: plan.milestones.length, features: plan.milestones.reduce((n, ms) => n + ms.features.length, 0) };
          m.milestones = plan.milestones.map((ms, mi) => ({
            id: 'ms-' + (mi + 1), title: ms.title, status: 'pending', fixCycles: 0, blockReason: null,
            features: ms.features.map((f, fi) => ({ id: 'f-' + (mi + 1) + '-' + (fi + 1), title: f.title, origin: 'plan', status: 'pending' })),
          }));
          break;
        }
        case 'milestone.started': {
          const ms = milestoneOf(m, p.milestoneId);
          if (ms) ms.status = 'active';
          if (m.startedAt == null) {
            m.startedAt = t;
            if (m.pad == null) {
              // Never seen on a pad (the board joined late): lift off from the first one.
              m.pad = 0;
              m.padAt = t;
            }
            leaveGround(m, t, PAD_CLEAR_MS);
            let k = firstFree(S.lanes, t);
            if (k < 0) k = S.order.indexOf(m.id) % LANES;
            S.lanes[k] = { id: m.id, freeAt: null };
            m.lane = k;
          }
          if (m.status === 'approved' || m.status === 'planning') m.status = 'running';
          break;
        }
        case 'feature.started': {
          let f = featureOf(m, p.featureId);
          if (!f) {
            // A feature the plan here does not know (a revised plan): file it
            // under the milestone its id names, or the one that is active.
            const ms = m.milestones.find((x) => p.featureId.indexOf('f-' + x.id.slice(3) + '-') === 0) || m.milestones.find((x) => x.status === 'active');
            if (ms) ms.features.push((f = { id: p.featureId, title: p.featureId, origin: 'plan', status: 'pending' }));
          }
          if (f) f.status = 'active';
          const ms = m.milestones.find((x) => x.features.indexOf(f) >= 0);
          if (ms && ms.status === 'validating') ms.status = 'active';
          break;
        }
        case 'worker.spawned':
          m.runs[p.runId] = { id: p.runId, role: p.role, featureId: p.featureId, milestoneId: p.milestoneId, model: p.model, startedAt: t, endedAt: null, result: null };
          m.activeRuns.push(p.runId);
          break;
        case 'worker.message':
          m.lastMessage = { runId: p.runId, tag: p.tag, content: p.content, t };
          break;
        case 'worker.completed': {
          const r = m.runs[p.runId];
          if (r) {
            r.endedAt = t;
            r.result = p.result;
          }
          m.activeRuns = m.activeRuns.filter((x) => x !== p.runId);
          if (typeof p.costUsd === 'number') m.costUsd += p.costUsd;
          break;
        }
        case 'feature.completed': {
          const f = featureOf(m, p.featureId);
          if (f) f.status = 'complete';
          break;
        }
        case 'feature.failed': {
          const f = featureOf(m, p.featureId);
          if (f) f.status = 'failed';
          break;
        }
        case 'feature.skipped': {
          const f = featureOf(m, p.featureId);
          if (f) f.status = 'skipped';
          break;
        }
        case 'milestone.validating': {
          const ms = milestoneOf(m, p.milestoneId);
          if (ms) ms.status = 'validating';
          break;
        }
        case 'validation.finding':
          m.findings++;
          break;
        case 'fixfeature.created': {
          const ms = milestoneOf(m, p.milestoneId);
          if (ms && p.feature) {
            ms.features.push({ id: p.feature.id, title: p.feature.title, origin: 'fix', status: 'pending' });
            ms.fixCycles++;
            ms.status = 'active';
          }
          break;
        }
        case 'grant.requested':
          m.pendingGrant = { milestoneId: p.milestoneId, kind: p.kind || 'command', command: p.command, askedAt: t };
          break;
        case 'grant.approved':
        case 'grant.denied':
          decided(m, 'grant', m.pendingGrant ? m.pendingGrant.askedAt : null, t, e.type === 'grant.approved' ? 'go' : 'nogo', (p.kind || 'command') + ' ' + p.command + (p.reason ? ' (' + p.reason + ')' : ''));
          m.pendingGrant = null;
          break;
        case 'milestone.blocked': {
          const ms = milestoneOf(m, p.milestoneId);
          if (ms) {
            ms.prevStatus = ms.status;
            ms.status = 'blocked';
            ms.blockReason = p.reason;
          }
          if (m.status !== 'blocked') m.prevStatus = m.status;
          m.status = 'blocked';
          m.blockedAt = t;
          m.blockReason = p.reason;
          m.blockedMilestone = p.milestoneId;
          break;
        }
        case 'milestone.unblocked': {
          const ms = milestoneOf(m, p.milestoneId);
          if (ms) {
            ms.status = ms.prevStatus && ms.prevStatus !== 'blocked' ? ms.prevStatus : 'active';
            ms.blockReason = null;
          }
          decided(m, 'block', m.blockedAt, t, 'go', p.reason);
          if (!m.milestones.some((x) => x.status === 'blocked')) {
            m.status = 'running';
            m.blockedAt = null;
            m.blockReason = null;
          }
          break;
        }
        case 'milestone.completed': {
          const ms = milestoneOf(m, p.milestoneId);
          if (ms) ms.status = 'complete';
          break;
        }
        case 'question.opened':
          m.questions.push({ questionId: p.questionId, text: p.text, options: p.options || [], askedAt: t, featureId: p.featureId, milestoneId: p.milestoneId });
          break;
        case 'question.answered': {
          const q = m.questions.find((x) => x.questionId === p.questionId);
          if (q) decided(m, 'question', q.askedAt, t, 'answered', p.answer);
          m.questions = m.questions.filter((x) => x.questionId !== p.questionId);
          break;
        }
        case 'question.cleared':
          m.questions = m.questions.filter((x) => x.questionId !== p.questionId);
          break;
        case 'mission.validating':
          m.status = 'validating';
          m.finalGateAt = t;
          break;
        case 'gate.result':
          m.gates.push({ gate: p.gate, verdict: p.verdict });
          break;
        case 'mission.paused':
          m.prevStatus = m.status;
          m.status = 'paused';
          break;
        case 'mission.resumed':
          m.status = m.prevStatus || 'running';
          break;
        case 'mission.completed':
          m.status = 'complete';
          m.completedAt = t;
          m.activeRuns = [];
          break;
        case 'mission.failed':
        case 'mission.abandoned': {
          const failed = e.type === 'mission.failed';
          m.status = failed ? 'failed' : 'abandoned';
          if (failed) m.failedAt = t;
          else m.abandonedAt = t;
          m.failReason = p.reason;
          m.activeRuns = [];
          m.pendingGrant = null;
          m.questions = [];
          leaveGround(m, t, 0);
          if (m.lane != null && S.lanes[m.lane] && S.lanes[m.lane].id === m.id) S.lanes[m.lane].freeAt = t + (failed ? FAIL_MS : 3000);
          break;
        }
        case 'board.merge-requested':
          decided(m, 'merge', m.mergeRefused ? m.mergeRefused.at : m.completedAt, t, 'go', 'merge ' + (m.missionBranch || ''));
          m.mergeRequestedAt = t;
          m.mergeRefused = null;
          break;
        case 'board.merge-refused':
          m.mergeRequestedAt = null;
          m.mergeRefused = { at: t, gate: p.gate, detail: p.detail, status: p.status };
          break;
        case 'board.merged': {
          m.mergedAt = t;
          m.landing = S.landings.length;
          S.landings.push(m.id);
          leaveLane(m, t + LANDING_MS);
          m.site = m.landing % SITES;
          break;
        }
        case 'user.message':
        case 'orchestrator.decision':
          m.lastDecision = { text: p.summary || p.text, t, by: e.type === 'user.message' ? 'you' : 'flight' };
          break;
        default:
          break;
      }
      settleGround(t);
    }

    /* Advance to T. Going forward continues from where it stopped; going
     * back refolds from the start, since the log is the source of truth. */
    function at(events, T) {
      if (T < S.T) reset();
      let changed = false;
      while (S.idx < events.length && events[S.idx].t <= T) {
        apply(events[S.idx++]);
        changed = true;
      }
      if (changed) S.version++;
      S.T = T;
      return S;
    }

    return { at, reset };
  }

  /* One of Kranz's pipeline stages (apps/dashboard/src/lib/pipelineStage.ts),
   * with Drafting and Reviewable split on whether a plan is parked. */
  function stageOf(m) {
    if (m.status === 'failed') return 'failed';
    if (m.status === 'abandoned') return 'abandoned';
    if (m.status === 'complete') return m.mergedAt != null ? 'landed' : 'delivered';
    if (m.status === 'running' || m.status === 'paused' || m.status === 'blocked' || m.status === 'validating') return 'running';
    if (m.status === 'approved') return 'queued';
    return m.parkedAt != null ? 'reviewable' : 'drafting';
  }

  /* What is waiting on the operator, oldest first: Kranz's "your move". */
  function pendingOf(S) {
    const out = [];
    for (const id of S.order) {
      const m = S.missions.get(id);
      const stage = stageOf(m);
      if (stage === 'reviewable') out.push({ id: id + ':plan', kind: 'plan', missionId: id, askedAt: m.parkedAt, parks: true });
      if (m.pendingGrant) out.push({ id: id + ':grant', kind: 'grant', missionId: id, askedAt: m.pendingGrant.askedAt, parks: true });
      if (m.status === 'blocked') out.push({ id: id + ':block', kind: 'block', missionId: id, askedAt: m.blockedAt, parks: true });
      if (stage === 'delivered' && m.mergeRequestedAt == null) out.push({ id: id + ':merge', kind: 'merge', missionId: id, askedAt: m.mergeRefused ? m.mergeRefused.at : m.completedAt, parks: false });
      for (const q of m.questions) out.push({ id: id + ':question:' + q.questionId, kind: 'question', missionId: id, questionId: q.questionId, askedAt: q.askedAt, parks: false });
    }
    out.sort((a, b) => a.askedAt - b.askedAt);
    return out;
  }

  function progressOf(m) {
    let done = 0, total = 0;
    for (const ms of m.milestones) for (const f of ms.features) {
      total++;
      if (f.status === 'complete') done++;
    }
    if (!total && m.planShape) total = m.planShape.features;
    return { done, total };
  }

  /* Kranz's four grant-latency buckets (crates/engine/src/outcomes.rs). */
  function latencyBucket(ms) {
    if (ms < 10000) return 0;
    if (ms < 60000) return 1;
    if (ms < 600000) return 2;
    return 3;
  }

  KB.createFold = createFold;
  KB.stageOf = stageOf;
  KB.pendingOf = pendingOf;
  KB.progressOf = progressOf;
  KB.latencyBucket = latencyBucket;
  KB.LATENCY_LABELS = ['<10s', '<60s', '<10m', '>=10m'];
  KB.FOLD = { PADS, LANES, SITES };
})((globalThis.KB = globalThis.KB || {}));
