/* sim.js: a stand-in for `kranz serve`.
 *
 * Emits events in Kranz's own shape (crates/engine/src/events.rs):
 *   { seq, ts, missionId, type, payload }
 * plus a few `board.*` observations for facts Kranz serves over REST rather
 * than writing to events.jsonl: the ticket-to-mission join, the parked plan,
 * and the merge request and its result. The board reads nothing else, so a
 * real source only has to produce the same stream.
 *
 * Discrete-event: runUntil(t) plays everything due up to t, so a seeded
 * backstory and the live feed come from one mechanism.
 */
(function (KB) {
  'use strict';

  function mulberry32(seed) {
    let a = seed >>> 0;
    return function () {
      a = (a + 0x6d2b79f5) | 0;
      let t = Math.imul(a ^ (a >>> 15), 1 | a);
      t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
      return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    };
  }
  function hash32(str) {
    let h = 2166136261;
    for (let i = 0; i < str.length; i++) {
      h ^= str.charCodeAt(i);
      h = Math.imul(h, 16777619);
    }
    return h >>> 0;
  }
  const hex = (n, len) => (n >>> 0).toString(16).padStart(8, '0').slice(0, len);

  // Same shape and defaults as the canned config in scripts/mock-server.mjs.
  const role = (model, effort, turns, budget) => {
    const r = { model, reasoningEffort: effort, maxBudgetUsd: budget };
    if (turns) r.maxTurns = turns;
    return r;
  };
  const CONFIG = {
    orchestrator: role('opus', 'high', 0, 20),
    worker: role('sonnet', 'medium', 50, 10),
    validatorScrutiny: role('opus', 'high', 40, 10),
    validatorFunctional: role('sonnet', 'medium', 40, 5),
    skipScrutiny: false,
    skipFunctional: false,
    maxFixCyclesPerMilestone: 2,
    maxRespawns: 2,
    maxParallelWorkers: 1,
    eventStreamThrottleMs: 250,
    denyPatterns: ['git push'],
    allowValidatorCommands: ['npm test'],
    dangerouslyAllowAll: false,
    allowBelowDefaultWorkerModel: false,
  };

  /* The demo fleet is the build plan for this board: the first eight tickets
   * are the work it would take to ship it in apps/dashboard, the rest are
   * follow-ups. `at` is the draft start in seconds relative to page load
   * (negative = backstory). `op` scripts the operator for the backstory:
   * seconds until the decision, or null to leave the poll for the viewer. */
  const TICKETS = [
    {
      slug: 'board-kit-assets', title: 'Vendor the Space Kit models', at: -420, draft: 12,
      ms: [['Asset pipeline', ['Copy the GLB set into public/kit', 'Licence notice for the CC0 kit']]],
      op: { plan: 21, merge: 34 },
    },
    {
      slug: 'board-fleet-fold', title: 'Fold missions into craft', at: -385, draft: 13,
      ms: [['Fleet model', ['Craft view from the mission fold', 'Stage and hold derivation']], ['Fixtures', ['Golden logs from the mock server']]],
      op: { plan: 9.4, merge: 47 },
      finding: { ms: 0, severity: 'major', subject: 'f-1-2', evidence: 'A blocked milestone folds back to running when a later worker.message arrives.', fix: 'Fix: blocked survives later worker messages' },
    },
    {
      slug: 'board-mission-profile', title: 'Flight-plan geometry', at: -300, draft: 12,
      ms: [['Paths', ['Earth and lunar orbit bands', 'Launch, coast and descent curves']], ['Clocks', ['Burn windows from event timestamps']]],
      op: { plan: 17, grant: 6.8, merge: 22 },
      grant: { ms: 0, kind: 'command', command: 'npx vitest run src/board/profile.test.ts' },
    },
    {
      slug: 'board-scene', title: 'Three.js scene as a lazy route', at: -175, draft: 14,
      ms: [['Route', ['/board route and lazy chunk', 'Stack, Eagle and the kit loader']], ['Bundle', ['Embedded bundle stays in sync']]],
      op: { plan: 26, grant: 31, merge: null },
      grant: { ms: 0, kind: 'egress', command: 'registry.npmjs.org:443' },
    },
    {
      slug: 'board-live-feed', title: 'Fleet feed over REST and WebSocket', at: -95, draft: 13,
      ms: [['Feed', ['Mission list polling', 'Per-mission socket with a since cursor']], ['Hosts', ['Repo-scoped routes']]],
      op: { plan: 14, question: 16, merge: null },
      question: { ms: 0, f: 0, text: 'Poll GET /api/missions every 2 s or every 5 s?', options: ['Every 2 s', 'Every 5 s'], answer: 1 },
    },
    {
      slug: 'board-holds-and-polls', title: 'Red ring and go/no-go polls', at: -78, draft: 13,
      ms: [['Red ring', ['One ring for anything waiting on you', 'The ask on the craft label']], ['Polls', ['Reuse the grant, plan and merge actions']]],
      op: { plan: 19, grant: null, merge: null },
      grant: { ms: 0, kind: 'command', command: 'npm run test:e2e' },
    },
    {
      slug: 'board-replay', title: 'Replay scrubber over the event log', at: -45, draft: 12,
      ms: [['Replay', ['Fold at time T', 'Scrubber with event marks']]],
      op: { plan: 11, merge: null },
      finding: { ms: 0, severity: 'major', subject: 'f-1-2', evidence: 'Scrubbing backwards leaves the flight log ahead of T.', fix: 'Fix: flight log follows T when scrubbing back' },
    },
    {
      slug: 'board-landed-observation', title: 'Record landings for replay', at: -27, draft: 14,
      ms: [['Decision', ['Observation log for merged and plan-parked']], ['Wiring', ['Replay reads the observation log']]],
      op: { plan: null, merge: null },
      grant: { ms: 1, kind: 'command', command: 'cargo test -p kranz-server merged' },
    },
    {
      slug: 'board-slack-link', title: 'Open the craft from a Slack card', at: -6, draft: 15,
      ms: [['Slack', ['Board link on the Slack bridge cards', 'Link opens the board on that craft']]],
      op: { plan: null, merge: null },
      question: { ms: 0, f: 0, text: 'Should the Slack card open the board or the mission page?', options: ['The board', 'The mission page'], answer: null },
      grant: { ms: 0, kind: 'egress', command: 'slack.com:443' },
    },
    {
      slug: 'board-sgian-strip', title: 'One-row board for the Sgian status strip',
      ms: [['Strip', ['Single-lane profile', 'Terminal renderer']], ['Attach', ['Hold opens a read-only attach']]],
      mergeRefused: { gate: 'cargo fmt --all --check', detail: 'Diff in crates/server/src/board.rs' },
    },
    {
      slug: 'board-g2-glance', title: 'Glance view for the G2 glasses',
      ms: [['Glance', ['Fleet summary line', 'Approve a hold from the temple tap']]],
      grant: { ms: 0, kind: 'command', command: 'npm run pack' },
    },
    {
      slug: 'board-free-return', title: 'Free return for failed missions',
      ms: [['Free return', ['Swing behind the Moon on mission.failed', 'Splashdown and recovery']], ['Report', ['Failure note in the craft card']]],
      finding: { ms: 1, severity: 'minor', subject: 'f-2-1', evidence: 'The failure note truncates multi-line reasons.', fix: 'Fix: wrap long failure reasons' },
    },
    {
      slug: 'board-surgeon-overlay', title: 'Grant-latency overlay from the flight surgeon',
      ms: [['Overlay', ['Latency buckets on the hold clock', 'Rubber-stamp marker under ten seconds']]],
    },
    {
      slug: 'board-sound', title: 'Launch and touchdown audio',
      ms: [['Audio', ['Launch rumble', 'Touchdown thud']], ['Controls', ['Mute by default']]],
      fail: { ms: 0, f: 0, summary: 'No audio context in the headless test runner.' },
    },
    {
      slug: 'board-reduced-motion', title: 'Reduced-motion mode',
      ms: [['Motion', ['Static beacons and no flicker', 'Step the craft once a second']]],
    },
    {
      slug: 'board-repo-sites', title: 'One landing site per repo',
      ms: [['Sites', ['Repo-scoped feeds', 'Landing site from the repo catalog']], ['Names', ['Site names on the board']]],
      grant: { ms: 0, kind: 'command', command: 'cargo test -p kranz-server repos' },
    },
  ];

  const MAX_RUNNING = 2; // concurrent runs (a small multi-repo host)
  const MAX_FLIGHT = 5; // craft between launch and merge, one per lane
  const GRANT_TIMEOUT = 90; // an unanswered grant times out to grant.denied
  const DRAFT_GAP = 26; // seconds between live draft starts
  const LANE_LANDING = 45, LANE_FAILED = 47; // seconds a lane stays taken after a merge or a failure (fold.js agrees)

  function createSim(opts) {
    const loadAt = opts.loadAt;
    const events = [];
    const queue = [];
    const seqs = new Map();
    const polls = new Map();
    const missions = [];
    const slotWaiters = [];
    let now = loadAt + Math.min(...TICKETS.map((t) => (t.at == null ? 0 : t.at))) * 1000 - 1000;
    let counter = 0;
    let pollN = 0;
    let running = 0;
    let nextLive = TICKETS.findIndex((t) => t.at == null);
    let iteration = 1;
    let lastDraftAt = -Infinity;

    function at(when, fn) {
      const item = { at: when, n: counter++, fn };
      let lo = 0, hi = queue.length;
      while (lo < hi) {
        const mid = (lo + hi) >> 1;
        if (queue[mid].at < when || (queue[mid].at === when && queue[mid].n < item.n)) lo = mid + 1;
        else hi = mid;
      }
      queue.splice(lo, 0, item);
    }

    function runUntil(t) {
      while (queue.length && queue[0].at <= t) {
        const item = queue.shift();
        if (item.at > now) now = item.at;
        item.fn();
      }
      if (t > now) now = t;
    }

    function emit(missionId, type, payload) {
      let seq = null;
      if (type.indexOf('board.') !== 0) {
        seq = (seqs.get(missionId) || 0) + 1;
        seqs.set(missionId, seq);
      }
      events.push({ seq, ts: new Date(now).toISOString(), t: now, missionId, type, payload });
    }

    function drive(gen) {
      function step(value) {
        const r = gen.next(value);
        if (r.done) return;
        const cmd = r.value;
        if (cmd.wait != null) at(now + cmd.wait * 1000, () => step());
        else if (cmd.poll) openPoll(cmd.poll, step);
        else if (cmd.slot) {
          slotWaiters.push({ m: cmd.slot, resume: step });
          pump();
        }
      }
      step();
    }

    function openPoll(p, resume) {
      const poll = { id: p.missionId + ':' + p.kind + ':' + ++pollN, missionId: p.missionId, kind: p.kind, questionId: p.questionId, askedAt: now, resume };
      polls.set(poll.id, poll);
      if (p.op != null) at(now + p.op * 1000, () => decide(poll.id, p.auto || 'go', { scripted: true }));
      if (p.timeout != null) at(now + p.timeout * 1000, () => decide(poll.id, 'nogo', { timedOut: true }));
      return poll;
    }

    function decide(id, choice, meta) {
      const poll = polls.get(id);
      if (!poll) return false;
      polls.delete(id);
      poll.resume(Object.assign({ choice }, meta || {}));
      return true;
    }

    /* The board's only write path. A real source maps the same call onto
     * Kranz's REST actions (ticket approve, approve-grant, merge, ...). */
    function act(action, when) {
      runUntil(when == null ? Date.now() : when);
      for (const poll of polls.values()) {
        if (poll.missionId !== action.missionId || poll.kind !== action.kind) continue;
        if (action.kind === 'question' && poll.questionId !== action.questionId) continue;
        return decide(poll.id, action.choice, { by: 'viewer' });
      }
      return false;
    }

    // Craft holding a lane: launched and not yet merged, plus the ones still
    // clearing it (a lander on its way down, a failure on its way round the Moon).
    function inFlight() {
      return missions.filter((m) => m.phase === 'run' || m.phase === 'blocked' || m.phase === 'delivered' || (m.laneUntil != null && m.laneUntil > now)).length;
    }
    function holdLane(m, seconds) {
      m.laneUntil = now + seconds * 1000;
      at(m.laneUntil, pump);
    }
    function pump() {
      for (let i = 0; i < slotWaiters.length && running < MAX_RUNNING; ) {
        const w = slotWaiters[i];
        if (w.m.phase === 'blocked' || inFlight() < MAX_FLIGHT) {
          slotWaiters.splice(i, 1);
          running++;
          w.m.phase = 'run';
          w.resume();
        } else i++;
      }
    }
    function endRun() {
      running--;
      at(now, pump);
    }

    function startMission(ticket, iter) {
      const slug = iter > 1 ? ticket.slug + '-iter-' + iter : ticket.slug;
      const seed = hash32(slug);
      const m = { id: 'm-' + hex(seed, 6), slug, ticket, phase: 'draft', rnd: mulberry32(seed) };
      missions.push(m);
      lastDraftAt = now;
      drive(missionProcess(m));
    }

    function liveTick() {
      const drafting = missions.filter((m) => m.phase === 'draft').length;
      const pre = missions.filter((m) => m.phase === 'draft' || m.phase === 'review' || m.phase === 'queued').length;
      if (drafting < 2 && pre < 4 && now - lastDraftAt >= DRAFT_GAP * 1000) {
        if (nextLive < 0 || nextLive >= TICKETS.length) {
          nextLive = 0;
          iteration++;
        }
        startMission(TICKETS[nextLive++], iteration);
      }
      at(now + 1000, liveTick);
    }

    const wait = (seconds) => ({ wait: seconds });

    function* missionProcess(m) {
      const t = m.ticket, id = m.id, rnd = m.rnd;
      const op = t.at != null && t.op ? t.op : {};
      const scripted = (k) => (m.slug === t.slug && k in op ? op[k] : null);
      const E = (type, payload) => emit(id, type, payload);
      const sha = () => hex((rnd() * 0xffffffff) >>> 0, 7);
      const between = (lo, hi) => lo + rnd() * (hi - lo);
      const usage = (scale) => ({ input: Math.round(between(22, 52) * 1000 * scale), output: Math.round(between(4, 10) * 1000 * scale), cacheRead: Math.round(between(90, 190) * 1000 * scale), cacheWrite: Math.round(between(4, 9) * 1000 * scale) });
      const word = () => (rnd() * 0xffffffff) >>> 0;
      const uuid = () => hex(word(), 8) + '-' + hex(word(), 4) + '-4' + hex(word(), 3) + '-b' + hex(word(), 3) + '-' + hex(word(), 8) + hex(word(), 4);
      let runN = 0;
      const spawn = (r, extra) => {
        const runId = 'r-' + ++runN;
        E('worker.spawned', Object.assign({ runId, role: r }, extra, {
          sdkSessionId: uuid(),
          model: r === 'worker' || r === 'validator-functional' ? 'sonnet' : 'opus',
          promptHash: 'sha256:' + hex(word(), 6),
          transcriptPath: 'runs/' + runId + '.jsonl',
        }));
        return runId;
      };
      const done = (runId, result, cost, report) => {
        const p = { runId, result, tokens: usage(cost), costUsd: Math.round(cost * 100) / 100 };
        if (report) p.report = report;
        E('worker.completed', p);
      };

      const nFeat = t.ms.reduce((n, ms) => n + ms[1].length, 0);
      const expected = Math.round((0.95 * nFeat + 0.9 * t.ms.length + 0.7) * 100) / 100;
      const estimate = { workerRuns: nFeat + 0.4, validatorRuns: t.ms.length * 2, lowUsd: Math.round(expected * 55) / 100, expectedUsd: expected, highUsd: Math.round(expected * 220) / 100 };
      const plan = {
        goal: t.title,
        validationContract: [
          { id: 'a-1', statement: 'The dashboard gates pass with the board in the bundle', check: 'command', command: 'npm test' },
          { id: 'a-2', statement: t.title + ' works end to end as the ticket describes', check: 'agent-judgement' },
        ],
        milestones: t.ms.map((ms) => ({ title: ms[0], features: ms[1].map((f) => ({ title: f, spec: f + ', tests first', validationCriteria: [f + ' works as specced'] })) })),
      };

      // --- draft -----------------------------------------------------------
      E('board.ticket', { slug: m.slug, title: t.title, priority: 2 });
      E('mission.created', { goal: t.title, baseBranch: 'main', missionBranch: 'kranz/mission-' + id, config: CONFIG });
      const orch = spawn('orchestrator');
      const draft = t.draft || between(12, 16);
      yield wait(draft * 0.45);
      E('worker.message', { runId: orch, tag: 'text', content: 'Contract first: the gates that must pass before any feature exists.' });
      yield wait(draft * 0.55);

      // --- review: the plan is parked until the operator queues it ---------
      for (;;) {
        m.phase = 'review';
        E('board.plan-parked', { estimate, milestones: plan.milestones.length, features: nFeat });
        const d = yield { poll: { missionId: id, kind: 'plan', op: scripted('plan') } };
        if (d.choice === 'go') break;
        m.phase = 'draft';
        E('board.plan-reshaping', {});
        E('user.message', { text: 'Reshape: tighten the contract before I queue this.', interrupt: false });
        yield wait(8);
        E('worker.message', { runId: orch, tag: 'text', content: 'Narrowed the contract and re-parked the plan.' });
      }
      E('plan.approved', { plan, baseSha: sha() });
      E('orchestrator.decision', { summary: 'Plan approved: ' + plan.milestones.length + ' milestone' + (plan.milestones.length > 1 ? 's' : '') + ', ' + nFeat + ' feature' + (nFeat > 1 ? 's' : '') + '.' });
      m.phase = 'queued';
      yield { slot: m };

      // --- run -------------------------------------------------------------
      let qN = 0;
      for (let mi = 0; mi < plan.milestones.length; mi++) {
        const msId = 'ms-' + (mi + 1);
        E('milestone.started', { milestoneId: msId, startSha: sha() });
        let nextFeat = plan.milestones[mi].features.length;
        let work = plan.milestones[mi].features.map((f, fi) => ({ id: 'f-' + (mi + 1) + '-' + (fi + 1), title: f.title, index: fi }));
        let cycle = 0;
        let openQuestion = null;
        for (;;) {
          for (const f of work) {
            E('feature.started', { featureId: f.id });
            let attempts = 0;
            for (;;) {
              const r = spawn('worker', { featureId: f.id, milestoneId: msId });
              const dur = between(9.5, 13.5);
              yield wait(dur * 0.32);
              E('worker.message', { runId: r, tag: 'text', content: 'Tests first: ' + f.title + '.' });
              if (t.question && cycle === 0 && t.question.ms === mi && t.question.f === f.index && !openQuestion) {
                const q = { questionId: 'q-' + ++qN, role: 'worker', text: t.question.text, options: t.question.options, runId: r, featureId: f.id, milestoneId: msId };
                E('question.opened', q);
                openQuestion = openPoll({ missionId: id, kind: 'question', questionId: q.questionId, op: scripted('question'), auto: t.question.answer }, (d) => {
                  const i = typeof d.choice === 'number' ? d.choice : 0;
                  E('question.answered', { questionId: q.questionId, answer: q.options[i], via: 'answer-question', option: i });
                  openQuestion = null;
                });
                openQuestion.questionId = q.questionId;
              }
              yield wait(dur * 0.43);
              E('worker.message', { runId: r, tag: 'tool-use', content: 'Bash: npm test' });
              yield wait(dur * 0.25);
              if (t.fail && t.fail.ms === mi && t.fail.f === f.index) {
                done(r, 'fail', between(0.5, 0.9), { result: 'fail', summary: t.fail.summary });
                if (++attempts > CONFIG.maxRespawns) {
                  const reason = f.id + ' failed after ' + CONFIG.maxRespawns + ' respawns: ' + t.fail.summary;
                  E('feature.failed', { featureId: f.id, reason: t.fail.summary, commits: [] });
                  done(orch, 'fail', between(0.4, 0.7));
                  E('mission.failed', { reason });
                  m.phase = 'failed';
                  holdLane(m, LANE_FAILED);
                  endRun();
                  return;
                }
                E('orchestrator.decision', { summary: 'Respawning ' + f.id + ' (' + attempts + ' of ' + CONFIG.maxRespawns + ').' });
                continue;
              }
              const commit = sha();
              done(r, 'pass', between(0.55, 1.35), { result: 'pass', summary: f.title + ' implemented with tests.', commits: [commit] });
              E('feature.completed', { featureId: f.id, commits: [commit] });
              break;
            }
          }

          // Validation round: scrutiny and functional, fresh per round.
          E('milestone.validating', { milestoneId: msId });
          const rs = spawn('validator-scrutiny', { milestoneId: msId });
          const rf = spawn('validator-functional', { milestoneId: msId });
          yield wait(3);
          if (t.grant && t.grant.ms === mi && cycle === 0) {
            const g = t.grant;
            E('worker.message', { runId: rf, tag: 'denied', content: (g.kind === 'egress' ? 'Egress: ' : 'Bash: ') + g.command + (g.kind === 'egress' ? ' (not on the egress allowlist)' : ' (outside the validator allow-set)') });
            E('grant.requested', { milestoneId: msId, kind: g.kind, command: g.command });
            const d = yield { poll: { missionId: id, kind: 'grant', op: scripted('grant'), timeout: GRANT_TIMEOUT } };
            if (d.choice === 'go') {
              E('grant.approved', { kind: g.kind, command: g.command });
            } else {
              E('grant.denied', { kind: g.kind, command: g.command, reason: d.timedOut ? 'timed out; deny is the default' : 'operator denied' });
              E('milestone.blocked', { milestoneId: msId, reason: 'grant denied: ' + g.command, blockContext: { owner: 'operator', cause: 'grant' } });
              m.phase = 'blocked';
              endRun(); // a blocked mission releases its run slot
              const b = yield { poll: { missionId: id, kind: 'block', op: scripted('block') } };
              if (b.choice !== 'go') {
                done(orch, 'partial', between(0.4, 0.7));
                E('mission.abandoned', { reason: 'operator abandoned after a denied grant' });
                m.phase = 'abandoned';
                holdLane(m, 3);
                return;
              }
              E('user.message', { text: 'Validate without that command and continue.', interrupt: false });
              E('milestone.unblocked', { milestoneId: msId, reason: 'user guidance received', blockContext: { owner: 'operator', cause: 'grant' } });
              yield { slot: m };
            }
          }
          yield wait(between(3.5, 5));
          done(rf, 'pass', between(0.15, 0.35));
          if (t.finding && t.finding.ms === mi && cycle === 0) {
            const fd = t.finding;
            E('validation.finding', { milestoneId: msId, runId: rs, finding: { subject: fd.subject, severity: fd.severity, evidence: fd.evidence, suggestedFix: fd.fix.replace(/^Fix: /, '') } });
            done(rs, 'fail', between(0.4, 0.9));
            const fix = { id: 'f-' + (mi + 1) + '-' + ++nextFeat, title: fd.fix, index: -1 };
            E('fixfeature.created', { milestoneId: msId, feature: { id: fix.id, title: fix.title, spec: fd.evidence, validationCriteria: [fd.fix.replace(/^Fix: /, '') + ' holds'], origin: 'fix', status: 'pending', workerRuns: [], commits: [], respawns: 0 } });
            E('orchestrator.decision', { summary: 'Filed fix feature ' + fix.id + ' from a ' + fd.severity + ' finding; ' + msId + ' re-validates after it.' });
            work = [fix];
            cycle++;
            continue;
          }
          done(rs, 'pass', between(0.4, 0.9));
          if (openQuestion) {
            polls.delete(openQuestion.id);
            E('question.cleared', { questionId: openQuestion.questionId, why: 'milestone completed' });
            openQuestion = null;
          }
          E('milestone.completed', { milestoneId: msId });
          break;
        }
      }

      // --- final gate, then delivered --------------------------------------
      E('mission.validating', {});
      yield wait(2.2);
      E('gate.result', { gate: 'merge-gate-suite', surface: 'final-gate', kind: 'deterministic', index: 0, verdict: 'pass', artefactRef: '.kranz/merge-gates.json' });
      yield wait(1.8);
      done(orch, 'pass', between(0.45, 0.85));
      E('mission.completed', {});
      m.phase = 'delivered';
      endRun();

      // --- merge is a human act; the result comes back from the POST -------
      let refused = false;
      for (;;) {
        yield { poll: { missionId: id, kind: 'merge', op: scripted('merge') } };
        E('board.merge-requested', {});
        yield wait(4);
        if (t.mergeRefused && !refused) {
          refused = true;
          E('board.merge-refused', { status: 422, gate: t.mergeRefused.gate, detail: t.mergeRefused.detail });
          continue;
        }
        E('board.merged', { commit: sha() });
        break;
      }
      m.phase = 'landed';
      holdLane(m, LANE_LANDING);
    }

    for (const t of TICKETS) if (t.at != null) at(loadAt + t.at * 1000, () => startMission(t, 1));
    at(loadAt + 1000, liveTick);

    return {
      label: 'Simulated feed',
      simulated: true,
      readOnly: false,
      status: { state: 'live', detail: '' },
      grantTimeoutSec: GRANT_TIMEOUT,
      events,
      tick: runUntil,
      act,
    };
  }

  KB.createSim = createSim;
  KB.SIM = { TICKETS, CONFIG, GRANT_TIMEOUT };
})((globalThis.KB = globalThis.KB || {}));
